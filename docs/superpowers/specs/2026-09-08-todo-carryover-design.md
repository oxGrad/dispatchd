# Todo carry-over — design

**Date:** 2026-09-08
**Status:** Approved, ready for implementation plan

## Summary

An unfinished todo from a recent past day stays visible instead of
vanishing at midnight. A `type='todo'` row from the last N days (default
7, configurable) that has **no `done` progress report** against it is
"still open" and is surfaced in:

- **`/progress add`** autocomplete — carried-over todos are selectable
  alongside today's open todos, so you file progress against the original
  todo (and its `todo_id` link) rather than re-typing it as unplanned
  work.
- **`/team status`** — a `+N carried` marker per member.
- **`/team report`** — a `Carried over:` block per member, with each
  todo's latest status and whether it was touched today.
- **`/todo list`** — a read-only `Carried over (still open)` block so the
  member sees what they're still on the hook for.

No schema change. Carry-over is a **derived query** over existing
`entries` rows — todo rows keep meaning "the day you planned it", and the
retained history the biweekly recap reads is untouched.

Config: `[carryover] lookback_days` (default 7, `0` disables the feature
entirely).

## Motivation

- Multi-day work is normal. Today a todo that isn't finished on day 1 is
  invisible on day 2 unless the member re-types it as a fresh todo (which
  loses the link to the original and its SOW ref) or the tech lead
  remembers to chase it.
- `/progress add`'s autocomplete only offers *today's* open todos, so
  reporting continued progress on yesterday's task means free-typing it —
  it then shows up as "unplanned work" in `/team report`, which is wrong.
- The tech lead has no at-a-glance signal for "this person has stale
  open work".

## Approach

**Query-time union, no schema change** (chosen over a morning
roll-forward that clones unfinished todos into fresh today-dated rows —
that needs a migration, an idempotent ticker step, and leaves duplicate
rows the recap has to reconcile forever).

### The carry-over rule

A todo carries over for member *U* on day *D* when **all** of:

- it is a `type='todo'` row owned by *U*
- its `date` is in `[D - lookback_days, D - 1]` — SQL
  `date >= date(D, '-<lookback_days> days') AND date < D` — i.e. the
  `lookback_days` calendar days immediately before today (for the
  default 7: `D-7 … D-1` inclusive)
- **no** `entries` row exists with `type='update'`, that `todo_id`, and
  `status='done'`

It drops off carry-over as soon as a `done` report is filed against it,
or when it ages past the window.

When `lookback_days == 0` every helper below short-circuits to
empty/zero and every surface behaves exactly as it does today.

### New `entries.rs` functions

All pure DB logic, no `serenity` types, unit-tested against a real
tempfile DB (per the project's testing conventions).

| Function | Returns | Consumer |
|---|---|---|
| `carryover_todos(conn, user, today, lookback_days, partial)` | `Vec<CarryoverTodo { id, task, sow_ref, origin_date }>`, ordered by `date` then `id`, capped at 25 | `/progress add` autocomplete (`partial` = typed text), `/todo list` (`partial` = `""`) |
| `carryover_report(conn, user, today, lookback_days)` | `Vec<CarryoverDetail { task, sow_ref, origin_date, latest_status, latest_progress, latest_blocker, updated_today }>` | `/team report` |
| `carryover_count(conn, user, today, lookback_days)` | `i64` | `/team status` |

`latest_*` come from the most-recent (`ORDER BY id DESC LIMIT 1`) linked
`update` row regardless of its date; `updated_today` is true when any
linked `update` row has `date = today`. `partial` uses the same
`task LIKE '%' || ? || '%'` filter as `list_open_todos`.

## Detailed design

### Config (`config.rs`, `config.example.toml`)

New table:

```toml
[carryover]
# How many days back /progress add and the /team views look for
# unfinished todos to carry forward. A todo carries over until a "Done"
# progress report is filed against it or it ages past this window.
# Set to 0 to disable carry-over entirely.
# lookback_days = 7
```

- `RawConfig` gains `#[serde(default)] carryover: RawCarryover` with
  `RawCarryover { lookback_days: Option<u32> }`.
- `Config` gains `carryover_lookback_days: u32`.
- `DEFAULT_CARRYOVER_LOOKBACK_DAYS: u32 = 7`.
- `from_raw`: `raw.carryover.lookback_days.unwrap_or(default)`. No
  range validation — `0` is meaningful (disabled) and an absurdly large
  value just widens the window harmlessly.

### Threading the value

The discord `Handler` struct (`src/discord/mod.rs`) gains
`carryover_lookback_days: u32` next to `timezone`. It is passed to:

- `progress::handle_autocomplete` (the `add` branch)
- `team::handle_status`
- `team::handle_report`
- `todo::handle_list`

`ticker` already receives the whole `Config` and needs no carry-over
logic (carry-over never fires a reminder — see "Unchanged" below).

### `/progress add` autocomplete (`src/discord/progress.rs`)

In the `"add"` branch of `handle_autocomplete`, after fetching today's
`list_open_todos`:

1. fetch `carryover_todos(conn, user, date, lookback_days, partial)`
2. merge: today's open todos first, then carry-over, **dedup by todo
   id**, cap the combined list at 25
3. today's choices unchanged (`AutocompleteChoice::new(task, "id:{id}")`);
   carry-over choices labelled `"{task} · carried from {Mon D}"`
   (e.g. `Refactor auth · carried from Sep 3`), value still `id:{id}`

The merge/dedup/label step is a pure helper
(`merge_task_choices(today, carryover) -> Vec<AutocompleteChoice>` or a
plain `Vec<(String, String)>` to stay serenity-free) so it is
unit-testable.

Downstream is already correct: the modal `custom_id` carries `id:{id}`,
`handle_modal_submission` resolves it via
`entries::todo_task(conn, id, user)` which is **owner-scoped, not
date-scoped**, so `insert_update` links `todo_id` to the original todo
with no change. `/progress edit` also already works — a carry-over
update is dated today, so it is in scope for today's `list_updates`.

### `/team status` (`src/status.rs`)

Two changes in `team_status` / `format_status_line`:

1. **Correctness fix (required by carry-over):** `matched_update_count`
   currently is
   `COUNT(DISTINCT todo_id) FROM entries WHERE type='update' AND
   date=today AND discord_user_id=? AND todo_id IS NOT NULL` — it does
   **not** verify the linked todo is today's. Once carry-over updates
   exist this over-counts (e.g. `3/2 updated`). Scope it:
   `AND todo_id IN (SELECT id FROM entries WHERE type='todo' AND
   date=today AND discord_user_id=?)`.

2. `MemberStatus` gains `carried_count: i64` from
   `carryover_count(...)`. `format_status_line`:
   - `carried_count == 0` → line unchanged
   - `todo_count > 0`, `carried_count > 0` →
     `✅ Alice - 3/3 updated (M1D1) +2 carried`
     (the `+N carried` goes after the SOW-ref parens)
   - `todo_count == 0`, `carried_count > 0` →
     `⚠️ Alice - no new todo (2 carried)`
     (instead of `❌ Alice - no todo posted`)

### `/team report` (`src/status.rs`)

`MemberReport` gains `carried: Vec<CarryoverDetail>`.
`format_report`, after the member's today-todos and `ad_hoc` blocks,
appends when `carried` is non-empty:

```
Carried over:
• Refactor auth [M2] — from Sep 3 · ⏳ in progress · updated today
  traced the leak to the cache layer
• Write migration — from Sep 5 · ⛔ blocked
  (blocker: waiting on DBA)
```

- glyph/label via the existing `status_glyph_label`
- ` · updated today` only when `updated_today`
- the latest progress text on its own indented line; blocker (if any) as
  `(blocker: …)` — same shape as `push_update_line`
- a todo with **no** report yet (carried purely because it was never
  worked): `• Foo — from Sep 3 · no report yet`

A member whose only activity is carry-over still renders (the
`todos.is_empty() && ad_hoc.is_empty()` → "nothing posted today"
early-return also checks `carried.is_empty()`).

`split_into_messages` is unchanged — the carry-over block is just more
text inside a member block.

### `/todo list` (`src/discord/todo.rs`)

After today's todo list, when `carryover_todos` is non-empty, append:

```
**Carried over (still open):**
Refactor auth [M2] — from Sep 3
Write migration — from Sep 5
```

- no `` `id` `` prefix — carry-over todos are not editable/deletable
  from here (see below)
- SOW ref shown in `[...]` as in the main list
- if the member has **no** todo today, the existing
  `You haven't submitted a todo today yet - use /todo add.` line still
  shows, followed by this block

The list body build moves into a pure `format_todo_list(today, carried)`
helper for testability.

### Unchanged

- **`/todo edit`, `/todo delete`** — stay scoped to today's todos.
  Editing/deleting a past day's row is out of scope; the member can file
  a `done` report to close it out, or let it age past the window.
- **`/progress edit`** — already works (carry-over updates are dated
  today).
- **Follow-up nags** (`followups::members_missing_todo` /
  `members_missing_update`) — about *today's* obligations; carry-over
  does not create a nag.
- **Ticker thread-sync** — a carry-over `/progress add` produces a
  normal today-dated `update` row that syncs to the thread like any
  other.
- **`dispatchd maintenance run`** — still only prunes
  `reminders_sent` / `followups_sent`; `entries` is never pruned, so
  carry-over history is intact.

## Testing

DB-layer, real tempfile DB (never `:memory:`), per project conventions.

**`entries::carryover_todos`**
- past todo, no updates → included
- past todo, latest update `blocked` / `in_progress` → included
- past todo, has a `done` update (even if a later `blocked` one exists)
  → excluded
- with default 7: todo dated `D-7` → included, `D-8` → excluded
- today's todo → excluded
- another user's unfinished past todo → excluded
- `partial` substring filter applies
- 25-row cap
- `lookback_days == 0` → always empty

**`entries::carryover_report`**
- `latest_status` / `latest_progress` / `latest_blocker` reflect the
  highest-`id` linked update
- `updated_today` true iff a linked update is dated today
- todo with no updates → `latest_status` is `None`, `updated_today`
  false

**`entries::carryover_count`** — matches `carryover_todos().len()` for
the same inputs; `0` when `lookback_days == 0`.

**`status::team_status` / `format_status_line`**
- `matched_update_count` no longer counts a carry-over update against a
  past todo (the correctness fix)
- `carried_count` populated
- render: `+N carried` suffix; the `no new todo (N carried)` line

**`status::team_report` / `format_report`**
- carry-over block rendering (with report, without report, with blocker,
  `updated today`)
- member with only carry-over still renders
- fully-empty member still says "nothing posted today"

**`config`**
- default `carryover_lookback_days == 7`
- `[carryover] lookback_days = 3` override honored
- `lookback_days = 0` accepted

**autocomplete merge helper** — today's first, then carry-over, dedup by
id, cap 25, carry-over labels carry the origin date.

The serenity-wrapped handlers (`handle_autocomplete`, `handle_status`,
`handle_report`, `handle_list`) keep the same "can't test without a live
gateway" caveat as the rest of `src/discord/*` — all their real logic is
in the pure helpers above.

## Docs to update in the same PR

- **`docs/user-guide.md`** — new "Carrying work over" subsection (the
  rule, the 7-day default, `0` to disable, where it shows up); touch the
  `/progress add`, `/team status`, `/team report`, `/todo list`
  sections to mention the carry-over block.
- **`config.example.toml`** — the `[carryover]` block.
- **`CLAUDE.md`** — `[carryover]` key in the config notes; the new
  `entries.rs` / `status.rs` functions in the project-layout section;
  a line under `/progress` and `/team` about carry-over.

## Out of scope / future

- Editing or deleting a carried-over todo from `/todo`.
- A per-member "snooze / drop this carry-over" action (today the only
  way to stop a carry-over without finishing it is to let it age out).
- Surfacing carry-over in the automated reminders / follow-up nags.
- Making the "no `done` report" rule configurable (e.g. "latest status
  must be non-done" vs "any non-done").

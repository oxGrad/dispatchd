# Standup Cut-offs, Leave, Explicit Carry-over, `/team performance` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce `/todo` (11:00) and `/progress` (15:50) cut-offs judged from submission timestamps, waive a cut-off the bot was down for, let leads mark leave/sick days, make carry-over an explicit re-add, and replace `/missed` with a `/team performance` review report.

**Architecture:** Three new serenity-free modules hold the logic: `cutoffs.rs` (uptime spans, cut-off instants, waiver), `leave.rs` (leave rows), `performance.rs` (report data + `.md` rendering). `followups.rs` miss queries become timestamp- and leave-aware and are shared by the nags, the cut-off alerts, the 16:00 recording, the startup backfill, and leave re-derivation. Discord files (`ticker.rs`, `todo.rs`, `progress.rs`, `team.rs`, `mod.rs`) only wire these in.

**Tech Stack:** Rust 2024, serenity, tokio, rusqlite + rusqlite_migration, jiff. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-19-standup-cutoffs-design.md`

## Global Constraints

- No new crates. jiff for all time math; `Timestamp::to_string()` (UTC RFC3339) for every stored or compared instant.
- Miss rules compare `entries.created_at` as text against the cut-off instant's `Timestamp::to_string()` (`created_at < ?`), like `github::report::Window`. A sub-second edge at the boundary is accepted.
- Config defaults: `todo_cutoff_time = "11:00"`, `progress_cutoff_time = "15:50"` (both `[schedule]`).
- Config ordering, hard error at load: `todo_time < todo_cutoff_time <= progress_cutoff_time <= day_summary_time` and `update_time < progress_cutoff_time`.
- Uptime tolerance = `2 × ticker_interval_seconds`. Uptime spans are never pruned.
- `maintenance` keeps `reminders_sent` rows of type `missed_recorded`; everything else prunes as today.
- DB logic lives in serenity-free modules, tested on a real `tempfile` DB (never `:memory:`); any test mutating `DISPATCHD_*` env takes `crate::test_support::ENV_LOCK`.
- After every task: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all clean.
- Commits: conventional (`feat(scope): ...`), no `Co-Authored-By` trailer, no session URLs.
- PR #34 (`day_summary_missing` marked sent when empty) touches `ticker.rs`; rebase this branch on `main` once it merges, before Task 5.
- Message copy below is exact; `{todo_cutoff}` / `{progress_cutoff}` render as `HH:MM` (`time.strftime("%H:%M")`).

## Review Focus

1. **Upgrading with a config that breaks the new ordering** (e.g. `update_time = "16:00"`): the bot must refuse to start with an error naming the offending keys, not a generic parse error. Test: Task 1 `ordering_error_names_both_keys`.
2. **Non-UTC timezone**: a todo at 10:59 Asia/Jakarta (03:59Z) is on time and one at 11:01 is late. Test: Task 4 `missing_todo_respects_a_non_utc_cutoff`.
3. **Double-submitted carry-over modal** (double click / two tabs): the second submission must not create a second todo for the same origin. Test: Task 6 `carry_over_todo_refuses_a_second_carry_of_the_same_origin`.
4. **Deleting today's carried todo**: the origin must reappear as a carry-over candidate, not vanish. Test: Task 6 `deleting_the_carried_todo_restores_the_origin_as_a_candidate`.
5. **Leave change on today before 16:00**: re-derivation must not write misses (or a marker) for a day that isn't recorded yet. Test: Task 4 `rederive_member_ignores_unrecorded_days`.

---

### Task 1: Cut-off config keys and ordering validation

**Files:**
- Modify: `src/config.rs` (consts at top, `Config`, `Default`, `RawSchedule`, `from_raw`, tests)
- Modify: `config.example.toml` (`[schedule]` section, after `update_time`)

**Interfaces:**
- Produces: `Config.todo_cutoff_time: jiff::civil::Time`, `Config.progress_cutoff_time: jiff::civil::Time`.

- [ ] **Step 1: Write the failing tests** in `src/config.rs` tests:

```rust
#[test]
fn cutoffs_default_to_11_and_15_50_and_are_overridable() {
    let d = Config::default();
    assert_eq!(d.todo_cutoff_time, jiff::civil::time(11, 0, 0, 0));
    assert_eq!(d.progress_cutoff_time, jiff::civil::time(15, 50, 0, 0));
    let (_dir, path) = write_config(
        "[schedule]\ntodo_cutoff_time = \"10:30\"\nprogress_cutoff_time = \"15:45\"\n",
    );
    let c = Config::from_raw(read_raw_config(&path).unwrap()).unwrap();
    assert_eq!(c.todo_cutoff_time, jiff::civil::time(10, 30, 0, 0));
    assert_eq!(c.progress_cutoff_time, jiff::civil::time(15, 45, 0, 0));
}

#[test]
fn ordering_error_names_both_keys() {
    let (_dir, path) = write_config("[schedule]\nupdate_time = \"16:00\"\n");
    let err = format!("{:#}", Config::from_raw(read_raw_config(&path).unwrap()).unwrap_err());
    assert!(err.contains("update_time") && err.contains("progress_cutoff_time"), "{err}");
}

#[test]
fn every_ordering_rule_is_enforced() {
    for (toml, a, b) in [
        ("todo_cutoff_time = \"09:00\"", "todo_time", "todo_cutoff_time"),
        ("todo_cutoff_time = \"16:00\"", "todo_cutoff_time", "progress_cutoff_time"),
        ("progress_cutoff_time = \"16:30\"", "progress_cutoff_time", "day_summary_time"),
    ] {
        let (_dir, path) = write_config(&format!("[schedule]\n{toml}\n"));
        let err = format!("{:#}", Config::from_raw(read_raw_config(&path).unwrap()).unwrap_err());
        assert!(err.contains(a) && err.contains(b), "{toml}: {err}");
    }
}
```

Also: change `partial_schedule_override_changes_only_that_field` to use `update_time = "15:30"` (18:30 now violates the ordering) and assert `jiff::civil::time(15, 30, 0, 0)`; add `todo_cutoff_time = "10:00"` / `progress_cutoff_time = "16:30"` to `full_override_changes_every_field` with matching asserts (its `day_summary_time` is 18:00, so 16:30 is valid).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test config::tests`
Expected: compile error (`no field todo_cutoff_time`).

- [ ] **Step 3: Implement** — consts `DEFAULT_TODO_CUTOFF_TIME = "11:00"`, `DEFAULT_PROGRESS_CUTOFF_TIME = "15:50"`; the two `Config` fields (doc comments: deadline semantics per the spec); `RawSchedule` gets `todo_cutoff_time: Option<String>`, `progress_cutoff_time: Option<String>` parsed exactly like `todo_time` (error context `invalid schedule.<key>: {s:?}`). After building the `Config`, call a new `fn check_schedule_order(c: &Config) -> Result<()>` that checks the five relations from Global Constraints in order and bails with `"schedule.{a} ({a_val}) must be before schedule.{b} ({b_val})"` (use "on or before" for the `<=` relations), times as `HH:MM`.

- [ ] **Step 4: Document the keys** in `config.example.toml` after `update_time`, using the exact comment text from the spec's "Config" section.

- [ ] **Step 5: Run tests**

Run: `cargo test config::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat(config): add todo/progress cut-off times with ordering validation"
```

---

### Task 2: Migration 0009 and `cutoffs.rs` (uptime spans, instants, waiver)

**Files:**
- Create: `src/db/migrations/0009_cutoffs_leave_carryover.sql`
- Modify: `src/db/mod.rs` (register migration; `fresh_open_creates_all_tables` list)
- Create: `src/cutoffs.rs`
- Modify: `src/main.rs` (`mod cutoffs;`)

**Interfaces:**
- Consumes: `Config.todo_cutoff_time`, `Config.progress_cutoff_time` (Task 1).
- Produces (all in `crate::cutoffs`):
  - `pub struct Rules { pub tz: TimeZone, pub todo_time: Time, pub todo_cutoff: Time, pub update_time: Time, pub progress_cutoff: Time, pub tolerance: jiff::SignedDuration }` + `impl Rules { pub fn from_config(c: &Config) -> Rules }` (`#[derive(Debug, Clone)]`)
  - `pub struct DayCutoffs { pub todo_cutoff: String, pub progress_cutoff: String, pub todo_waived: bool, pub progress_waived: bool }` (`#[derive(Debug, Clone, PartialEq, Eq)]`; cut-offs are `Timestamp::to_string()`)
  - `pub fn instant(date: &str, time: Time, tz: &TimeZone) -> anyhow::Result<Timestamp>`
  - `pub fn covered(spans: &[(Timestamp, Timestamp)], lo: Timestamp, hi: Timestamp, tolerance: SignedDuration) -> bool`
  - `pub fn start_span(conn: &Connection, now: Timestamp) -> anyhow::Result<i64>`
  - `pub fn touch_span(conn: &Connection, id: i64, now: Timestamp) -> anyhow::Result<()>`
  - `pub fn day_cutoffs(conn: &Connection, rules: &Rules, date: &str) -> anyhow::Result<DayCutoffs>`
  - `pub fn ensure_rules_since(conn: &Connection, today: &str) -> anyhow::Result<String>`
  - `pub fn rules_since(conn: &Connection) -> anyhow::Result<Option<String>>`
  - Tables `member_leave(date, discord_user_id, reason)` PK `(date, discord_user_id)`, `uptime_spans(id, started_at, last_seen)`, `app_meta(key PK, value)`, column `entries.carried_from` with partial unique index.

- [ ] **Step 1: Write the migration** `0009_cutoffs_leave_carryover.sql`:

```sql
-- One row per member per leave/sick day (ritual days only). Never pruned.
CREATE TABLE member_leave (
    date            TEXT NOT NULL,
    discord_user_id TEXT NOT NULL,
    reason          TEXT NOT NULL,   -- 'leave' | 'sick'
    PRIMARY KEY (date, discord_user_id)
);
-- One row per process start; last_seen bumped every ticker tick. Never
-- pruned - the cut-off waiver for any recorded day is derived from it.
CREATE TABLE uptime_spans (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    last_seen  TEXT NOT NULL
);
-- Small durable key/value facts, e.g. 'cutoff_rules_since'.
CREATE TABLE app_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- The earlier todo this one continues (explicit carry-over).
ALTER TABLE entries ADD COLUMN carried_from INTEGER REFERENCES entries(id);
CREATE UNIQUE INDEX idx_entries_carried_from ON entries (carried_from)
    WHERE carried_from IS NOT NULL;
```

Register it as `M::up(include_str!("migrations/0009_cutoffs_leave_carryover.sql"))` and add `"app_meta"`, `"member_leave"`, `"uptime_spans"` to the sorted list in `fresh_open_creates_all_tables`.

- [ ] **Step 2: Write failing tests** in `src/cutoffs.rs` (`#[cfg(test)] mod tests`, `open_test_db` as in `followups.rs`):

```rust
fn ts(s: &str) -> Timestamp { s.parse().unwrap() }
const TOL: SignedDuration = SignedDuration::from_secs(120);

#[test]
fn instant_converts_local_time_to_utc() {
    let tz = TimeZone::get("Asia/Jakarta").unwrap();
    assert_eq!(instant("2026-11-19", jiff::civil::time(11, 0, 0, 0), &tz).unwrap().to_string(),
               "2026-11-19T04:00:00Z");
}

#[test]
fn covered_by_one_span_across_the_window() {
    let s = [(ts("2026-11-19T01:00:00Z"), ts("2026-11-19T05:00:00Z"))];
    assert!(covered(&s, ts("2026-11-19T02:00:00Z"), ts("2026-11-19T04:00:00Z"), TOL));
}

#[test]
fn a_restart_gap_within_tolerance_is_still_covered() {
    let s = [(ts("2026-11-19T01:00:00Z"), ts("2026-11-19T02:59:00Z")),
             (ts("2026-11-19T03:00:30Z"), ts("2026-11-19T05:00:00Z"))];
    assert!(covered(&s, ts("2026-11-19T02:00:00Z"), ts("2026-11-19T04:00:00Z"), TOL));
}

#[test]
fn a_gap_longer_than_tolerance_is_not_covered() {
    let s = [(ts("2026-11-19T01:00:00Z"), ts("2026-11-19T02:30:00Z")),
             (ts("2026-11-19T03:30:00Z"), ts("2026-11-19T05:00:00Z"))];
    assert!(!covered(&s, ts("2026-11-19T02:00:00Z"), ts("2026-11-19T04:00:00Z"), TOL));
}

#[test]
fn a_first_span_starting_inside_the_window_is_not_covered() {
    let s = [(ts("2026-11-19T03:00:00Z"), ts("2026-11-19T05:00:00Z"))];
    assert!(!covered(&s, ts("2026-11-19T02:00:00Z"), ts("2026-11-19T04:00:00Z"), TOL));
    assert!(!covered(&[], ts("2026-11-19T02:00:00Z"), ts("2026-11-19T04:00:00Z"), TOL));
}

#[test]
fn day_cutoffs_waives_only_the_window_with_downtime() {
    // UTC rules: todo 09:00-11:00, progress 15:00-15:50; bot down 10:30-11:30.
    let conn = open_test_db();
    let a = start_span(&conn, ts("2026-11-19T08:00:00Z")).unwrap();
    touch_span(&conn, a, ts("2026-11-19T10:30:00Z")).unwrap();
    let b = start_span(&conn, ts("2026-11-19T11:30:00Z")).unwrap();
    touch_span(&conn, b, ts("2026-11-19T17:00:00Z")).unwrap();
    let d = day_cutoffs(&conn, &utc_rules(), "2026-11-19").unwrap();
    assert_eq!(d, DayCutoffs {
        todo_cutoff: "2026-11-19T11:00:00Z".into(),
        progress_cutoff: "2026-11-19T15:50:00Z".into(),
        todo_waived: true,
        progress_waived: false,
    });
}

#[test]
fn rules_since_is_set_once_and_skips_an_already_recorded_today() {
    let conn = open_test_db();
    crate::reminders::mark_sent(&conn, "2026-11-19", "missed_recorded").unwrap();
    assert_eq!(ensure_rules_since(&conn, "2026-11-19").unwrap(), "2026-11-20");
    assert_eq!(ensure_rules_since(&conn, "2026-11-25").unwrap(), "2026-11-20");
    assert_eq!(rules_since(&conn).unwrap().as_deref(), Some("2026-11-20"));
}
```

`utc_rules()` is a test helper: tz UTC, 09:00 / 11:00 / 15:00 / 15:50, tolerance 120 s.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test cutoffs::`
Expected: compile errors (module items missing).

- [ ] **Step 4: Implement `src/cutoffs.rs`**
  - `instant`: `Date::strptime("%Y-%m-%d", date)?.to_datetime(time).to_zoned(tz.clone())?.timestamp()`.
  - `covered`: sort spans by start; sweep `cur = lo`; for each span, if `start > cur` return false, else `cur = max(cur, last_seen + tolerance)`; return true as soon as `cur >= hi`; false at the end. Only the *end* gets tolerance (a restart's gap is `prev.last_seen → next.started_at`), so a first-ever span starting inside the window is not covered — that's the rollout-day behaviour the spec wants.
  - `day_cutoffs`: load spans with `started_at < hi` (text compare) as parsed `Timestamp`s, compute the four instants, waived = `!covered(...)` over `[todo_time, todo_cutoff]` and `[update_time, progress_cutoff]`.
  - `ensure_rules_since`: `INSERT OR IGNORE INTO app_meta (key, value) VALUES ('cutoff_rules_since', ?)` with `today`, or the next day if `reminders::already_sent(conn, today, "missed_recorded")`; return the stored value. Key name `cutoff_rules_since`.
  - `Rules::from_config`: `tolerance = SignedDuration::from_secs(2 * c.ticker_interval_seconds as i64)`.

- [ ] **Step 5: Run tests**

Run: `cargo test cutoffs:: db::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/db src/cutoffs.rs src/main.rs
git commit -m "feat(cutoffs): uptime spans, cut-off instants and downtime waiver"
```

---

### Task 3: `leave.rs` (leave rows) and `members::role_of`

**Files:**
- Create: `src/leave.rs`; Modify: `src/main.rs` (`mod leave;`)
- Modify: `src/members.rs` (add `role_of` + test)

**Interfaces:**
- Consumes: table `member_leave` (Task 2).
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Reason { Leave, Sick }` with `pub fn as_str(self) -> &'static str` (`"leave"`/`"sick"`), `pub fn parse(s: &str) -> Option<Reason>`, `pub fn label(self) -> &'static str` (`"Leave"`/`"Sick"`)
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct LeaveDay { pub date: String, pub discord_user_id: String, pub name: String, pub reason: Reason }`
  - `pub fn ritual_days(start: &str, end: &str, include_weekends: bool) -> anyhow::Result<Vec<String>>` (pure, inclusive, ascending)
  - `pub fn add(conn: &Connection, discord_user_id: &str, dates: &[String], reason: Reason) -> anyhow::Result<usize>` (upsert; returns rows written)
  - `pub fn remove(conn: &Connection, discord_user_id: &str, dates: &[String]) -> anyhow::Result<usize>`
  - `pub fn list(conn: &Connection, start: &str, end: &str) -> anyhow::Result<Vec<LeaveDay>>` (ordered by date, then name; name falls back to the id like `missed_detail`)
  - `members::role_of(conn: &Connection, discord_user_id: &str) -> anyhow::Result<Option<String>>`

- [ ] **Step 1: Write failing tests** in `src/leave.rs`:

```rust
#[test]
fn ritual_days_skips_weekends_unless_included() {
    // 2026-11-20 is a Friday.
    assert_eq!(ritual_days("2026-11-20", "2026-11-23", false).unwrap(),
               vec!["2026-11-20", "2026-11-23"]);
    assert_eq!(ritual_days("2026-11-20", "2026-11-23", true).unwrap().len(), 4);
}

#[test]
fn add_upserts_the_reason_and_remove_deletes() {
    let conn = open_test_db();
    seed_member(&conn, "1", "Bima");
    let days = vec!["2026-11-20".to_string(), "2026-11-23".to_string()];
    assert_eq!(add(&conn, "1", &days, Reason::Leave).unwrap(), 2);
    add(&conn, "1", &days[..1], Reason::Sick).unwrap();
    let rows = list(&conn, "2026-11-01", "2026-11-30").unwrap();
    assert_eq!(rows.iter().map(|r| (r.date.as_str(), r.reason)).collect::<Vec<_>>(),
               vec![("2026-11-20", Reason::Sick), ("2026-11-23", Reason::Leave)]);
    assert_eq!(rows[0].name, "Bima");
    assert_eq!(remove(&conn, "1", &days).unwrap(), 2);
    assert!(list(&conn, "2026-11-01", "2026-11-30").unwrap().is_empty());
}
```

In `src/members.rs`: `role_of_returns_the_role_or_none` (seed one viewer, assert `Some("viewer")`, unknown id → `None`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test leave:: members::tests::role_of`
Expected: compile errors.

- [ ] **Step 3: Implement** `src/leave.rs` and `members::role_of` per the signatures (`add` uses `INSERT ... ON CONFLICT(date, discord_user_id) DO UPDATE SET reason = excluded.reason` in one transaction).

- [ ] **Step 4: Run tests**

Run: `cargo test leave:: members::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/leave.rs src/members.rs src/main.rs
git commit -m "feat(leave): store leave and sick days per member"
```

---

### Task 4: Timestamp- and leave-aware miss rules, waiver-aware recording, re-derivation

**Files:**
- Modify: `src/followups.rs` (queries, `record_missed`, `backfill_missed`, new `rederive_member`, tests)
- Modify: `src/discord/ticker.rs` (call sites only, so the crate builds: `run`, `tick`, `maybe_fire_followups`, `maybe_record_missed`)

**Interfaces:**
- Consumes: `cutoffs::{Rules, DayCutoffs, day_cutoffs, instant}` (Task 2), table `member_leave` (Task 2).
- Produces (signature changes):
  - `pub fn members_missing_todo(conn: &Connection, date: &str, todo_cutoff: &str) -> Result<Vec<String>>` — non-viewer, not on leave for `date`, no `type='todo'` row for `date` with `created_at < todo_cutoff`.
  - `pub fn members_missing_update(conn: &Connection, date: &str, progress_cutoff: &str) -> Result<Vec<String>>` — non-viewer, not on leave, with a `type='todo'` row for `date` and `created_at < progress_cutoff` that has no `type='update'` row (`todo_id` = it) with `created_at < progress_cutoff`.
  - `pub fn members_with_no_activity(conn: &Connection, date: &str) -> Result<Vec<String>>` — unchanged signature, now also excludes leave.
  - `pub fn record_missed(conn: &Connection, date: &str, day: &DayCutoffs) -> Result<()>` — skips the 'todo' kind when `day.todo_waived`, the 'update' kind when `day.progress_waived`; still writes the marker.
  - `pub fn backfill_missed(conn: &Connection, today: &str, include_weekends: bool, rules: &Rules) -> Result<Vec<String>>` — computes `day_cutoffs` per date.
  - `pub fn rederive_member(conn: &Connection, discord_user_id: &str, dates: &[String], rules: &Rules, rules_since: Option<&str>) -> Result<()>` — in one transaction, for each date that has a `missed_recorded` marker:
    - **on or after `rules_since`**: delete that member's `missed_submissions` rows for the date, then insert their rows for whichever of `members_missing_todo` / `members_missing_update` (that day's `DayCutoffs`, waived kinds skipped) contain them;
    - **before `rules_since`, or `rules_since` is `None`** (old end-of-day verdicts): delete the member's rows only if they're on leave that date now; never insert. Removing leave on such a day must not re-judge it, and a lead's `/team leave remove` on a day that never had leave must not erase an old miss.

    Unrecorded dates are untouched.

- [ ] **Step 1: Update existing tests to the new signatures.** Add test helpers:

```rust
/// A cut-off far in the future: every entry counts, which is what the
/// pre-existing tests assume.
const OPEN: &str = "9999-12-31T00:00:00Z";
fn open_day() -> DayCutoffs {
    DayCutoffs { todo_cutoff: OPEN.into(), progress_cutoff: OPEN.into(),
                 todo_waived: false, progress_waived: false }
}
fn set_created_at(conn: &Connection, id: i64, ts: &str) {
    conn.execute("UPDATE entries SET created_at = ?1 WHERE id = ?2", params![ts, id]).unwrap();
}
fn seed_leave(conn: &Connection, id: &str, date: &str) {
    crate::leave::add(conn, id, &[date.to_string()], crate::leave::Reason::Sick).unwrap();
}
/// UTC rules (09:00/11:00/15:00/15:50, 120 s tolerance) plus one uptime
/// span covering all of `date`, so nothing is waived.
fn covered_utc_rules(conn: &Connection, date: &str) -> crate::cutoffs::Rules { /* ... */ }
/// Distinct `discord_user_id`s in `missed_submissions`, sorted.
fn missed_ids(conn: &Connection) -> Vec<String> { /* ... */ }
```

Pass `OPEN` / `&open_day()` everywhere existing tests call the changed functions; `backfill_missed` tests pass a UTC `Rules` whose spans cover nothing — so add `start_span`/`touch_span` covering the backfilled days there, or assert waived behaviour explicitly where that's the point.

- [ ] **Step 2: Write the new failing tests:**

```rust
#[test]
fn missing_todo_counts_a_todo_after_the_cutoff_as_missed() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A"); seed_member(&conn, "2", "B");
    let a = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
    set_created_at(&conn, a, "2026-08-29T10:59:00Z");
    let b = entries::insert_todo(&conn, "2", DATE, "y", None, None).unwrap();
    set_created_at(&conn, b, "2026-08-29T11:01:00Z");
    assert_eq!(members_missing_todo(&conn, DATE, "2026-08-29T11:00:00Z").unwrap(), vec!["2"]);
}

#[test]
fn one_todo_before_the_cutoff_is_enough_even_with_a_later_one() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    let a = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
    set_created_at(&conn, a, "2026-08-29T09:15:00Z");
    let b = entries::insert_todo(&conn, "1", DATE, "y", None, None).unwrap();
    set_created_at(&conn, b, "2026-08-29T13:00:00Z");
    assert!(members_missing_todo(&conn, DATE, "2026-08-29T11:00:00Z").unwrap().is_empty());
}

#[test]
fn missing_todo_respects_a_non_utc_cutoff() {
    // Asia/Jakarta 11:00 = 04:00Z.
    let conn = open_test_db();
    seed_member(&conn, "1", "A"); seed_member(&conn, "2", "B");
    let a = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
    set_created_at(&conn, a, "2026-08-29T03:59:00Z");
    let b = entries::insert_todo(&conn, "2", DATE, "y", None, None).unwrap();
    set_created_at(&conn, b, "2026-08-29T04:01:00Z");
    let tz = jiff::tz::TimeZone::get("Asia/Jakarta").unwrap();
    let cutoff = crate::cutoffs::instant(DATE, jiff::civil::time(11, 0, 0, 0), &tz).unwrap();
    assert_eq!(members_missing_todo(&conn, DATE, &cutoff.to_string()).unwrap(), vec!["2"]);
}

#[test]
fn missing_update_ignores_todos_added_after_the_progress_cutoff() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    let t = entries::insert_todo(&conn, "1", DATE, "tomorrow's plan", None, None).unwrap();
    set_created_at(&conn, t, "2026-08-29T16:10:00Z");
    assert!(members_missing_update(&conn, DATE, "2026-08-29T15:50:00Z").unwrap().is_empty());
}

#[test]
fn a_report_after_the_progress_cutoff_does_not_count() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    let t = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
    set_created_at(&conn, t, "2026-08-29T09:00:00Z");
    let u = entries::insert_update(&conn, "1", DATE, "x", Some(t), "done", "ok", None).unwrap();
    set_created_at(&conn, u, "2026-08-29T15:55:00Z");
    assert_eq!(members_missing_update(&conn, DATE, "2026-08-29T15:50:00Z").unwrap(), vec!["1"]);
}

#[test]
fn members_on_leave_are_excluded_everywhere() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    seed_leave(&conn, "1", DATE);
    assert!(members_missing_todo(&conn, DATE, OPEN).unwrap().is_empty());
    assert!(members_with_no_activity(&conn, DATE).unwrap().is_empty());
    entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
    assert!(members_missing_update(&conn, DATE, OPEN).unwrap().is_empty());
}

#[test]
fn record_missed_skips_a_waived_kind() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    let day = DayCutoffs { todo_waived: true, ..open_day() };
    record_missed(&conn, DATE, &day).unwrap();
    let kinds: Vec<String> = conn.prepare("SELECT kind FROM missed_submissions").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    assert!(kinds.is_empty(), "{kinds:?}");
    assert!(crate::reminders::already_sent(&conn, DATE, "missed_recorded").unwrap());
}

#[test]
fn rederive_member_clears_then_restores_misses() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A"); seed_member(&conn, "2", "B");
    let rules = covered_utc_rules(&conn, DATE); // helper: UTC Rules + a span covering DATE
    record_missed(&conn, DATE, &crate::cutoffs::day_cutoffs(&conn, &rules, DATE).unwrap()).unwrap();
    seed_leave(&conn, "1", DATE);
    rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-01-01")).unwrap();
    assert_eq!(missed_ids(&conn), vec!["2"]); // helper: distinct ids in missed_submissions
    crate::leave::remove(&conn, "1", &[DATE.into()]).unwrap();
    rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-01-01")).unwrap();
    assert_eq!(missed_ids(&conn), vec!["1", "2"]);
}

#[test]
fn rederive_member_before_rules_since_only_clears_for_leave() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    let rules = covered_utc_rules(&conn, DATE);
    record_missed(&conn, DATE, &open_day()).unwrap();
    // No leave: an old-rule miss is left alone.
    rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-09-01")).unwrap();
    assert_eq!(missed_ids(&conn), vec!["1"]);
    // Leave added: cleared. Removed again: not re-judged under the new rules.
    seed_leave(&conn, "1", DATE);
    rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-09-01")).unwrap();
    assert!(missed_ids(&conn).is_empty());
    crate::leave::remove(&conn, "1", &[DATE.into()]).unwrap();
    rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-09-01")).unwrap();
    assert!(missed_ids(&conn).is_empty());
}

#[test]
fn rederive_member_ignores_unrecorded_days() {
    let conn = open_test_db();
    seed_member(&conn, "1", "A");
    let rules = covered_utc_rules(&conn, DATE);
    rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-01-01")).unwrap();
    assert!(missed_ids(&conn).is_empty());
    assert!(!crate::reminders::already_sent(&conn, DATE, "missed_recorded").unwrap());
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test followups::`
Expected: FAIL / compile errors on the new signatures.

- [ ] **Step 4: Implement** the signatures above. Leave exclusion is `AND discord_user_id NOT IN (SELECT discord_user_id FROM member_leave WHERE date = ?1)`; `members_missing_update` joins `members` for `role != 'viewer'`. Update doc comments to the new semantics (they currently describe "no todo at all").

- [ ] **Step 5: Fix the ticker call sites** so the crate builds:
  - `run`: `let rules = cutoffs::Rules::from_config(&config);` and pass `&rules` to `backfill_missed` and on into `tick`.
  - `maybe_fire_followups`: `query: fn(&Connection, &str, &str) -> anyhow::Result<Vec<String>>` plus a `cutoff: &str` argument; the two callers pass `cutoffs::instant(date, config.todo_cutoff_time / config.progress_cutoff_time, &config.timezone)?.to_string()` (nags ignore waivers).
  - `maybe_record_missed(db, date, rules)`: `day_cutoffs(&conn, rules, date)` then `record_missed(&conn, date, &day)`.

  Until Task 5 records uptime spans, every cut-off computes as waived; that's expected between these two commits.

- [ ] **Step 6: Run tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/followups.rs src/discord/ticker.rs
git commit -m "feat(followups): judge misses by cut-off timestamps, honour leave and waivers"
```

---

### Task 5: Ticker wiring — uptime, cut-off alerts, rules block

**Files:**
- Modify: `src/discord/ticker.rs` (`run`, `tick`, `maybe_fire_todo_reminder`, new alert fn, tests)

**Interfaces:**
- Consumes: `cutoffs::{Rules, start_span, touch_span, day_cutoffs, ensure_rules_since}` (Task 2); `followups::{members_missing_todo, members_missing_update}` (Task 4); the `rules` already threaded through `run`/`tick` in Task 4.
- Produces (pure, `ticker.rs`):
  - `fn standup_message(mentions: &str, todo_cutoff: Time, progress_cutoff: Time) -> String`
  - `#[derive(Clone, Copy)] enum CutoffKind { Todo, Progress }`
  - `fn cutoff_alert_message(kind: CutoffKind, cutoff: Time, missing: &[String], waived: bool) -> Option<String>`

- [ ] **Step 1: Write failing tests** in `ticker.rs` tests:

```rust
#[test]
fn standup_message_leads_with_the_rules() {
    assert_eq!(
        standup_message("<@1> <@2>", jiff::civil::time(11, 0, 0, 0), jiff::civil::time(15, 50, 0, 0)),
        "<@1> <@2>\n📌 **Standup rules**\n\
         • `/todo` and `/progress` are both required every day. Submitting one doesn't cover the other.\n\
         • Every todo needs its own `/progress` report.\n\
         • `/todo` cut-off: 11:00\n\
         • `/progress` cut-off: 15:50\n\
         • Anything not submitted by its cut-off is recorded and reflected in your performance review.\n\
         • On leave or sick? Tell your lead before 11:00.\n\
         • Unfinished from yesterday? Carry it over with `/todo add carryover:`.\n\
         📋 Time for today's standup! Submit your todo with `/todo add`."
    );
}

#[test]
fn cutoff_alerts_mention_the_missing() {
    let t = jiff::civil::time(11, 0, 0, 0);
    assert_eq!(
        cutoff_alert_message(CutoffKind::Todo, t, &["1".into(), "2".into()], false).unwrap(),
        "⛔ **/todo cut-off passed (11:00):** <@1> <@2> - no `/todo` submitted before the cut-off. This has been recorded. Submit it now with `/todo add`."
    );
    let p = jiff::civil::time(15, 50, 0, 0);
    assert_eq!(
        cutoff_alert_message(CutoffKind::Progress, p, &["3".into()], false).unwrap(),
        "⛔ **/progress cut-off passed (15:50):** <@3> - you have a todo with no `/progress` report. This has been recorded. Submit it now with `/progress add`."
    );
    assert_eq!(cutoff_alert_message(CutoffKind::Todo, t, &[], false), None);
}

#[test]
fn a_waived_cutoff_posts_a_notice_without_mentions() {
    assert_eq!(
        cutoff_alert_message(CutoffKind::Progress, jiff::civil::time(15, 50, 0, 0), &["1".into()], true).unwrap(),
        "⚠️ The bot was offline before the `/progress` cut-off (15:50), so today's `/progress` cut-off is waived. Please still submit with `/progress add`."
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test discord::ticker`
Expected: compile errors.

- [ ] **Step 3: Implement**
  - `run`: under the DB lock and *before* the backfill: `start_span(now)` (keep the id), `ensure_rules_since(today)`. Pass `span_id` into `tick`.
  - `tick`: `touch_span(span_id, now)` first, *before* the weekend early-return (uptime is tracked every day). Add, after the meeting reminder and before the day summary: `if is_due(now_time, config.todo_cutoff_time) { maybe_fire_cutoff_alert(.., CutoffKind::Todo, ..) }` and the same for `progress_cutoff_time`.
  - `maybe_fire_cutoff_alert(http, db, date, kind, rules)`: `already_sent` on `"todo_cutoff"` / `"progress_cutoff"`; compute `day_cutoffs`; query `members_missing_todo`/`members_missing_update` with that day's cut-off string; `cutoff_alert_message(...)`; `None` → `reminders::mark_sent` and return (same as PR #34); `Some` → `maybe_fire_simple_reminder`.
  - `maybe_fire_todo_reminder` uses `standup_message(&mentions, config.todo_cutoff_time, config.progress_cutoff_time)` (thread the two times or `&Config` through).
  - The waiver notice for `/todo` is the spec's text: `⚠️ The bot was offline before the `/todo` cut-off ({todo_cutoff}), so today's `/todo` cut-off is waived. Please still submit with `/todo add`.`

- [ ] **Step 4: Run the full suite**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add src/discord/ticker.rs
git commit -m "feat(ticker): cut-off alerts, standup rules block, uptime tracking"
```

---

### Task 6: Explicit carry-over

**Files:**
- Modify: `src/entries.rs` (carry-over queries, new fns, `todo_task`, tests)
- Modify: `src/discord/todo.rs` (`add` option, carry modal, autocomplete, help text, `format_todo_list` header, tests)
- Modify: `src/discord/progress.rs` (today-only autocomplete, reject non-today ids, drop `merge_task_choices` + its tests, help text)
- Modify: `src/discord/mod.rs` (dispatch `add` opts, carry modal route, autocomplete params)
- Modify: `docs/user-guide.md` (`/todo add carryover:`; `/progress add` lists today's todos only)

**Interfaces:**
- Consumes: `entries.carried_from` + unique index (Task 2).
- Produces:
  - `entries::carryover_origin(conn: &Connection, discord_user_id: &str, today: &str, lookback_days: i64, origin_id: i64) -> Result<Option<TodoForEdit>>` — `Some` only when `origin_id` is currently one of the member's carry-over candidates.
  - `entries::carry_over_todo(conn: &Connection, discord_user_id: &str, today: &str, lookback_days: i64, origin_id: i64, task: &str, notes: Option<&str>, sow_ref: Option<&str>) -> Result<Option<i64>>` — one transaction: re-check candidacy, insert today's todo with `carried_from = origin_id`; `None` if not a candidate (incl. already carried).
  - `entries::todo_task(conn, todo_id, discord_user_id, date: &str)` — now also scoped to `date`.
  - `carryover_todos` / `carryover_count` / `carryover_report` additionally require `NOT EXISTS (SELECT 1 FROM entries c WHERE c.carried_from = t.id)`.
  - `todo::CARRY_MODAL_PREFIX: &str = "todo_carry_modal:"` (custom_id = prefix + origin id).
  - `todo::handle_add(ctx, command, options, db, timezone, carryover_lookback_days: u32)`, `todo::handle_carry_modal_submission(ctx, modal, db, timezone, carryover_lookback_days: u32)`, `todo::handle_autocomplete(ctx, autocomplete, db, timezone, carryover_lookback_days: u32)`.
  - `progress::handle_autocomplete(ctx, autocomplete, db, timezone)` (lookback param removed).

- [ ] **Step 1: Write failing tests** in `src/entries.rs`:

```rust
#[test]
fn carry_over_todo_links_the_origin_and_hides_it_from_candidates() {
    let conn = open_test_db();
    let origin = insert_todo(&conn, "1", "2026-08-27", "Fix login", Some("plan"), Some("M1D2")).unwrap();
    let prefill = carryover_origin(&conn, "1", "2026-08-28", 7, origin).unwrap().unwrap();
    assert_eq!(prefill.task, "Fix login");
    let new_id = carry_over_todo(&conn, "1", "2026-08-28", 7, origin, "Fix login", Some("plan"), Some("M1D2"))
        .unwrap().unwrap();
    let carried: Option<i64> = conn.query_row("SELECT carried_from FROM entries WHERE id = ?1",
        [new_id], |r| r.get(0)).unwrap();
    assert_eq!(carried, Some(origin));
    assert!(carryover_todos(&conn, "1", "2026-08-28", 7, "").unwrap().is_empty());
    assert_eq!(carryover_count(&conn, "1", "2026-08-28", 7).unwrap(), 0);
    assert!(carryover_report(&conn, "1", "2026-08-28", 7).unwrap().is_empty());
}

#[test]
fn carry_over_todo_refuses_a_second_carry_of_the_same_origin() {
    let conn = open_test_db();
    let origin = insert_todo(&conn, "1", "2026-08-27", "x", None, None).unwrap();
    assert!(carry_over_todo(&conn, "1", "2026-08-28", 7, origin, "x", None, None).unwrap().is_some());
    assert_eq!(carry_over_todo(&conn, "1", "2026-08-28", 7, origin, "x", None, None).unwrap(), None);
}

#[test]
fn carry_over_todo_refuses_someone_elses_or_a_finished_todo() {
    let conn = open_test_db();
    let theirs = insert_todo(&conn, "2", "2026-08-27", "x", None, None).unwrap();
    assert_eq!(carry_over_todo(&conn, "1", "2026-08-28", 7, theirs, "x", None, None).unwrap(), None);
    let done = insert_todo(&conn, "1", "2026-08-27", "y", None, None).unwrap();
    insert_update(&conn, "1", "2026-08-27", "y", Some(done), "done", "ok", None).unwrap();
    assert_eq!(carry_over_todo(&conn, "1", "2026-08-28", 7, done, "y", None, None).unwrap(), None);
}

#[test]
fn deleting_the_carried_todo_restores_the_origin_as_a_candidate() {
    let conn = open_test_db();
    let origin = insert_todo(&conn, "1", "2026-08-27", "x", None, None).unwrap();
    let new_id = carry_over_todo(&conn, "1", "2026-08-28", 7, origin, "x", None, None).unwrap().unwrap();
    delete_todo(&conn, new_id, "1", "2026-08-28").unwrap();
    assert_eq!(carryover_todos(&conn, "1", "2026-08-28", 7, "").unwrap().len(), 1);
}

#[test]
fn todo_task_is_scoped_to_the_date() {
    let conn = open_test_db();
    let id = insert_todo(&conn, "1", "2026-08-27", "x", None, None).unwrap();
    assert_eq!(todo_task(&conn, id, "1", "2026-08-27").unwrap().as_deref(), Some("x"));
    assert_eq!(todo_task(&conn, id, "1", "2026-08-28").unwrap(), None);
}
```

(Check `delete_todo`'s actual parameter order in `entries.rs:408` and match it; update the existing `todo_task_returns_text_for_owning_user_only` test for the new `date` argument.)

In `src/discord/todo.rs`: `carryover_choice_label_shows_origin_date_and_fits_discord` — `fn carryover_choice_label(task: &str, origin_date: &str) -> String` returns `"{task} · from {short_date}"`, truncated with `truncate_chars` to 100 chars; and `todo_help_text_mentions_every_subcommand` gains `carryover:`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test entries:: discord::todo`
Expected: compile errors.

- [ ] **Step 3: Implement the DB side** (`entries.rs`) per the Interfaces block.

- [ ] **Step 4: Implement the Discord side**
  - `todo::command()`: the `add` subcommand gets `CreateCommandOption::new(CommandOptionType::String, "carryover", "Unfinished todo from an earlier day to carry over").required(false).set_autocomplete(true)`.
  - `handle_add`: no `carryover` option → today's blank modal (unchanged). Option present but not an integer, or `carryover_origin` returns `None` → ephemeral `"⚠️ Please pick an unfinished todo from the suggestions."`. Otherwise open the add modal prefilled (same three inputs as `handle_edit`, title `"Carry over todo"`) with custom_id `CARRY_MODAL_PREFIX{origin_id}`.
  - `handle_carry_modal_submission`: `carry_over_todo(...)`; `Some` → `"✅ Todo carried over: {task}"`; `None` → `"⚠️ That todo can't be carried over anymore (already carried, finished, or too old)."`.
  - `handle_autocomplete`: for the `add` subcommand, choices from `carryover_todos(..., partial)` with `carryover_choice_label`, value = id.
  - `format_todo_list`'s carried section header becomes `"**Not carried over yet** (use `/todo add carryover:`)"`; update its tests.
  - `TODO_HELP_TEXT`: `/todo add [carryover:<...>]` - submit a new todo for today, or carry over an unfinished one (prefilled).
  - `progress.rs`: `PROGRESS_HELP_TEXT`'s `add` line says it lists today's todos only (carry older ones over with `/todo add carryover:` first); `add` autocomplete = `list_open_todos` only; delete `merge_task_choices` and its two tests; in `handle_modal_submission`, an `id:` value whose `todo_task(.., &date)` is `None` replies `"⚠️ That todo isn't one of today's. Carry it over first with `/todo add carryover:`."` and inserts nothing.
  - `mod.rs`: `Some(("add", opts)) => todo::handle_add(&ctx, &command, opts, &self.db, &self.timezone, self.carryover_lookback_days)`; route `Interaction::Modal` with `CARRY_MODAL_PREFIX`; update both autocomplete calls.
  - `docs/user-guide.md`: carry-over section rewritten to the explicit flow.

- [ ] **Step 5: Run the full suite**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/entries.rs src/discord/todo.rs src/discord/progress.rs src/discord/mod.rs docs/user-guide.md
git commit -m "feat(todo): explicit carry-over via /todo add carryover:"
```

---

### Task 7: `/team leave add|remove|list`

**Files:**
- Modify: `src/discord/team.rs` (command group, helpers, handler, autocomplete, tests)
- Modify: `src/discord/mod.rs` (`Handler.rules: cutoffs::Rules`, dispatch)
- Modify: `src/recap.rs` (`resolve_month_or_range`)
- Modify: `src/discord/help.rs`, `docs/user-guide.md`

**Interfaces:**
- Consumes: `leave::*`, `members::role_of` (Task 3); `followups::rederive_member` (Task 4); `cutoffs::{Rules, rules_since}` (Task 2).
- Produces:
  - `recap::resolve_month_or_range(start: Option<&str>, end: Option<&str>, today: &str, max_days: i64) -> Result<(String, String), String>` — neither = 1st of `today`'s month → `today`; otherwise identical to `resolve_day_or_range` but capped at `max_days` (share one private inner fn).
  - `team::leave_subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])>` — `Some` when the top option is the `leave` `SubCommandGroup`.
  - `team::handle_leave(ctx, command, sub: &str, options, db, timezone, rules: &Rules, run_on_weekends: bool)`.
  - pure: `fn leave_added_reply(name: &str, reason: Reason, days: &[String]) -> String`, `fn leave_removed_reply(name: &str, removed: usize) -> String`, `fn format_leave_list(rows: &[LeaveDay], start: &str, end: &str) -> String`.

- [ ] **Step 1: Write failing tests**

`src/recap.rs`:

```rust
#[test]
fn month_or_range_defaults_to_the_month_so_far() {
    assert_eq!(resolve_month_or_range(None, None, "2026-11-19", 366).unwrap(),
               ("2026-11-01".into(), "2026-11-19".into()));
    assert_eq!(resolve_month_or_range(Some("2026-11-05"), None, "2026-11-19", 366).unwrap(),
               ("2026-11-05".into(), "2026-11-05".into()));
    assert!(resolve_month_or_range(Some("2025-01-01"), Some("2026-11-19"), "2026-11-19", 366)
        .unwrap_err().contains("366"));
}
```

`src/discord/team.rs`:

```rust
#[test]
fn leave_replies() {
    assert_eq!(leave_added_reply("Bima", Reason::Sick, &["2026-11-20".into(), "2026-11-23".into()]),
               "✅ Bima marked Sick for Nov 20, Nov 23.");
    assert_eq!(leave_added_reply("Bima", Reason::Leave, &[]),
               "⚠️ No working days in that range - nothing recorded.");
    assert_eq!(leave_removed_reply("Bima", 2), "✅ Removed 2 leave day(s) for Bima.");
}

#[test]
fn leave_list_formats_one_line_per_day() {
    let rows = vec![LeaveDay { date: "2026-11-20".into(), discord_user_id: "1".into(),
                               name: "Bima".into(), reason: Reason::Sick }];
    assert_eq!(format_leave_list(&rows, "2026-11-01", "2026-11-30"),
               "🏖️ **Leave 2026-11-01 to 2026-11-30**\n• Nov 20 · Bima · Sick");
    assert_eq!(format_leave_list(&[], "2026-11-01", "2026-11-30"),
               "No leave between 2026-11-01 and 2026-11-30.");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test recap:: discord::team`
Expected: compile errors.

- [ ] **Step 3: Implement**
  - `team::command()`: add a `CommandOptionType::SubCommandGroup` `"leave"` ("Mark members on leave or sick") with sub-commands `add` (`member` String required autocomplete; `reason` String required, choices `"Leave"→"leave"`, `"Sick"→"sick"`; `start`, `end` optional, descriptions as `/team github`), `remove` (`member`, `start`, `end`), `list` (`start`, `end`, description "default: this month").
  - `handle_leave`: gate `members::is_active_lead` with the same `⛔ This command is restricted to the tech lead.` reply as `/team remind`. For add/remove: member must have `role_of` = `Some(role)` with `role != "viewer"` (else `"⚠️ That member isn't tracked (not on the roster, or a viewer)."`); dates via `recap::resolve_day_or_range`, then `leave::ritual_days(.., run_on_weekends)`; write via `leave::add` / `leave::remove`; then `followups::rederive_member(conn, member, &days, rules, cutoffs::rules_since(conn)?.as_deref())`. `list` uses `resolve_month_or_range(.., 366)` + `leave::list`. Every reply ephemeral; DB errors log and reply `"⚠️ Something went wrong - please try again."`.
  - `handle_autocomplete`: read the `member` partial from either a plain subcommand or the `leave` group's nested subcommand.
  - `mod.rs`: `Handler` gains `rules: cutoffs::Rules` (from `Rules::from_config(&config)`); in the `"team"` arm, check `team::leave_subcommand` first and dispatch to `handle_leave`.
  - `help.rs`: add `/team leave add|remove|list` line (tech lead only) and the entry to its command-name test list. `docs/user-guide.md`: a `/team leave` section.

- [ ] **Step 4: Run the full suite**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/discord/team.rs src/discord/mod.rs src/recap.rs src/discord/help.rs docs/user-guide.md
git commit -m "feat(team): /team leave to mark leave and sick days"
```

---

### Task 8: `performance.rs` — report data and `.md` rendering

**Files:**
- Create: `src/performance.rs`; Modify: `src/main.rs` (`mod performance;`)

**Interfaces:**
- Consumes: `followups::missed_detail` (existing), `leave` table, `reminders_sent` `missed_recorded` markers, `github::report::{window, team_activity, MemberActivity}` (existing), `status::render_aligned_table`, `entries::short_date`.
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct MemberPerformance { pub name: String, pub expected: usize, pub on_time: usize, pub leave_dates: Vec<String>, pub sick_dates: Vec<String>, pub missed_todo: Vec<String>, pub missed_progress: Vec<String>, pub tasks: usize, pub done: usize, pub blocked_days: usize, pub github: Option<MemberActivity> }`
  - `pub fn performance(conn: &Connection, start: &str, end: &str, github: Option<&TimeZone>) -> Result<Vec<MemberPerformance>>` — one row per non-viewer member, alphabetical; `github: Some(tz)` fills `github` from `team_activity(conn, &window(start, end, tz)?)` matched by name, `None` leaves it `None`.
  - `pub fn format_performance_file(rows: &[MemberPerformance], start: &str, end: &str, with_github: bool, rules_since: Option<&str>) -> String`

Definitions (from the spec, exact):
- missed dates come from `followups::missed_detail(conn, start, end)`, matched to roster rows by name (it's the only remaining caller - without it `missed_detail` becomes dead code);
- recorded days = `reminders_sent` `type='missed_recorded'` dates in `[start, end]`;
- first-seen = earliest date in the member's `entries` ∪ `missed_submissions` ∪ `member_leave` rows;
- expected = recorded days `>= first-seen` minus the member's leave+sick dates; `on_time` = expected days with no `missed_submissions` row for the member;
- tasks = distinct chain roots (follow `carried_from` back to the todo whose `carried_from IS NULL`, recursive CTE) among the member's todos dated in range; done = those roots where any todo in the chain has a `status='done'` update dated `<= end`;
- blocked_days = distinct dates in range with a `status='blocked'` update by the member.

- [ ] **Step 1: Write failing tests.** `fixture()` builds a temp-file DB with:
  - members Bima ("1", senior), Alya ("2", senior), and a viewer ("9");
  - `missed_recorded` markers for Nov 3–7, 2026 (Mon–Fri);
  - Bima: todo A on Nov 3, carried (direct SQL `carried_from`) to Nov 4 and again to Nov 5, with a `done` update on the Nov 5 todo; todo B on Nov 6 with `blocked` updates on Nov 6 and Nov 7; a Sick leave row on Nov 4; `missed_submissions` rows `todo` Nov 5, `update` Nov 5 and Nov 6;
  - Alya: one todo on Nov 6 (her first record) and nothing missed.

  A test helper `row_cells(file, name) -> Vec<String>` returns the trimmed cells of the table row starting `| {name}`.

```rust
#[test]
fn discipline_counts_follow_the_spec() {
    // Recorded days Nov 3-7 (Mon-Fri); Bima first seen Nov 3; sick Nov 4;
    // missed /todo Nov 5; missed /progress Nov 5 and Nov 6.
    let conn = fixture();
    let bima = &performance(&conn, "2026-11-01", "2026-11-30", None).unwrap()[1];
    assert_eq!(bima.name, "Bima");
    assert_eq!((bima.expected, bima.on_time), (4, 2));
    assert_eq!(bima.sick_dates, vec!["2026-11-04"]);
    assert_eq!(bima.missed_todo, vec!["2026-11-05"]);
    assert_eq!(bima.missed_progress, vec!["2026-11-05", "2026-11-06"]);
}

#[test]
fn expected_starts_at_first_seen_and_viewers_are_omitted() {
    // Alya's first record is Nov 6 -> expected 2 (Nov 6, 7), not 5.
    let conn = fixture();
    let rows = performance(&conn, "2026-11-01", "2026-11-30", None).unwrap();
    assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Alya", "Bima"]);
    assert_eq!(rows[0].expected, 2);
}

#[test]
fn a_carried_chain_counts_as_one_task() {
    // Bima: todo A (Nov 3) carried to Nov 4 and Nov 5, done Nov 5; todo B
    // (Nov 6) blocked Nov 6 and Nov 7, never done.
    let conn = fixture();
    let bima = &performance(&conn, "2026-11-01", "2026-11-30", None).unwrap()[1];
    assert_eq!((bima.tasks, bima.done, bima.blocked_days), (2, 1, 2));
}

#[test]
fn file_renders_tables_then_people_and_the_rules_note() {
    let conn = fixture();
    let rows = performance(&conn, "2026-11-01", "2026-11-30", None).unwrap();
    let file = format_performance_file(&rows, "2026-11-01", "2026-11-30", false, Some("2026-11-03"));
    assert!(file.starts_with("# Team performance: Nov 1 – Nov 30\n\n> Misses before Nov 3 were recorded under the old end-of-day rule (no cut-offs).\n"), "{file}");
    assert!(file.contains("## Standup discipline\n\n| Member | Expected | Leave | Sick | Missed /todo | Missed /progress | On time |"));
    assert_eq!(row_cells(&file, "Bima")[..7], ["Bima", "4", "0", "1", "1", "2", "50%"]);
    assert!(file.contains("## Output\n\n| Member | Tasks | Done | Blocked days |"));
    assert!(!file.contains("PRs merged"));
    assert!(file.contains("## Bima\n- Sick: Nov 4\n- Missed /todo: Nov 5\n- Missed /progress: Nov 5, Nov 6\n"));
}

#[test]
fn on_time_is_a_dash_when_nothing_was_expected_and_no_note_after_rules_since() {
    let rows = vec![MemberPerformance { name: "New".into(), expected: 0, on_time: 0,
        leave_dates: vec![], sick_dates: vec![], missed_todo: vec![], missed_progress: vec![],
        tasks: 0, done: 0, blocked_days: 0, github: None }];
    let file = format_performance_file(&rows, "2026-12-01", "2026-12-05", false, Some("2026-11-03"));
    assert!(!file.contains("> Misses before"));
    assert!(file.contains("| —"), "{file}");
}
```

Plus one GitHub test: seed `members.github_user/github_id` and one merged PR + one review in the window (same inserts as `github/report/tests.rs`), call with `Some(&TimeZone::UTC)`, and assert the Output table has `PRs merged | PRs open | Reviews | Sub-issues closed` columns, Bima's row shows `1` merged, and his section has `- PRs merged: api#210`; an unlinked member's GitHub cells are `—`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test performance::`
Expected: compile errors.

- [ ] **Step 3: Implement** per the definitions. Rendering:
  - Header `# Team performance: {short_date(start)} – {short_date(end)}`; if `rules_since` is `Some(s)` and `start < s`, a blank line then `> Misses before {short_date(s)} were recorded under the old end-of-day rule (no cut-offs).`
  - `## Standup discipline` table (`Member | Expected | Leave | Sick | Missed /todo | Missed /progress | On time`), On time = `round(100 × on_time / expected)%` or `—` when expected is 0.
  - `## Output` table (`Member | Tasks | Done | Blocked days`, plus `PRs merged | PRs open | Reviews | Sub-issues closed` when `with_github`; cells `—` when the member isn't `linked`).
  - Then `## {name}` per member with non-empty lines only, in order: Leave, Sick, Missed /todo, Missed /progress, PRs merged, Reviewed (dates via `short_date`, items via `ItemRef`'s `Display`, comma-joined). Skip a member with no lines.
  - Escape `|` in names as `followups` does (`replace('|', "/")`).

- [ ] **Step 4: Run tests**

Run: `cargo test performance::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/performance.rs src/main.rs
git commit -m "feat(performance): per-member review report data and rendering"
```

---

### Task 9: `/team performance`, remove `/missed`, retention, docs

**Files:**
- Modify: `src/discord/team.rs` (`performance` subcommand + handler; `pending_today_note` and `is_ritual_day` moved here with their tests)
- Delete: `src/discord/missed.rs`
- Modify: `src/discord/mod.rs` (drop `mod missed`, registration, dispatch; add `performance` dispatch)
- Modify: `src/followups.rs` (delete `format_missed_report_file`, `MissedDetail::missed_days`, `short_date_list` if now unused, and their tests)
- Modify: `src/maintenance.rs` (+ test)
- Modify: `src/discord/help.rs`, `docs/user-guide.md`, `CLAUDE.md`

**Interfaces:**
- Consumes: `performance::{performance, format_performance_file}` (Task 8); `recap::resolve_month_or_range` (Task 7); `cutoffs::rules_since` (Task 2); `super::recap::{attachment_name, range_label}`.
- Produces: `team::handle_performance(ctx, command, options, db, timezone, github_on: bool, day_summary_time: Time, run_on_weekends: bool)`.

- [ ] **Step 1: Write failing tests**

`src/maintenance.rs`:

```rust
#[test]
fn missed_recorded_markers_survive_pruning() {
    let conn = open_test_db();
    crate::reminders::mark_sent(&conn, "2000-01-03", "missed_recorded").unwrap();
    crate::reminders::mark_sent(&conn, "2000-01-03", "todo_reminder").unwrap();
    run(&conn).unwrap();
    assert!(crate::reminders::already_sent(&conn, "2000-01-03", "missed_recorded").unwrap());
    assert!(!crate::reminders::already_sent(&conn, "2000-01-03", "todo_reminder").unwrap());
}
```

`src/discord/help.rs`: the command list test drops `"/missed"` and contains `"/team performance"` (and `"/team leave"` from Task 7).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test maintenance:: discord::help`
Expected: FAIL.

- [ ] **Step 3: Implement**
  - `maintenance::run`: `... WHERE date < date('now', ?1) AND type != 'missed_recorded'`; update the doc comment.
  - `team::command()`: subcommand `performance` ("Per-member review: standup discipline and output for a period") with optional `start` / `end` ("default: this month").
  - `handle_performance`: gate `members::is_lead` (same ⛔ text as `/missed`); range via `resolve_month_or_range(.., 366)`; `performance(conn, start, end, github_on.then_some(timezone))`; `rules_since`; `pending_today_note` as `/missed` computed it. Reply ephemeral: `"📈 **Team performance ({range_label})** - see attached."` (+ `\n{note}` when present) with attachment `attachment_name("performance", start, end)` holding `format_performance_file(..)`; empty roster → `"No team members to report on."` with no file.
  - Delete `/missed` per the file list; `mod.rs` passes `self.day_summary_time`, `self.run_on_weekends`, `self.github.is_some()` to `handle_performance`.
  - `help.rs`: replace the `/missed` line with `/team performance start:<date> end:<date>` - (tech lead only) per-member review for a period; no dates = this month.
  - `docs/user-guide.md`: replace the `/missed` section with `/team performance`; add the cut-off rules, the alerts, and the downtime waiver to the daily-flow section.
  - `CLAUDE.md`: project layout gains `cutoffs.rs`, `leave.rs`, `performance.rs`, migration 0009; drop `missed.rs`; update the `followups.rs`, `ticker.rs`, `team.rs`, `todo.rs`, `progress.rs`, `maintenance.rs` and config notes to match.

- [ ] **Step 4: Run everything**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build --release`
Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add -A src docs CLAUDE.md
git commit -m "feat(team): /team performance review report, replacing /missed"
```

- [ ] **Step 6: Live check (manual, needs a real guild)** — per CLAUDE.md, gateway code is unverified until run: with a `.testenv` config whose cut-offs are a few minutes ahead, confirm the rules block, both cut-off alerts, a waiver notice after stopping the bot across a cut-off, `/todo add carryover:` prefill, `/team leave add`, and `/team performance` attachment.

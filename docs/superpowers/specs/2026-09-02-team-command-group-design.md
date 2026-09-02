# `/team` command group — design

**Date:** 2026-09-02
**Status:** Approved, ready for implementation plan

## Summary

Replace the standalone `/team-status` command with a `/team` command group
holding three tech-lead-only subcommands:

- `/team status` — the existing one-line-per-member summary, moved verbatim.
- `/team report` — full detail: every member's todos for today, each with its
  notes, SOW ref, and matching progress report(s), plus any unplanned progress.
- `/team remind` — the tech lead sends a manual reminder (to submit a todo, or
  to post a progress update) to one team member via `member` + `kind`
  arguments; the reminder is posted into today's standup thread.

`/team-status` is removed. No database schema change.

## Motivation

- The tech lead currently only has `/team-status`, which shows *whether* each
  member updated, not *what* they said. Reviewing the actual content means
  scrolling the standup thread. `/team report` gives the lead the full picture
  in one ephemeral message.
- The automated follow-up nags (`ticker.rs` → `followups.rs`) fire on a fixed
  schedule. The lead has no way to prompt a specific person off-schedule.
  `/team remind` fills that gap without disturbing the automated track.
- Grouping the three under `/team` keeps the command list tidy and gives the
  lead a single namespace to remember.

## Current state (what exists today)

- `src/discord/team_status.rs` — `command()` returns `CreateCommand::new("team-status")`
  with `.default_member_permissions(Permissions::MANAGE_GUILD)`; `handle_command`
  does the `members::is_lead` gate then renders `status::team_status` +
  `status::format_status_line`.
- `src/discord/mod.rs` — registers the command in the `ready` handler's
  `commands` vec; dispatches `"team-status"` in the `Interaction::Command` arm.
  Handles `Interaction::Command`, `Interaction::Autocomplete`, and
  `Interaction::Modal` today.
- `src/discord/todo.rs` — the existing precedent for a multi-subcommand command:
  `subcommand()` helper unwrapping `CommandDataOptionValue::SubCommand`, one
  `handle_*` per subcommand, autocomplete handler, modal custom_id encoding
  (`EDIT_MODAL_PREFIX`).
- `src/status.rs` — `team_status` (per-member `COUNT` queries, "simple not one
  big JOIN, fine for 6 people"), `format_status_line`, `MemberStatus`. Fully
  unit-tested against a real temp-file DB.
- `src/reminders.rs` — `thread_for(conn, date) -> Result<Option<String>>` returns
  today's standup thread id.
- `src/followups.rs` — `members_missing_todo` / `members_missing_update`,
  `already_sent` / `mark_sent` against `followups_sent`.
- `src/discord/ticker.rs` — `maybe_fire_followups` posts `<@id> <message>` into
  the standup thread, treating a `10003 Unknown Channel` error as "thread
  deleted, give up" (`is_unknown_channel_error`).
- `src/members.rs` — `is_lead`, `all_member_ids` (ids only, no names).
- `src/config.rs` — `Config { thread_creation_time, todo_time, update_time,
  meeting_reminder_time, timezone, .. }` (all `NaiveTime` / `Tz`).
- `Handler` struct in `mod.rs` holds `{ guild_id, db, timezone }` — **not** the
  full `Config`.

## Design

### 1. Module rename and command surface

- Rename `src/discord/team_status.rs` → `src/discord/team.rs`; `mod team_status;`
  → `mod team;` in `mod.rs`.
- `team::command()` returns:

  ```rust
  CreateCommand::new("team")
      .description("Tech-lead tools: team status, full report, manual reminders")
      .default_member_permissions(Permissions::MANAGE_GUILD)
      .add_option(CreateCommandOption::new(SubCommand, "status",
          "One-line-per-member update summary for today"))
      .add_option(CreateCommandOption::new(SubCommand, "report",
          "Full detail: everyone's todos and progress for today"))
      .add_option(
          CreateCommandOption::new(SubCommand, "remind",
              "Remind a team member to submit a todo or progress update")
          .add_sub_option(
              CreateCommandOption::new(String, "member", "Who to remind")
                  .required(true).set_autocomplete(true))
          .add_sub_option(
              CreateCommandOption::new(String, "kind", "What to remind them about")
                  .required(true)
                  .add_string_choice("Submit a todo", "todo")
                  .add_string_choice("Post a progress update", "progress")),
      )
  ```

  Both `member` and `kind` are **required** — Discord's client won't submit the
  subcommand until they're filled, so the handler can assume both are present
  (with a defensive fallback if not).

- `team::subcommand(options) -> Option<(&str, &[CommandDataOption])>` — copy of
  `todo::subcommand`.

### 2. `mod.rs` routing changes

- `ready` handler: replace `team_status::command()` with `team::command()`.
- `Interaction::Command` arm: replace the `"team-status"` branch with:

  ```rust
  "team" => match team::subcommand(&command.data.options) {
      Some(("status", _)) => team::handle_status(&ctx, &command, &self.db, &self.timezone).await,
      Some(("report", _)) => team::handle_report(&ctx, &command, &self.db, &self.timezone).await,
      Some(("remind", opts)) => team::handle_remind(&ctx, &command, opts, &self.db, &self.timezone).await,
      _ => {}
  },
  ```

- `Interaction::Autocomplete` arm: add `"team" => team::handle_autocomplete(&ctx, &autocomplete, &self.db).await`.

No `Interaction::Component` handling is added — `/team remind` is
arguments-only.

### 3. `/team status`

`team::handle_status` is the current `team_status::handle_command` body,
unchanged. `status::team_status` / `format_status_line` / `MemberStatus` stay
where they are.

### 4. `/team report`

#### 4a. Query — `src/status.rs`

```rust
pub struct MemberReport {
    pub name: String,
    pub todos: Vec<TodoDetail>,
    pub ad_hoc: Vec<UpdateDetail>,   // updates with todo_id IS NULL
}

pub struct TodoDetail {
    pub task: String,
    pub notes: Option<String>,
    pub sow_ref: Option<String>,
    pub updates: Vec<UpdateDetail>,  // updates whose todo_id == this todo's id, in id order
}

pub struct UpdateDetail {
    pub task: String,                // the update row's own task text (used for ad_hoc)
    pub status: String,              // 'done' | 'in_progress' | 'blocked'
    pub progress: String,
    pub blocker: Option<String>,
}

pub fn team_report(conn: &Connection, date: &str) -> Result<Vec<MemberReport>>;
```

Implementation mirrors `team_status`'s style: one `SELECT discord_user_id, name
FROM members ORDER BY name`, then per member:

- `SELECT id, task, notes, sow_ref FROM entries WHERE type='todo' AND date=?1
  AND discord_user_id=?2 ORDER BY id`
- for each todo id: `SELECT task, status, progress, blocker FROM entries WHERE
  type='update' AND todo_id=?1 ORDER BY id`
- ad-hoc: `SELECT task, status, progress, blocker FROM entries WHERE
  type='update' AND date=?1 AND discord_user_id=?2 AND todo_id IS NULL ORDER BY id`

Per-member N+1 is acceptable at 6 members × a handful of todos, consistent with
the existing rationale in `status.rs`.

#### 4b. Formatting — `src/status.rs`

```rust
pub fn format_report(report: &MemberReport) -> String;
```

- Member with no todos and no ad-hoc updates: `**{name}** — nothing posted today`
- Otherwise:
  ```
  **{name}**
  • {task} [{sow_ref}]            (the ` [sow_ref]` omitted when None)
    notes: {notes}                (line omitted when None)
    {glyph} {status_label} — {progress} (blocker: {blocker})
    {glyph} ...                   (one line per update; "(blocker: …)" omitted when None)
  • {task}
    ❌ no progress report yet      (when updates is empty)
  • unplanned: {task}             (one bullet per ad_hoc update)
    {glyph} {status_label} — {progress} (blocker: {blocker})
  ```
- Glyphs / labels: `done` → `✅ done`, `in_progress` → `⏳ in progress`,
  `blocked` → `⛔ blocked`. Unknown status value → `• {status}` verbatim
  (defensive, shouldn't happen).

#### 4c. Chunking — `src/status.rs`

```rust
pub fn split_into_messages(full: &str, limit: usize) -> Vec<String>;
```

- Joins per-member blocks with a blank line. Accumulates blocks into a chunk
  until adding the next would exceed `limit`; then starts a new chunk.
- If a single member's block alone exceeds `limit`, that block is hard-wrapped
  at `limit`-sized char boundaries (rare — would need a very verbose member).
- Callers pass `limit = 2000` (Discord message content cap). Returns at least
  one string (possibly empty-ish "no members" text handled by the caller before
  calling this).

#### 4d. Handler — `team::handle_report`

- `is_lead` gate (same as status). Non-lead → `⛔ This command is restricted to the tech lead.`
- Empty roster → `No team members configured yet - see members.toml.` (verbatim
  from `team_status`).
- Otherwise: build `full = reports.iter().map(format_report).collect().join("\n\n")`,
  `chunks = split_into_messages(&full, 2000)`.
  - First chunk → `command.create_response(..)` ephemeral.
  - Remaining chunks → `command.create_followup_message(.. .ephemeral(true))` in
    order. A follow-up send error is logged and does not abort the rest.

### 5. `/team remind`

#### 5a. Autocomplete — `team::handle_autocomplete`

- Read the focused `member` option value (partial string) via the same pattern
  as `todo`/`progress` autocomplete.
- `members::roster(conn)` → `Vec<(String /*id*/, String /*name*/)>` ordered by
  name.
- Filter: `name.to_lowercase().contains(&partial.to_lowercase())`. Up to 25
  `AutocompleteChoice::new(name, id)`.

#### 5b. Handler — `team::handle_remind`

- `is_lead` gate.
- Read `member` (id string) and `kind` from `opts` via `get_option_string`.
  Both are `required`, so either missing → `⚠️ Provide both a member and a
  kind.` ephemeral (defensive; not reachable from a normal client).
- `RemindKind::parse(kind)` — unknown → `⚠️ Unknown reminder kind.` ephemeral.
- Validate `member` id is on the roster (`members::name_of(conn, id)` returns
  `Some`). Not on roster → `⚠️ That user isn't on the team roster.` ephemeral.
- Call the send path (§6). Reply ephemerally with the outcome:
  - `Sent` → `✅ Reminder sent to {name} in today's standup thread.`
  - `NoThread` → `⚠️ Today's standup thread hasn't been created yet — try again
    after it's posted.`
  - `ThreadGone` → `⚠️ Today's standup thread appears to have been deleted.`
  - `Failed` → `⚠️ Something went wrong sending the reminder.`

### 6. Send path — `team.rs`

```rust
enum RemindKind { Todo, Progress }

impl RemindKind {
    fn parse(s: &str) -> Option<Self>;          // "todo" | "progress"
    fn as_str(&self) -> &'static str;
    fn reminder_text(&self, user_id: &str) -> String;
}

enum SendOutcome { Sent, NoThread, ThreadGone, Failed }

async fn send_reminder(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    member_id: &str,
    kind: RemindKind,
) -> SendOutcome;
```

- `date = entries::today_in(timezone)`.
- `reminders::thread_for(conn, &date)`:
  - `Ok(None)` → `NoThread`
  - `Ok(Some(tid))`, `tid.parse::<u64>()` ok → `ChannelId::new(tid).send_message(
    http, CreateMessage::new().content(kind.reminder_text(member_id)))`:
    - `Ok` → `Sent`
    - `Err(e)` where `is_unknown_channel_error(&e)` → `ThreadGone`
    - `Err(e)` otherwise → `eprintln!` + `Failed`
  - parse failure / `Err` from `thread_for` → `eprintln!` + `Failed`
- **No** read or write of `followups_sent`. The manual reminder is fully
  independent of the ticker's automated nags (per the approved decision).
- `is_unknown_channel_error` currently lives in `ticker.rs` as a private fn.
  Move it (and `UNKNOWN_CHANNEL_ERROR_CODE` / `is_unknown_channel_code`) to a
  shared spot — `discord/mod.rs` as `pub(crate)` — and have both `ticker.rs`
  and `team.rs` use it. Keep the existing `is_unknown_channel_code` unit test,
  relocated.

`reminder_text`:

- `Todo` → `👋 <@{user_id}> — reminder from the tech lead: please submit your \`/todo\` for today.`
- `Progress` → `👋 <@{user_id}> — reminder from the tech lead: please post a \`/progress\` update for today's todo(s).`

### 7. `src/members.rs` additions

```rust
/// Every member as (discord_user_id, name), ordered by name. For the
/// `/team remind` autocomplete.
pub fn roster(conn: &Connection) -> Result<Vec<(String, String)>>;

/// A member's display name, or None if the id isn't on the roster.
pub fn name_of(conn: &Connection, discord_user_id: &str) -> Result<Option<String>>;
```

### 8. Help + docs

- `src/discord/help.rs`: replace the `/team-status` line with
  `\`/team status\` - (tech lead) one line per member: who's updated today`,
  `\`/team report\` - (tech lead) full detail of everyone's todos + progress`,
  `\`/team remind\` - (tech lead) nudge a member to submit a todo or progress update`.
  Update the test needles list accordingly (`/team status`, `/team report`,
  `/team remind`; drop `/team-status`).
- `README.md`: replace the `/team-status` table row with three rows (or one row
  for `/team` + a sub-list) covering the three subcommands.
- `docs/discord-setup.md`:
  - §5 command list: `/team-status` → `/team status` (+ mention `report` /
    `remind` exist).
  - §6 heading `## 6. \`/team-status\` permissions` → `## 6. \`/team\` permissions`;
    body updated to say the `MANAGE_GUILD` default and `is_lead` check apply to
    all three subcommands, and the per-subcommand override path is
    **Server Settings → Integrations → dispatchd → team → status / report / remind**.
- `docs/user-guide.md`: rename the `## /team-status` section to `## /team status`,
  add `## /team report` and `## /team remind` sections (what each shows / does,
  lead-only, where the reminder lands).
- `CLAUDE.md`:
  - The `(\`/todo\`, \`/progress\`, \`/team-status\`)` mentions (lines ~4, ~139,
    ~148) → `/team status` (and note `/team report`, `/team remind`).
  - Project-layout block: `status.rs` comment (add `team_report` / report
    formatting + chunking), `team_status.rs` entry → `team.rs` describing the
    three subcommands.

### 9. Not in scope

- No DB schema migration (`entries` already has every needed column).
- No change to `/todo`, `/progress`, `/help` behaviour, or the ticker's
  automated follow-up logic.
- `/team remind` targets exactly one member per invocation — no "remind everyone
  missing X" bulk path (deferred; could be added later as a `kind`-only
  invocation that fans out over `followups::members_missing_*`).
- **No no-args interactive picker.** A `/team remind` with select-menu dropdowns
  was considered and dropped: modals can't hold dropdowns, and a select-menu
  message would need an `Interaction::Component` state machine the bot otherwise
  doesn't have. `member` autocomplete + the `kind` choice dropdown already give
  a pick-from-a-list UX inside the command picker.
- No persistence / audit log of manually-sent reminders.

## Testing

Pure / DB-layer only (serenity handlers stay uncovered, consistent with the
codebase — note this explicitly in the relevant modules).

- **`src/status.rs`**
  - `team_report`: todo with two updates → both listed in id order; ad-hoc
    update surfaces in `ad_hoc` not under a todo; member with nothing → empty
    `todos` + empty `ad_hoc`; `sow_ref` / `notes` populated through; members
    ordered by name.
  - `format_report`: fully-updated todo; todo with no update →
    `❌ no progress report yet`; blocked update → `⛔ blocked` + `(blocker: …)`;
    `sow_ref` tag present/absent; `notes` line present/absent; ad-hoc bullet
    prefix `unplanned:`; "nothing posted today" line.
  - `split_into_messages`: single short report → one chunk; several members
    over 2000 → splits on member boundary, every chunk ≤ 2000; one oversized
    member block → hard-wrapped, still ≤ 2000 per chunk; `""` → `[]` (the
    handler guards the empty-roster case before calling, so this is just a
    defensive assertion).
- **`src/discord/team.rs`**
  - `RemindKind::parse` round-trips `"todo"` / `"progress"`, `None` for junk.
  - `reminder_text` contains `<@{id}>` and the right command name.
  - `is_unknown_channel_code` test relocated here or to `mod.rs` with the fn.
- **`src/members.rs`**
  - `roster`: returns `(id, name)` for every seeded member, ordered by name.
  - `name_of`: `Some(name)` for a seeded id, `None` for an unknown id.
- **`src/discord/help.rs`**
  - needles updated to `/team status`, `/team report`, `/team remind`.

## Rollout

- Guild commands are overwritten wholesale on every `ready`
  (`guild_id.set_commands`), so on the next restart `/team-status` disappears and
  `/team` appears within seconds — no stale-command window, no manual cleanup.
- No config or DB migration; a running instance picks everything up on restart.
- `docs/user-guide.md` change is the user-facing "what changed" note for the team.

## Open questions

None. All decisions resolved during brainstorming:

- Subcommand name for the detailed view: **`report`**.
- Report content: **full detail** (todos + notes + SOW ref + nested progress + ad-hoc).
- Permission: **tech lead only** for all three (unchanged from `/team-status`).
- `/team-status`: **removed** outright.
- `/team remind` targeting: **single member per invocation, no bulk path**;
  `member` + `kind` are both required arguments.
- Reminder destination: **today's standup thread**.
- Manual vs automated follow-up: **independent** (no `followups_sent` interaction).
- Interactive picker: **dropped** — arguments-only (`member` autocomplete +
  `kind` choice dropdown).

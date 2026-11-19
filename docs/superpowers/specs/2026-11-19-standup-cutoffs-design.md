# Standup cut-offs, leave, explicit carry-over, and `/team performance`

Date: 2026-11-19. Status: approved design, not yet planned.

## Why

Standup submissions now feed performance reviews. Today the bot only
nags: misses are recorded once at `day_summary_time` (16:00) from
whatever exists at that moment, so a `/todo` posted at 15:00 counts as
on time, someone on leave is recorded as missing, carried-over work
never has to be re-declared or reported on, and there's no report a lead
can take into a review. This change makes the rules explicit, enforces
them with real deadlines, lets leads excuse absences, and adds one
review report covering both discipline and output (incl. GitHub).

## Goals

1. Two configurable cut-offs: `/todo` (default 11:00) and `/progress`
   (default 15:50). A submission after its cut-off counts as missed,
   exactly as if it never came.
2. At each cut-off, one thread message @mentioning every member still
   missing that submission.
3. The day's rules, as bullets, at the top of the morning standup
   message.
4. `/team leave` so a lead can mark a member on leave or sick, for any
   date range (past, today, future); that member is neither nagged nor
   recorded on those days.
5. Carry-over becomes explicit: yesterday's unfinished todo must be
   re-added to today (prefilled), and `/progress add` only offers
   today's todos.
6. `/team performance`: a per-member report for a period (default: the
   current month) - standup discipline plus output, incl. GitHub.
7. Delete `/missed` (unused; superseded by 6).

## Non-goals

- Team-wide public holidays. The bot still runs and records everyone as
  missed on a holiday unless each member is marked on leave. Tracked
  separately in oxGrad/dispatchd#35.
- Discord-side outages while the bot itself is up. The uptime waiver
  below only sees the bot's own process; a Discord outage across a
  cut-off is not detected.
- Recomputing history. Days recorded before this ships keep their old
  end-of-day verdicts; nobody is judged retroactively against cut-offs
  they were never told about.
- Leave approval workflows, quotas, or member self-service. The lead
  records leave; members tell the lead.
- Per-member cut-offs, DMs, or any channel other than the standup
  thread.

## Anti-goals (failure even if it "works")

- Recording a miss against someone who submitted on time, or who was on
  leave. This record goes into performance reviews; a false positive is
  the worst outcome. When in doubt, don't record.
- The live alert, the 16:00 recording, and a post-downtime backfill
  disagreeing about the same member and day.
- A task carried over three days counting as three tasks, or appearing
  twice (as today's todo and as "carried over").

## Approaches considered

**How "late counts as missed" is decided**

- *Snapshot at each cut-off* - record whoever is missing when the 11:00
  / 15:50 tick fires. Simple, but if the bot is down or restarting at
  the cut-off, the snapshot is taken at the next tick and anyone who
  submitted in between slips through; a backfill can't reconstruct it.
- *Compare submission timestamps (chosen)* - every `entries` row already
  has `created_at`. A miss is defined from timestamps vs the cut-off
  instant, so the alert, the 16:00 recording, and the backfill all
  compute the same answer whenever they run. Recording stays in one
  place (`day_summary_time`).

**When nobody could submit (bot down across a cut-off)**

While the bot is offline, slash commands fail, so a 10:30-11:30 outage
would otherwise record a `/todo` miss for everyone who hadn't submitted
by 10:30.

- *Accept it* - the lead fixes a bad day by marking everyone on leave,
  which distorts the Leave column and Expected.
- *Lead-run "day off" command* - covers outages and holidays, but is
  manual and after the fact. (The holiday half lives on in #35.)
- *Uptime tracking with an automatic waiver (chosen)* - the bot records
  when it was running and waives a cut-off whose submission window it
  wasn't fully up for. Automatic and deterministic; doesn't see Discord
  outages.

**How carry-over is made explicit**

- *Separate `/todo carryover` command* - extra surface for the same
  thing.
- *Auto-date todos added after the progress cut-off to the next working
  day* - one step for "plan tomorrow", but it would also silently move
  a member's late, missed todo for today onto tomorrow.
- *Optional `carryover:` option on `/todo add` that prefills the normal
  modal (chosen)* - same pattern as `/todo edit`'s prefilled modal and
  `/progress add`'s autocomplete; no new command, no special dating.

**Where the review report lives**

- *Extend `/missed`* - the user never uses it (the daily report already
  covers a single day) and it has no output section.
- *New `/team performance`, delete `/missed` (chosen)*.

## Decided behaviour

### Config

New `[schedule]` keys (config already rejects unknown keys):

```toml
# Deadline for the day's /todo. A todo submitted after this counts as
# missed. Members still missing one are @mentioned in the thread.
# todo_cutoff_time = "11:00"

# Deadline for /progress: every todo submitted before this needs at
# least one report submitted before this.
# progress_cutoff_time = "15:50"
```

Validation at load, hard error: `todo_time < todo_cutoff_time <=
progress_cutoff_time <= day_summary_time` and `update_time <
progress_cutoff_time`. (Recording runs at `day_summary_time`; a progress
cut-off after it would record misses before the deadline passed. The
reminder-to-cut-off spans are the uptime waiver's windows, so they must
be non-empty.)

### Miss rules (timestamp-based)

For member M on date D, with cut-off instants `T = D + todo_cutoff_time`
and `P = D + progress_cutoff_time` in `config.timezone`:

- **Missed /todo**: M has no `type='todo'` row for D with `created_at < T`.
  A second todo added after T is just an extra todo, not a miss.
- **Missed /progress**: M has at least one `type='todo'` row for D with
  `created_at < P` that has no `type='update'` row (`todo_id` = it) with
  `created_at < P`. Todos created after P need no report that day.
- Edits (`/todo edit`, `/progress edit`) change rows in place and keep
  `created_at`, so they never make something late. Delete + re-add after
  the cut-off does.
- Members with role `viewer` are never in scope (unchanged).
- Members on leave for D are never in scope (new).
- A waived cut-off (see "Bot downtime" below) records no misses of
  that kind for anyone on D.

These rules replace the current `members_missing_todo` /
`members_missing_update` semantics everywhere they are used: the 09:30 /
15:30 per-member nags, the new cut-off alerts, `record_missed` at
`day_summary_time`, and `backfill_missed` at startup. Leave also excludes
members from `members_with_no_activity` ("No submissions today").

### Cut-off alerts

Fire once per day when the tick reaches each cut-off (own
`reminders_sent` kinds, e.g. `todo_cutoff` / `progress_cutoff`), one
message in today's standup thread. Marked sent even when nobody is
missing (same as `day_summary_missing` after PR #34). Same skip rules as
the other reminders: weekends unless `run_on_weekends`, no thread = skip.

```
⛔ **/todo cut-off passed (11:00):** <@a> <@b> - no `/todo` submitted before the cut-off. This has been recorded. Submit it now with `/todo add`.
```
```
⛔ **/progress cut-off passed (15:50):** <@a> <@b> - you have a todo with no `/progress` report. This has been recorded. Submit it now with `/progress add`.
```

Times are the configured values, `HH:MM`.

If the cut-off is waived for the day, the alert posts this instead, with
no mentions (designer's call - the team should know why nobody was
called out):

```
⚠️ The bot was offline before the `/todo` cut-off (11:00), so today's `/todo` cut-off is waived. Please still submit with `/todo add`.
```

### Bot downtime across a cut-off

- **Uptime spans.** Each process start inserts a span row
  (`started_at`); every ticker tick bumps that row's `last_seen`.
- **"Up" at instant t** = some span has `started_at <= t <= last_seen +
  2 × ticker_interval_seconds`. The tolerance absorbs tick granularity,
  so a restart shorter than that (e.g. `/admin upgrade`) isn't downtime.
- **Waiver.** The `/todo` cut-off for D is waived if the bot was not up
  at every instant of `[D + todo_time, D + todo_cutoff_time]`; the
  `/progress` cut-off likewise over `[D + update_time, D +
  progress_cutoff_time]`. Downtime outside those windows (overnight, at
  08:00) doesn't waive anything.
- **Deterministic.** The waiver is computed from the stored spans, so the
  cut-off alert, the 16:00 recording, a leave re-derivation, and
  `backfill_missed` all agree. On rollout day, the windows before the
  first span count as downtime, so the first day's cut-offs are waived
  if the deploy lands inside them - the safe direction.
- **Backfill.** `backfill_missed` keeps its rule of skipping days with
  no entries at all, and applies the waiver like `record_missed`.
- **Pure core.** "Was the bot up for all of window W, given spans S and
  tolerance" is a pure function and gets the bulk of the tests.

### Morning rules block

Prepended to the existing `todo_reminder` message (same message, after
the mentions, before the standup line):

```
📌 **Standup rules**
• `/todo` and `/progress` are both required every day. Submitting one doesn't cover the other.
• Every todo needs its own `/progress` report.
• `/todo` cut-off: 11:00
• `/progress` cut-off: 15:50
• Anything not submitted by its cut-off is recorded and reflected in your performance review.
• On leave or sick? Tell your lead before 11:00.
• Unfinished from yesterday? Carry it over with `/todo add carryover:`.
📋 Time for today's standup! Submit your todo with `/todo add`.
```

### `/team leave`

A subcommand group under `/team`, gated like `/team remind` /
`skip-meeting` (`members::is_active_lead` - role `lead` only):

- `/team leave add member:<autocomplete> reason:<Leave|Sick> start:<date> end:<date>`
- `/team leave remove member:<autocomplete> start:<date> end:<date>`
- `/team leave list start:<date> end:<date>` - default: current month

Dates as `/recap`: none = today, one = that day, both = range
(`recap::resolve_day_or_range`, 60-day cap). Weekend days in a range are
skipped unless `run_on_weekends`. Storage is one row per member per day
with its reason; adding an existing day updates the reason.

On a member's leave day: no cut-off alert, no 09:30/15:30 nag, no "No
submissions today" mention, nothing recorded. They still get the
team-wide 09:00 and pre-meeting pings. Their submissions, if any, still
count as output.

Retroactive changes: whenever leave is added or removed for a day that
has already been recorded (`missed_recorded`), that member's
`missed_submissions` rows for the day are deleted and the day is
re-derived with the same rules (`record_missed` is deterministic from
timestamps + leave, and `INSERT OR IGNORE`). Removing leave can
therefore re-create misses; adding it clears them.

### Explicit carry-over

- `entries` gains `carried_from` (FK to the `todo` row it continues),
  unique when not null, so a todo can be carried forward once.
- `/todo add` gains an optional `carryover:` string option with
  autocomplete over the member's carry-over candidates. Picking one
  opens the normal add modal prefilled with the origin's task, notes,
  and SOW ref; submitting inserts today's todo with `carried_from` set.
  Without the option, `/todo add` is unchanged. The origin must belong
  to the member and still be a candidate when the modal is submitted.
- Carry-over candidates (`carryover_todos` / `carryover_count` /
  `carryover_report`, i.e. `/todo list`, `/team status` "+N carried",
  `/team report`) additionally exclude todos that have been carried
  forward. A carried task therefore appears once: as today's todo.
- `/progress add` autocomplete offers today's todos only (drop the merge
  with carry-over candidates). Reporting against a non-today todo is
  rejected; free text is still an unplanned report.
- Planning tomorrow after 15:50 needs no special case: add it today (no
  report needed, it's after P), carry it over tomorrow before 11:00.

### `/team performance`

Gated like `/recap` / `/team report` (`members::is_lead` - lead and
viewer). Ephemeral reply: short header plus a `.md` attachment,
`performance-<start>_<end>.md` (`recap::attachment_name`). Options
`start:` / `end:`: none = 1st of the current month to today, one = that
day, both = range; cap 366 days.

Rows: every non-viewer roster member, alphabetical.

```
# Team performance: Nov 1 – Nov 19

## Standup discipline
| Member | Expected | Leave | Sick | Missed /todo | Missed /progress | On time |
|--------|----------|-------|------|--------------|------------------|---------|
| Bima   | 12       | 2     | 0    | 2            | 3                | 75%     |

## Output
| Member | Tasks | Done | Blocked days | PRs merged | PRs open | Reviews | Sub-issues closed |
|--------|-------|------|--------------|------------|----------|---------|-------------------|
| Bima   | 18    | 13   | 3            | 9          | 2        | 14      | 6                 |

## Bima
- Leave: Nov 4, Nov 5
- Missed /todo: Nov 12, Nov 18
- Missed /progress: Nov 7, Nov 12, Nov 19
- PRs merged: api#210, api#214, web#88
- Reviewed: api#205, api#211
```

Definitions:

- **Expected**: days in range with a `missed_recorded` marker (the
  ritual actually ran and was recorded), on or after the member's
  *first-seen* date, minus their leave/sick days. First-seen = the
  earliest date with any entry, `missed_submissions` row, or leave row
  for that member. `record_missed` only writes rows for members on the
  roster at the time, so this is the first ritual day they were on the
  roster - no join-date field needed, and misses and Expected start
  from the same day. A member who never submits still gets misses from
  their first day, so they show 0% on time, not "—". (Designer's call.)
- **Leave / Sick**: their leave rows in range by reason.
- **Missed /todo / Missed /progress**: `missed_submissions` rows in
  range by kind (reuse `followups::missed_detail` for the per-member
  dates).
- **On time**: expected days with no miss of either kind, ÷ expected.
  "—" when expected is 0.
- **Tasks**: distinct carry-over chains (follow `carried_from` to the
  root) with at least one todo dated in range.
- **Done**: of those, chains with a `done` report on any of their todos.
- **Blocked days**: distinct dates in range with a `blocked` report by
  the member.
- **GitHub columns**: `github::report::team_activity` over
  `github::report::window(start, end, tz)`: `prs_merged`, `prs_opened`
  (still open at range end), `reviewed` (distinct PRs), and
  `sub_issues_closed`. The whole GitHub column set and per-person PR
  lines are omitted when `[github]` is not configured; a member with no
  `github_user` shows "—".
- If the range includes today and today isn't recorded yet, append the
  same note `/missed` shows today (`pending_today_note`, moved).
- If the range starts before the cut-off rules took effect, the header
  carries one line: misses before that date used the end-of-day rule.

### Retention

`maintenance` must stop pruning `reminders_sent` rows of type
`missed_recorded` (one row per day, tiny) - "Expected" depends on them
for ranges up to a year; today they're deleted after 90 days. Uptime
spans are pruned on the same 90-day rule as `reminders_sent` (only the
last 14 days are ever re-derived by the backfill).

### Removing `/missed`

Delete `src/discord/missed.rs`, its registration/dispatch in
`discord/mod.rs`, its `/help` entry, `format_missed_report_file`, and the
docs. Keep `missed_submissions`, `record_missed`, `backfill_missed`,
`missed_detail` (used by `/team performance`). Discord drops the
command on the next registration.

## Constraints

- DB logic stays in serenity-free modules with unit tests on a real
  temp-file DB (CLAUDE.md conventions); the Discord files only wire
  commands. The gateway paths can't be exercised in tests - confirm
  against a real guild.
- One new migration (`0009`): the leave table, the uptime span table,
  and `entries.carried_from`.
- Builds on PR #34 (`day_summary_missing` marked sent when empty) -
  merge that first.
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`,
  `cargo test` clean; CI coverage gate stays ≥ 30.
- Update `config.example.toml`, `docs/user-guide.md`, `/help`, `/todo
  help`, `/progress help`, and the CLAUDE.md layout notes.

## Left to the builder

- Table and column names for leave; `reminders_sent` kind names.
- How the cut-off instant is compared against `created_at` (text compare
  like `report::Window`, or parse). `created_at` is jiff's UTC RFC3339
  with variable fractional digits; a sub-second edge either way is
  acceptable.
- How the "rules took effect" date is stored (e.g. set once by migration
  0009) - only needs to be durable.
- Whether `resolve_day_or_range` grows default/cap parameters or gets a
  sibling for the month default.
- Thread sync wording for a carried todo (e.g. "carried over from Nov
  18") - optional.
- Viewer passed to `/team leave add`: reject with a clear message.
- Build order. Suggested: migration + config → uptime spans + waiver →
  timestamp rules + leave exclusion in the queries → cut-off alerts +
  rules block → `/team
  leave` → carry-over → `/team performance` + delete `/missed` +
  retention.

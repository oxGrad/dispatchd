# GitHub activity in the standup — design

**Date:** 2026-10-15
**Status:** Draft, open questions resolved, awaiting final review

## Summary

dispatchd pulls the team's GitHub activity (issues, PRs, reviews) from
the org via a **GitHub App**, stores a daily snapshot in SQLite next to
`entries`, and folds it into the two reports that already exist:

- the **daily day-summary** attachment in the standup thread
  (`day-summary-<date>.md`)
- the **biweekly `/recap`** attachment (`recap-<start>_<end>.md`)

It **complements** `/todo` + `/progress`, it does not replace them. GitHub
answers "what shipped"; the standup still owns plans, blockers and
non-code work (design, research, calls), which never shows up on GitHub.

The mapping that makes this work is the team's existing issue structure:

```
parent issue  "D.P.1.2 Payment gateway integration"   ← SOW deliverable
  └─ sub-issue  #201 "Stripe webhook handler"          ← a /todo
       └─ PR    #210 (linked via sidebar "Development") ← its /progress
```

Later phases also use it to **cut manual input**: `/todo add` can pick a
sub-issue (pre-fills task + SOW ref), `/progress add` can pick a PR, and
a merged PR closing a todo's issue marks that todo done automatically.

Fully opt-in: no `[github]` section in `config.toml` = feature off,
every report renders exactly as today.

## Goals / non-goals

**Goals**

- Stakeholder-ready biweekly view **per SOW deliverable**: what closed,
  what's in flight, what hasn't started.
- A per-day record of what each member shipped/reviewed, alongside what
  they planned and reported.
- Less typing for engineers (phases 4–5).

**Non-goals** (deliberate, see the discussion that led to this doc)

- No activity scoring, ranking or leaderboards. Per-member tables are
  sorted by name, never by volume.
- No "inactivity" alerts or nags driven by GitHub data. The only
  compliance nag stays the existing `day_summary_missing` message.
- No hidden views: everything rendered here lands in the team-visible
  standup thread or the existing lead-only `/recap`, same as today.
- No commit-level tracking. Commits are noisy, author-dated (forgeable),
  and the PR is already the unit of shipped work.
- No webhooks. dispatchd typically runs on a Pi behind NAT with no public
  endpoint; polling is enough at this team size.

## Decisions (confirmed)

| Topic | Decision |
|---|---|
| Relationship to standup | Alongside, inside dispatchd (same binary, DB, roster, ticker) |
| Member mapping | `github_user` (GitHub username) in `members.toml`, optional per member |
| Auth | GitHub App, read-only, installed on the org |
| PR ↔ issue link | Sidebar "Development" link (GraphQL `closingIssuesReferences`) |
| SOW ref source | Parent issue **label** `sow:D.P.a.b` first, **title** as fallback |
| SOW ref formats | Both `D.P.a.b` and legacy `M<a>D<b>`, treated as equivalent, displayed as `D.P.a.b` |
| Report delivery | Daily in the standup thread (unchanged); biweekly via new `/recap-biweekly`, ad-hoc via `/recap` |
| Date options | `/recap` and `/team github` share one rule: no dates = today, one date = that day, both = range |
| Auto-done vs `/missed` | A GitHub-auto-done todo does **not** record a missed `update` |
| Repo scope | Whatever repos the App installation covers; no repo list in config |

---

## Report changes

Examples below use a fictional team (Andi = designer, Budi / Sari / Rina
= engineers, org repos `api` and `web`). Real output is padded by
`status::render_aligned_table`, so columns line up in a fixed-width viewer.

Issue/PR references are written `<repo>#<number>` (`api#210`) because the
org has several repos. They're plain text, not markdown links: a link's
`[..](url)` syntax would wreck column alignment in the raw `.md` file.

### Daily day-summary (`day-summary-<date>.md`)

#### Before (today)

Discord message: `📊 **Day progress — 2026-10-15**` + attachment:

```
# 2026-10-15

| Member | Task                    | Status           | Progress                 | Blocker           |
|--------|-------------------------|------------------|--------------------------|-------------------|
| Sari   | Refund flow             | ⛔ blocked        | API returns 500 on retry | waiting on vendor |
| Andi   | Onboarding copy review  | ❌ no report yet  | —                        | —                 |
| Budi   | Stripe webhook handler  | ❌ no report yet  | —                        | —                 |
| Sari   | Webhook retries         | ⏳ in progress    | backoff done, tests left | —                 |
| Rina   | Fix flaky login test (unplanned) | ✅ done | fixed race in fixture    | —                 |
```

#### After

Discord message gains a one-line team headline (team totals only, no
per-person numbers in chat):

```
📊 **Day progress — 2026-10-15**
GitHub: 3 PRs merged · 1 sub-issue closed (D.P.1.2) · 4 reviews
```

Attachment:

```
# 2026-10-15

## Standup

| Member | Task                   | SOW     | Status            | Progress                 | Blocker           | GitHub                   |
|--------|------------------------|---------|-------------------|--------------------------|-------------------|--------------------------|
| Sari   | Refund flow            | D.P.1.2 | ⛔ blocked         | API returns 500 on retry | waiting on vendor | api#205 · PR api#219 open |
| Andi   | Onboarding copy review | —       | ❌ no report yet   | —                        | —                 | —                        |
| Sari   | Webhook retries        | D.P.1.2 | ⏳ in progress     | backoff done, tests left | —                 | api#206 · PR api#221 draft |
| Budi   | Stripe webhook handler | D.P.1.2 | ✅ done (GitHub)   | —                        | —                 | api#201 · PR api#210 merged |
| Rina   | Fix flaky login test (unplanned) | — | ✅ done       | fixed race in fixture    | —                 | PR web#222 merged        |

## GitHub work not on a todo

| Author | Item                                  | SOW     | Today    |
|--------|---------------------------------------|---------|----------|
| Budi   | PR api#224 Bump serde to 1.0.215      | —       | merged   |
| Sari   | Issue api#230 Webhook signature check | D.P.1.2 | opened   |

## Code reviews

| Reviewer | Reviewed                                   |
|----------|--------------------------------------------|
| Budi   | api#219 (changes requested)                |
| Rina   | api#210 (approved), api#221 (commented)    |
| Sari   | api#210 (approved)                         |
```

What changed, row by row:

1. **New `SOW` column.** The todo's `sow_ref` already exists in the DB but
   isn't shown in the day table today. It's normalised for display
   (`M1D2` → `D.P.1.2`); an unparseable free-text value is shown as typed.
2. **New `GitHub` column.** For a todo linked to an issue (phase 4's
   `/todo add issue:`): the issue plus each linked PR and its state
   (`open` / `draft` / `merged` / `closed`). For an unplanned
   `/progress` row whose task matches a PR picked from autocomplete
   (phase 4), that PR. Otherwise `—`. Until phase 4 ships, todos aren't
   linked yet, so this column fills in via the "not on a todo" table
   instead.
3. **`✅ done (GitHub)` status.** (Phase 5.) A todo with **no** manual
   report whose linked issue was closed as completed shows done, marked
   as GitHub-derived. A manual report always wins. If a member says
   `in progress` but the issue closed, the Status column keeps their
   word and the GitHub column shows the fact; nothing is overwritten.
   The row sorts into the `done` bucket accordingly.
4. **"GitHub work not on a todo"** (new section, omitted when empty). PRs
   merged/opened and issues opened/closed today by a roster member that
   don't match any of today's (or carried-over) todos. This is the
   "unplanned" bucket for GitHub, mirroring the existing `(unplanned)`
   rows. The `Author` column is the PR/issue author. Members without
   `github_user` (e.g. a designer) simply never appear here; nothing
   flags their absence.
5. **"Code reviews"** (new section, omitted when empty). Review work is
   real work that never appears in `/progress`. One row per **reviewer**
   (the roster member who submitted the review, not the PR author),
   listing the PRs they reviewed that day with their latest review state
   on each. Sorted by reviewer name.

"Today" means the configured `timezone`'s calendar day, applied to
GitHub's UTC timestamps (`merged_at`, `closed_at`, `submitted_at`, …).

### Biweekly `/recap` (`recap-<start>_<end>.md`)

#### Before (today)

`📅 **Recap (2026-10-01 to 2026-10-14)** - see attached.` + one day table
per day, nothing else:

```
# 2026-10-01

| Member | Task | Status | Progress | Blocker |
...

# 2026-10-02

| Member | Task | Status | Progress | Blocker |
...
```

#### After

Same Discord message. The attachment becomes a **stakeholder-facing,
person-free** report: deliverable progress only, no names anywhere. Who
did what is the lead's business, covered by `/team github`, `/team
report` and the daily summaries in the standup threads.

The per-day, per-person tables that make up today's whole `/recap` move
behind a new opt-in flag, so the default file is safe to forward as-is.

The command also splits in two:

```
/recap-biweekly [daily_log:<true|false>]
/recap [start:<date>] [end:<date>] [daily_log:<true|false>]
```

`daily_log` defaults to `false` on both. Both stay lead-only (`is_lead`),
same gate as today's `/recap`.

- **`/recap-biweekly`** is today's no-argument `/recap`: the 14 days
  ending yesterday (`DEFAULT_RECAP_DAYS`), no date options at all. This
  is the one you run every other week for the stakeholder.
- **`/recap`** switches to the same date rule as `/team github`:

  | `start` | `end` | Covers |
  |---|---|---|
  | — | — | today |
  | set | — | that day only |
  | — | set | that day only |
  | set | set | `start..end` inclusive, max 60 days |

  Behaviour change for existing users: `/recap` with no options used to
  mean "last 14 days" and now means "today"; `start` alone used to run to
  today and now means that single day. `docs/user-guide.md` and `/help`
  get updated in the same PR.
- `/missed` keeps its current date handling (no options = 14 days ending
  yesterday), unchanged by this design.
- Implementation: `recap::resolve_range` stays as-is for `/missed`; a new
  `recap::resolve_day_or_range` implements the table above (same date
  parsing, ordering check, 60-day cap and error strings), shared by
  `/recap` and `/team github`. `/recap-biweekly` calls `resolve_range(None,
  None, today)` directly.

```
# Recap 2026-10-01 → 2026-10-14

## Deliverables

| SOW     | Deliverable                 | Sub-issues        | PRs merged | Todos done |
|---------|-----------------------------|-------------------|------------|------------|
| D.P.1.2 | Payment gateway integration | 5/7 closed (+3)   | 9          | 11/13      |
| D.P.1.3 | Admin dashboard             | 2/6 closed (+2)   | 4          | 6/9        |
| D.P.2.1 | Mobile auth                 | 0/4 closed (+0)   | 0          | 1/2        |
| —       | Not linked to a deliverable | —                 | 3          | 8/10       |

### D.P.1.2 — Payment gateway integration (5/7 closed)

| Sub-issue                      | State          | PRs                            | Last activity              |
|--------------------------------|----------------|--------------------------------|----------------------------|
| api#205 Refund flow            | ⛔ blocked      | api#219 open                   | Oct 14 "waiting on vendor" |
| api#206 Webhook retries        | ⏳ in progress  | api#221 draft                  | Oct 14 PR updated          |
| api#207 Invoice PDF            | ⬜ not started  | —                              | —                          |
| api#201 Stripe webhook handler | ✅ closed       | api#210 merged                 | Oct 6 closed               |
| api#202 Payment intent API     | ✅ closed       | api#211 merged, api#214 merged | Oct 8 closed               |

Closed before this period: api#198, api#199

### D.P.1.3 — Admin dashboard (2/6 closed)
...

```

With `daily_log:true`, a `## Daily log` section is appended: one
`### <date>` per day with the same "Standup" / "GitHub work not on a
todo" / "Code reviews" tables as the daily summary (names included).
That's the lead's own working copy, not the one to forward.

How each section is computed:

- **Deliverables table.** One row per SOW ref that had *any* activity in
  the range: a todo with that `sow_ref`, or a PR/issue under a parent
  carrying it. `D.P.1.2` and `M1D2` collapse into one row.
  - *Deliverable* = the parent issue's title with the SOW ref stripped.
    Falls back to the ref alone for a legacy todo-only ref with no parent
    issue on GitHub.
  - *Sub-issues* = closed/total **as of `end`** (cumulative progress is
    what stakeholders ask about), with `(+N)` = closed *within* the
    range. Closed as "not planned" counts toward neither.
  - *PRs merged* = PRs merged in range that close one of its sub-issues.
  - *Todos done* = in-range todos with that SOW ref that ended `done`
    (manual or GitHub-derived).
  - The `—` row collects in-range work with no SOW ref (unlinked PRs,
    todos without `sow_ref`), so totals are honest.
  - Sorted by SOW ref (milestone, then deliverable), `—` last.
- **Per-deliverable breakdown.** One table per SOW row (not for `—`), every
  sub-issue of the parent, including untouched ones (`⬜ not started`
  is exactly what a stakeholder needs to see).
  - *State* = issue closed → `✅ closed`; else the latest manual
    `/progress` status on a todo linked to it (so `⛔ blocked` surfaces
    here); else any open PR → `⏳ in progress`; else `⬜ not started`.
  - Sorted by the same bucket order as the day tables (blocked first,
    closed last). Sub-issues closed before the range collapse into one
    trailing line.
  - No assignee/author/reviewer column, by design.
- **Daily log** (`daily_log:true` only). Exactly the daily summary's
  tables per day. Empty days are still skipped, as today.

With `[github]` unset, the day summary renders **exactly as today** (no
SOW column either; that addition ships with the GitHub column in phase
3). `/recap` without GitHub has no sub-issue data to build deliverables
from, so it keeps today's output (the per-day tables) regardless of
`daily_log`. The one change a non-GitHub install *does* see is the
`/recap` / `/recap-biweekly` split and the new date rule, which aren't
tied to GitHub.

### New command: `/team github`

The day-summary headline (`GitHub: 3 PRs merged · 1 sub-issue closed
(D.P.1.2) · 4 reviews`) broken down per person, on demand.

```
/team github [start:<YYYY-MM-DD>] [end:<YYYY-MM-DD>]
```

- Part of the `/team` group, so the same gate as `/team status` /
  `report`: `MANAGE_GUILD` plus the bot-side `members::is_lead` check
  (`lead` or `viewer`). Ephemeral reply, like `/team report`.
- **Range:** same rule as `/recap` (`recap::resolve_day_or_range`): no
  dates = today, `start` or `end` alone = that day only, both = the
  inclusive range, max 60 days.
- Plain chat message, not a file (six short lines fit easily under
  2000 chars; it's chunked via `status::split_into_messages` if a bigger
  roster ever overflows).

Example (`/team github`, today):

```
🐙 **GitHub — 2026-10-15**
Team: 3 PRs merged · 1 sub-issue closed (D.P.1.2) · 4 reviews

**Budi** — 2 PRs merged (api#210, api#224) · 1 sub-issue closed (api#201, D.P.1.2) · 1 review
**Rina** — 1 PR merged (web#222) · 2 reviews
**Sari** — 1 PR opened (api#219) · 1 issue opened (api#230) · 1 review
**Andi** — no GitHub account linked
```

Example (`/team github start:2026-10-01 end:2026-10-14`): over a range,
item lists are dropped (they'd blow the message size) and only counts
remain; `/recap` has the item-level detail.

```
🐙 **GitHub — 2026-10-01 to 2026-10-14**
Team: 16 PRs merged · 7 sub-issues closed (D.P.1.2 ×3, D.P.1.3 ×2, D.P.2.1 ×2) · 26 reviews

**Budi** — 7 PRs merged · 4 sub-issues closed (D.P.1.2 ×2, D.P.1.3 ×2) · 9 reviews
**Rina** — 5 PRs merged · 2 sub-issues closed (D.P.2.1 ×2) · 11 reviews
**Sari** — 4 PRs merged · 1 sub-issue closed (D.P.1.2) · 6 reviews · 2 PRs open
**Andi** — no GitHub account linked
```

Line rules:

- Members sorted by **name**, never by volume (non-goal: no ranking).
  `viewer`s are excluded, like the standup reminders.
- Each segment appears only when non-zero. A linked member with nothing
  in range shows `— no GitHub activity`; a member without `github_user`
  shows `— no GitHub account linked`. Both are stated plainly, not
  flagged or highlighted.
- Attribution: credit goes to **whoever did the work, never whoever
  clicked the button**. Who merged a PR or closed an issue isn't stored
  or shown at all; merging someone else's PR is a review-like act and
  is already credited via their review.
  - **PR merged / opened** → the PR's author, even when someone else
    merged it (opened = created in range and still open at the end of
    it).
  - **Sub-issue closed** (closed as completed, in range) → the author(s)
    of the merged PR(s) linked to it, not whoever closed it. If it was
    closed with no linked PR, its assignees. The SOW ref in parentheses
    is the parent's.
  - **Issue opened** → the issue's author.
  - **Review** → the reviewer; counts distinct PRs reviewed, not review
    submissions (three "comment" rounds on one PR = 1).
- **Team line** counts each PR / sub-issue / review once. It can be less
  than the sum of member lines when two members co-own one sub-issue.
  The day-summary headline is exactly this line for that day.

---

## Configuration

`config.toml`:

```toml
[github]
# GitHub App ID (from the App's settings page). Not a secret.
# app_id = 123456

# The org the App is installed on.
# org = "oxgrad"

# How often the ticker pulls new GitHub activity into the DB. Autocomplete
# and the reports read the DB, never GitHub directly.
# sync_interval_minutes = 10
```

`members.toml`, a new optional field per member:

```toml
[[members]]
discord_user_id = "000000000000000003"
name = "Senior Engineer's Name"
role = "senior"
github_user = "budi-dev"   # GitHub username; omit for non-GitHub members
```

`dispatchd validate` checks it's a syntactically valid GitHub username
and warns when `github_user` is set but `[github]` isn't (or vice versa).

Members are matched by GitHub's numeric **user id**, resolved once from
`github_user` on first sync and stored. A GitHub username rename then
doesn't break history. If the stored id stops matching the username, the
sync logs a warning and re-resolves.

## GitHub App

**Permissions (repository, all read-only):** Issues, Pull requests,
Metadata. Nothing at org level, no write access anywhere.

**Install:** on the org, "All repositories" or a chosen subset. That
choice *is* the repo scope: there's no repo allow/deny list in
`config.toml`, the bot simply sees what the installation sees. To leave a
repo (e.g. infra, docs) out of the reports, deselect it in the App's
installation settings.

**Private key:** the downloaded `.pem` is the only secret. It's stored
the same way as the Discord token:

```sh
sudo dispatchd github login --key ./dispatchd.2026-10-15.private-key.pem
```

- Reads `app_id` + `org` from config, mints an App JWT, calls
  `GET /app` and `GET /orgs/{org}/installation` to prove the key is right
  and the App is installed, then pipes the key into
  `systemd-creds encrypt --name=github_app_key --with-key=host` →
  `/etc/dispatchd/github_app_key.cred`.
- `dispatchd github logout` removes it (idempotent, like `discord logout`).
- `service install` adds
  `LoadCredentialEncrypted=github_app_key:/etc/dispatchd/github_app_key.cred`
  to the unit **only if that file exists**. Existing installs re-run
  `sudo dispatchd service install` once after `github login`.
- Dev/local fallback (outside systemd): `DISPATCHD_GITHUB_APP_KEY_PATH`.

**Runtime:** JWT (RS256, 10-min expiry) → installation access token (1 h)
via `POST /app/installations/{id}/access_tokens`, cached in memory and
refreshed ~5 min before expiry. New dependency: `jsonwebtoken`.
`reqwest` is already in the tree.

**Health:** `dispatchd status` and `/admin status` gain a line:

```
GitHub:   app 123456 installed on oxgrad · 14 repos · last sync 3 min ago · 182 ms
```

## Sync

A new ticker step, every `sync_interval_minutes`, independent of the
standup thread (like `maybe_record_missed`). A failed sync logs and
retries next interval; reports render from whatever's already stored.

Per run, GraphQL with the installation token, incremental from a stored
`updated_at` high-water mark (first run backfills 14 days, matching
`backfill_missed`):

1. `search(type: ISSUE, query: "org:<org> is:pr updated:>=<since>")`
   → each PR's repo/number/title/state/draft/author/created/merged/closed,
   `closingIssuesReferences` (the sidebar link), and `reviews` (author,
   state, submittedAt).
2. `search(... "org:<org> is:issue updated:>=<since>")` → each issue's
   state/stateReason/assignees/closedAt/labels/title and `parent`.
3. For every **parent** seen in 1–2 that carries a SOW ref: its full
   `subIssues` list with states, so the recap can show untouched
   sub-issues.
4. Resolve any new `github_user` → user id.

Only activity by roster members is attributed to anyone. Everything else
(external contributors, bots like dependabot) is stored for linking
purposes but never shown under a member.

Rate budget: an App installation gets ≥5,000 GraphQL points/hour. A
10-minute incremental sync for a 6-person team is a few dozen points.

## SOW ref parsing

One pure function, `sow::parse(&str) -> Option<SowRef>`, with
`SowRef { milestone: u32, deliverable: Option<u32> }` displaying as
`D.P.<m>` / `D.P.<m>.<d>`.

| Input | Parsed |
|---|---|
| `D.P.1.2` | m1 d2 |
| `D.P.1` | m1 (milestone only) |
| `M1D2` | m1 d2 |
| `M1` | m1 (milestone only, the format migration 0004 documented) |
| `d.p.1.2`, `m1d2` | case-insensitive, same as above |
| `Payment stuff` | `None`, kept as raw text, grouped under itself |

Where it's read from:

- **Parent issue label:** a label named `sow:<ref>`, e.g. `sow:D.P.1.2`
  (prefix case-insensitive, `<ref>` parsed per the table above, so
  `sow:M1D2` works too). Labels without the `sow:` prefix are ignored,
  so unrelated labels can never be mistaken for a SOW ref. If several
  `sow:` labels parse, the most specific wins (one with a deliverable
  over milestone-only), ties → lowest; the sync logs a warning. A
  `sow:` label that doesn't parse (e.g. `sow:TBD`) is ignored with a
  warning and the title fallback applies.
- **Parent issue title** (fallback): only the `D.P.` form, as a whole
  token anywhere in the title. The bare `M1` form is **not** searched in
  titles, to avoid false hits on words like "M2 Mac".
- **`entries.sow_ref`** (existing todos): both forms. Stored values are
  never rewritten (no data migration); normalisation happens at read
  time.

A sub-issue's SOW ref is its parent's. A sub-issue's own label/title is
not consulted, so one source of truth per deliverable.

## Data model

Migration `0008_github.sql`. All new tables are **retained forever**
(never pruned by `maintenance run`), same as `entries` and
`missed_submissions`: they're recap history.

```sql
ALTER TABLE members ADD COLUMN github_user TEXT;
ALTER TABLE members ADD COLUMN github_id   INTEGER;   -- resolved, stable

-- todo-only, like sow_ref/notes. Set by /todo add issue:<..> (phase 4).
ALTER TABLE entries ADD COLUMN github_issue_id TEXT;  -- GraphQL node id
-- update-only. Set by /progress add picking a PR (phase 4).
ALTER TABLE entries ADD COLUMN github_pr_id    TEXT;

CREATE TABLE github_issues (
    node_id        TEXT PRIMARY KEY,
    repo           TEXT NOT NULL,
    number         INTEGER NOT NULL,
    title          TEXT NOT NULL,
    state          TEXT NOT NULL,        -- 'open' | 'closed'
    state_reason   TEXT,                 -- 'completed' | 'not_planned' | ...
    author_id      INTEGER,
    created_at     TEXT NOT NULL,        -- UTC RFC3339, as GitHub sends it
    closed_at      TEXT,
    parent_node_id TEXT,                 -- sub-issue → parent
    sow_ref        TEXT,                 -- parsed+normalised, parents only
    updated_at     TEXT NOT NULL
);
CREATE TABLE github_issue_assignees (
    issue_node_id TEXT NOT NULL,
    github_id     INTEGER NOT NULL,
    PRIMARY KEY (issue_node_id, github_id)
);
CREATE TABLE github_prs (
    node_id    TEXT PRIMARY KEY,
    repo       TEXT NOT NULL,
    number     INTEGER NOT NULL,
    title      TEXT NOT NULL,
    state      TEXT NOT NULL,            -- 'open' | 'merged' | 'closed'
    is_draft   INTEGER NOT NULL,
    author_id  INTEGER,
    created_at TEXT NOT NULL,
    merged_at  TEXT,
    closed_at  TEXT,
    updated_at TEXT NOT NULL
);
CREATE TABLE github_pr_issues (              -- closingIssuesReferences
    pr_node_id    TEXT NOT NULL,
    issue_node_id TEXT NOT NULL,
    PRIMARY KEY (pr_node_id, issue_node_id)
);
CREATE TABLE github_reviews (
    node_id      TEXT PRIMARY KEY,
    pr_node_id   TEXT NOT NULL,
    author_id    INTEGER,
    state        TEXT NOT NULL,          -- 'approved' | 'changes_requested' | 'commented'
    submitted_at TEXT NOT NULL
);
CREATE TABLE github_sync_state (
    key   TEXT PRIMARY KEY,              -- 'pr_high_water' | 'issue_high_water'
    value TEXT NOT NULL
);
```

The sync upserts, so a PR that changes state (open → merged) is updated
in place; day attribution comes from its timestamps, not from when the
sync happened to see it.

## Code layout

Following the existing "DB logic has no serenity types" split:

| File | Contents |
|---|---|
| `src/sow.rs` | `SowRef`, `parse`, display/ordering. Pure |
| `src/github/mod.rs` | GraphQL response types + parsing into rows. Pure, tested against JSON fixtures in `tests/fixtures/github/` |
| `src/github/auth.rs` | JWT minting, installation token cache |
| `src/github/client.rs` | reqwest calls (the only untested-by-unit I/O, same as `upgrade.rs`'s) |
| `src/github/store.rs` | upserts + report queries (tempfile DB tests) |
| `src/github_login.rs` | `dispatchd github login/logout`, mirrors `discord_login.rs` (Linux-only `run`, non-Linux stub) |
| `src/recap.rs` | `RecapRow` gains `sow_ref` + `github: Vec<GithubRef>`; new `DeliverableSummary` (person-free); `format_recap_file` takes `daily_log: bool` |
| `src/discord/recap.rs` | `/recap` switches to `resolve_day_or_range`; new `/recap-biweekly` command (no date options); both get the optional `daily_log` boolean |
| `src/discord/ticker.rs` | sync step + headline line in `day_summary_content` |
| `src/discord/team.rs` | `github` subcommand: option parsing + reply; counting in `github::store::member_activity`, formatting in a pure `format_member_activity` |
| `src/discord/todo.rs`, `progress.rs` | phase 4 autocomplete |

## Phase 4–5 details (input side)

- **`/todo add issue:<autocomplete>`** (optional option): choices = open
  issues assigned to the member, from the DB, ≤25, sub-issues of a
  SOW-tagged parent first. Picking one opens the usual modal pre-filled
  with the issue title and the parent's SOW ref, and stores
  `github_issue_id` on the todo. Omitting it = today's blank modal.
- **`/progress add`** task autocomplete: today's/carried todos as now,
  plus the member's PRs updated today (`PR api#219 Refund flow`).
  Picking a PR that closes a todo's issue files the report **against
  that todo** (`todo_id` set) and pre-selects status (merged → done,
  else in progress). A PR matching no todo becomes an unplanned report
  with `github_pr_id` set.
- **Auto-done (phase 5):** the `✅ done (GitHub)` rule from the report
  section, plus: such a todo no longer counts as "still open" for
  carry-over, doesn't trigger the afternoon update nag, and does **not**
  record a missed `update` in `missed_submissions` (the merged PR is the
  report). `followups::members_missing_update` gains the same exclusion,
  so the live nag and `record_missed` stay in lockstep as they are today.
  The no-activity-at-all `day_summary_missing` message is unaffected
  (a todo was still submitted).

## Testing

- `sow::parse`: table-driven over every row of the parsing table.
- GraphQL parsing: recorded JSON fixtures (PR with sidebar-linked issue,
  draft PR, sub-issue with parent, review states, pagination cursor).
- Store + report queries: real tempfile DB (never `:memory:`), per the
  repo convention. Timezone bucketing at day boundaries (a PR merged
  23:30 UTC lands on the next local day in UTC+7).
- Report rendering: column alignment holds with the new columns, `—`
  handling, sections omitted when empty, and **byte-identical output to
  today when `[github]` is unset**.
- Not unit-testable (same rule as the Discord gateway and `upgrade.rs`):
  the live App auth + GraphQL calls. These need confirming end-to-end
  against the real org before each phase is called done.

## Rollout (one PR per phase)

1. **App plumbing.** `[github]` config, `github_user` in members,
   `github login/logout`, token minting, `service install` credential
   line, status line.
2. **Sync + storage.** Migration 0008, `sow.rs`, `github/store.rs`,
   ticker sync step.
3. **Reports.** Day summary + `/recap` changes above (incl. the
   `/recap-biweekly` split and the new date rule), plus `/team github`. This is the stakeholder value, and it needs no change
   in how engineers work.
4. **Autocomplete.** `/todo add issue:`, PRs in `/progress add`.
5. **Auto-done.** GitHub-derived done status, carry-over + nag effects.

Docs per phase: `docs/github-setup.md` (creating/installing the App,
mirrors `discord-setup.md`) in phase 1, `user-guide.md` updates in 3–4,
CLAUDE.md layout entries as modules land.

## Open questions

None. All resolved; see the Decisions table at the top.

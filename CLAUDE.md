# dispatchd

A Discord bot that automates a 6-person team's daily standup ritual
(`/todo`, `/progress`, `/team status`) and stores submissions in SQLite for
a later biweekly recap. Rust, `serenity` + `tokio` for Discord, `rusqlite`
for storage. Full requirements live in the original handover doc (ask the
user for it if you need the "why" behind a design choice — it's not
checked into this repo).

## Build, test, lint

```sh
cargo build                    # debug build
cargo build --release          # release build - also worth checking after
                                # dependency changes, since it exercises the
                                # full serenity/tokio tree under optimization
cargo test                     # all unit tests
cargo clippy --all-targets     # must be clean, no warnings
cargo fmt                      # apply formatting
cargo fmt --check              # verify formatting without changing files
```

All four should be clean before considering a change done - still worth
running locally before pushing, for a fast inner loop. CI is the shared
oxHive pipeline (`oxHive/pipelines`, pinned `@v2`, a floating major tag),
consumed by two thin workflows here.
`rust-check`, `rust-audit`, and `rust-verify-version` are composite actions
under `oxHive/pipelines/.github/actions/*` now (not reusable workflows), so
each runs as a step inside a plain `runs-on: ubuntu-latest` job rather than
a job-level `uses:`:

- `.github/workflows/pull-request.yml` (every PR + push to `main`) runs
  `rust-check` (`cargo fmt --check`, `cargo clippy -- -D warnings`, and
  `cargo tarpaulin` coverage gated at `fail-under-coverage: 30` - see
  below) and `rust-audit` (`rustsec/audit-check`). Note the pipeline's
  clippy is not `--all-targets`, so warnings in `#[cfg(test)]` code only
  fail your local `cargo clippy --all-targets`, not CI - keep running it.
- `.github/workflows/release.yml` (on a `v*` tag) runs the same
  `rust-verify-version` -> `rust-check` -> `rust-audit` gates, then a
  bespoke binary build (static musl for x86_64/aarch64/armv7 + macOS
  arm64, `SHA256SUMS` + `SHA256SUMS.minisig`, GitHub Release). The
  signature uses the `MINISIGN_SECRET_KEY` Actions secret, whose public
  half is the committed `minisign.pub` (embedded into the binary;
  `dispatchd upgrade` refuses an unsigned or mis-signed release). The
  secret key also lives at `~/.minisign/dispatchd.key` on the
  maintainer's machine - rotating it means a new `minisign.pub`, and a
  binary built with the old key can't verify releases signed with the new
  one. `rust-build-binaries` (also now a
  composite action) is deliberately not used - it's glibc-only and drops
  armv7 (Raspberry Pi), and dispatchd is not a crates.io crate so
  `rust-publish-crates` is skipped too. See `docs/installing.md`.

Coverage sits around 35% (measured 2026-09-01): the DB-layer modules are
~fully covered, the `src/discord/*` serenity code is near zero because it
can't be exercised without a live gateway (see the testing notes below).
`fail-under-coverage: 30` guards the tested core against regression
without demanding the impossible - raise it as real tests grow.

`build.rs` bakes the git short-SHA and an "is this an exact tag" flag
into compile-time env vars (`DISPATCHD_GIT_SHA`, `DISPATCHD_IS_TAGGED`,
and the composed `DISPATCHD_VERSION` that `--version` prints); it degrades
to a clean version string when `git` isn't available (e.g. `cross` in the
release workflow's container).

## Release tooling

- `just` (`.justfile` + `recipes/*.just`, all gitignored-scratch-aware):
  `just check` runs the fast local loop (`cargo fmt --check`, `cargo
  clippy --all-targets -D warnings`, `cargo test`, `cargo build
  --release`) - a superset of the pipeline's clippy, but it doesn't
  re-run coverage or the audit; `just release-patch`
  (`-minor`, `-major`) wraps `cargo release`; `just testenv-init` /
  `testenv-run` exercise the binary against an isolated
  `XDG_CONFIG_HOME`/`XDG_DATA_HOME` under `.testenv/` so testing never
  touches your real `~/.config/dispatchd` or the real DB.
- `release.toml` (`cargo-release`): bumps `Cargo.toml`/`Cargo.lock`,
  commits `chore: release v{version}`, tags `v{version}`, and pushes -
  which is what fires `release.yml`. Not a crates.io crate (`publish =
  false`). Dry-run by default; `--execute` to actually do it.
- `cliff.toml` (`git-cliff`): conventional-commits changelog config,
  commit URLs point at `github.com/oxgrad/dispatchd`. Existing history
  is not conventional-commits, so a generated changelog only picks up
  `feat:`/`fix:`/etc. commits going forward.
- `.github/dependabot.yml`: weekly `cargo` + `github-actions` bumps.
- `.markdownlint.json`: line-length rule (MD013) off for `docs/`.
- `.cargo/audit.toml`: `cargo audit` (the CI `rust-audit` job) ignore
  list. Currently holds the `rustls-webpki 0.102.8` and `rustls 0.22.4`
  advisories pulled in transitively via `serenity` -> `tokio-tungstenite
  0.21` -> `rustls 0.22`; unbumpable until serenity ships a release on
  `rustls 0.23+`, and `RUSTSEC-2026-0459` (`libsqlite3-sys 0.37`), blocked
  on `rusqlite_migration` supporting `rusqlite 0.41`.
  The file documents why each is safe to ignore - revisit when serenity
  or `rusqlite_migration` updates.

## Running it

```sh
dispatchd init      # writes commented-out config.toml + members.toml
                     # templates to their resolved locations (XDG or the
                     # DISPATCHD_*_PATH env vars below), never overwrites
dispatchd validate   # parses config.toml and members.toml and reports
                     # errors (e.g. an invalid members.toml role) without
                     # opening the DB, taking the singleton lock, or
                     # connecting to Discord - unlike the bare run below,
                     # which does all three
dispatchd discord login   # (Linux, root) prompts for the bot token (hidden
                     # input), validates it against Discord, then pipes it
                     # into `systemd-creds encrypt --with-key=host` and
                     # writes /etc/dispatchd/discord_token.cred - dispatchd
                     # runs the encryption itself. See docs/discord-setup.md.
dispatchd discord logout  # (Linux, root) removes /etc/dispatchd/discord_token.cred.
                     # Idempotent - a missing credential is not an error.
dispatchd github login --key <file.pem>   # (Linux, root) optional GitHub
                     # integration: validates the GitHub App private key
                     # against config.toml's [github] app_id/org (JWT ->
                     # /app -> org installation -> installation token ->
                     # repo count), encrypts it via `systemd-creds encrypt
                     # --name=github_app_key` to
                     # /etc/dispatchd/github_app_key.cred, and patches the
                     # installed unit's LoadCredentialEncrypted= line in.
                     # See docs/github-setup.md.
dispatchd github logout   # removes that line from the unit first (a unit
                     # pointing at a missing credential won't start), then
                     # the .cred. Idempotent.
dispatchd github preview [--days N]   # runs the GitHub sync's own queries
                     # once (default 1 day back, max 14) and prints totals +
                     # a per-GitHub-user breakdown - no DB writes. The way
                     # to check the integration end to end from the CLI.
dispatchd            # loads config, opens/migrates the DB, replaces the
                     # members table with members.toml (no file = empty
                     # roster - removed members lose access), prints a status block, then connects to
                     # Discord if configured (see below) - otherwise exits 0
                     # after the status block, which is the normal state
                     # during initial setup
dispatchd service install   # Linux only: writes/enables a systemd unit at
                     # /etc/systemd/system/dispatchd.service (needs root -
                     # sudo dispatchd service install). See docs/discord-setup.md.
                     # Also installs+starts dispatchd-maintenance.timer, which
                     # runs `dispatchd maintenance run` weekly.
dispatchd maintenance run   # the handover doc's weekly cron: prunes
                     # reminders_sent/followups_sent rows older than 90 days
                     # (except missed_recorded markers) and VACUUMs. entries is never pruned - it's the
                     # retained history the biweekly recap depends on.
dispatchd status     # reports the systemd side (version, unit
                     # installed/enabled/active, upgrade helper installed,
                     # encrypted token present) and the Discord side
                     # (resolves a token the same way the bot itself would,
                     # pings Discord's API, reports round-trip latency).
dispatchd upgrade    # resolves the latest GitHub release; if newer,
                     # downloads the matching prebuilt, verifies its
                     # SHA256SUMS minisign signature (minisign.pub)
                     # and SHA-256 against it, atomically
                     # swaps the binary, restarts dispatchd. Flags: --check
                     # (report current vs latest, do nothing), --no-restart
                     # (swap only, print the restart command), --version
                     # <tag> (install any tag, incl. older = downgrade).
                     # Needs root. Also the engine behind Discord's
                     # /admin upgrade, via the hidden `upgrade --from-request`
                     # helper mode that dispatchd-upgrade.service runs.
dispatchd --version  # prints the crate version (from Cargo.toml, baked
                     # in at compile time by build.rs) - useful for
                     # confirming what a `curl | sh` install actually
                     # fetched. Non-tagged builds (local, CI, a `main`
                     # install) also get the short git SHA appended
                     # ("0.1.0 (abc1234)"); a build made from an exact
                     # version tag stays just "0.1.0".
```

Config file: `$XDG_CONFIG_HOME/dispatchd/config.toml` (`~/.config/dispatchd/config.toml`
by default). Every key is optional; see `config.example.toml` for the
full set with defaults documented inline.

Env vars (all `DISPATCHD_*`, overriding the XDG-resolved path/value):
- `DISPATCHD_CONFIG_PATH`, `DISPATCHD_DB_PATH`, `DISPATCHD_MEMBERS_PATH`
- `DISPATCHD_DISCORD_TOKEN` - the bot token, never put in `config.toml`
  (it's a secret; `discord_guild_id`/`discord_standup_channel_id` in
  `config.toml` are not secrets and live there instead). This is the
  fallback checked only when no `systemd-creds`-encrypted credential is
  found (see `dispatchd discord login` above) - useful for local/dev use
  outside systemd.
- `DISPATCHD_GITHUB_APP_KEY_PATH` - path to the GitHub App's plaintext
  `.pem`, the same dev/local fallback for the GitHub App key (the bot
  prefers `$CREDENTIALS_DIRECTORY/github_app_key`, see `github login`).

Discord application/bot setup (creating the app, intents, invite link,
getting guild/channel IDs) is in `docs/discord-setup.md` - don't duplicate
it here. How a team member actually uses the bot day-to-day (`/todo`,
`/progress`, `/team status` / `report` / `remind`, the reminder/follow-up
timeline) is in `docs/user-guide.md` - point new engineers there rather
than the setup doc. Creating/installing the optional GitHub App is in
`docs/github-setup.md`; the GitHub integration's full design (phases 0-5)
is `docs/superpowers/specs/2026-10-15-github-activity-design.md`.
Installing the `dispatchd` binary itself (prebuilt releases via
`curl | sh`, no Rust toolchain needed, generated by `oxGrad/get` from
this repo's release contract - see `.github/workflows/release.yml`) is
in `docs/installing.md`.

## A note on live Discord testing

Every Discord-facing feature so far (`/ping`, `/todo`, `/progress`,
`/team status`, `/team report`, `/team remind`) has been written without
ever exercising a live gateway connection or a real interaction - the
sandbox this was originally built in has its egress proxy configured to
block `discord.com` outright
(confirmed via the proxy's own status endpoint, not assumed). **That
restriction is specific to that sandbox, not a property of this project** -
if you have real network access, actually try connecting and running the
commands rather than assuming the same limitation applies to you. Get a
bot token + guild set up per `docs/discord-setup.md` and confirm end-to-end
before treating a Discord-facing change as verified.

Everything DB-layer (`src/db.rs`, `src/entries.rs`, `src/members.rs`,
`src/status.rs`) and the pure helpers in `src/discord/progress.rs`
(`status_code`, `encode_task_for_modal`, `parse_custom_id`,
`parse_edit_custom_id`, `summarize`, `format_progress_list`, etc.),
`src/discord/admin.rs` (`permission_denied_reply`, `version_line`,
`format_admin_status`, `render_steps`), `src/upgrade.rs` (`Version`,
`sha256_for`, `verify_sha256`, `parse_latest_tag`, `parse_status`,
`RequestGuard` cleanup, the request/status serde), and the
`format_status` / `format_ping` renderers in `service.rs` /
`discord_login.rs` are fully unit-tested and don't have this limitation.
The reqwest/tar I/O in `upgrade.rs` (`download_and_stage`, `check`) and
the `--from-request` helper flow are not - they need a real GitHub
release and a systemd host, same "confirm end-to-end yourself" rule as
the gateway code. Same for the GitHub App calls in `github/client.rs`
(`Client`, `check`): their response parsing and error mapping are
unit-tested, the requests themselves need a real App installed on a real
org (the original sandbox's proxy blocks `api.github.com/app` and doesn't
let reqwest's bundled roots verify it, so it couldn't be exercised there).

## Testing conventions worth preserving

- **Shared env-var lock**: any test that mutates a `DISPATCHD_*` (or
  `XDG_CONFIG_HOME`) env var must take `crate::test_support::ENV_LOCK`
  (defined in `main.rs`) for the duration of the mutation. `cargo test`
  runs tests in parallel within one process by default, and these vars
  are read by `Config::default`/`load` and the roster/init path
  resolvers - without the shared lock, two such tests can race each other
  even if they live in different files. Pure functions (`Config::from_raw`,
  the `status`/`update` helpers, etc.) never read the environment, so
  they're unaffected and don't need the lock.
- **DB-backed tests use a real file, never `:memory:`**: `db::open` sets
  `PRAGMA journal_mode = WAL`, which SQLite silently ignores for an
  in-memory database. Tests open a fresh `tempfile::tempdir()` path
  instead (see any `open_test_db` helper in `entries.rs`/`status.rs` for
  the pattern) so the pragma actually takes effect and migrations behave
  the same as in production.
- Each Discord command's DB-touching logic lives in its own module
  (`entries.rs`, `members.rs`, `status.rs`) with no `serenity` types in
  sight, kept separate from the `src/discord/*.rs` files that build
  commands/modals and dispatch interactions. New commands should follow
  the same split so their DB logic stays unit-testable without a live
  connection.

## Project layout

```
src/
  main.rs        CLI entry point, Config/DB/seed wiring, Discord gating
  config.rs      Config struct, XDG lookup + env-var overrides, TOML merge;
                 carryover_lookback_days ([carryover] lookback_days,
                 default 7, 0 disables) - how far back /progress add +
                 /team look for unfinished todos; github
                 (Option<GithubConfig> from [github] app_id + org +
                 sync_interval_minutes - both app_id and org or neither,
                 one alone is an error; None = integration off);
                 [schedule] todo_cutoff_time (11:00) / progress_cutoff_time
                 (15:50), validated at load: todo_time < todo_cutoff_time
                 <= progress_cutoff_time <= day_summary_time and
                 update_time < progress_cutoff_time
  db/            SQLite connection + embedded migrations (0005 adds
                 members.is_admin, 0006 adds missed_submissions - never
                 pruned by maintenance.rs, same as entries; 0007 indexes
                 entries by (date, discord_user_id) and todo_id; 0008 adds
                 members.github_user/github_id, entries.github_issue_id/
                 github_pr_id, and the github_* activity tables - also
                 never pruned; 0009 adds member_leave, uptime_spans and
                 app_meta - all never pruned - plus entries.carried_from
                 with a unique partial index)
  entries.rs     todo/update row DB logic, incl. entries.sow_ref - a
                 purely informational, unvalidated cross-reference into
                 an external scope-of-work doc (e.g. "M1D2"), todo-only.
                 update rows: insert_update (append), plus list_updates /
                 update_for_edit / update_update backing /progress
                 list|edit (edit revises status/progress/blocker in place,
                 leaving task + the todo_id link alone).
                 Carry-over query helpers: carryover_todos /
                 carryover_count / carryover_report - `type='todo'` rows
                 from the last N days with no `done` update, feeding
                 /progress add autocomplete, /team status ("+N carried"),
                 /team report ("Carried over" block) and /todo list.
                 short_date formats an origin date ("2026-08-27" ->
                 "Aug 27")
  members.rs     roster seeding (seed replaces the whole table in one
                 transaction - members.toml is the source of truth;
                 optional github_user is validated - GitHub's username
                 rules, unique case-insensitively - and seeded into
                 members.github_user; a resolved members.github_id is
                 carried across the reseed while github_user is unchanged,
                 so a GitHub rename can't orphan or misattribute history) +
                 is_member (gates /todo + /progress in discord/mod.rs) +
                 is_lead/is_active_lead checks +
                 all_member_ids + roster/name_of (used by /team remind's
                 member autocomplete). role (`lead | designer | senior |
                 medior | junior | viewer`) and is_admin (a separate
                 members.toml bool) are fully decoupled - is_admin no
                 longer implies is_lead, they're set independently.
                 is_lead admits `lead` and `viewer` (gates /team
                 status/report); is_active_lead admits `lead` only (gates
                 /team remind/skip-meeting, since a viewer shouldn't
                 manage the team, only watch it). all_member_ids excludes
                 `viewer` (they're not pinged by the standup reminders),
                 as does followups::members_missing_todo (never nagged to
                 submit). load_and_validate is the shared read+parse+role-check
                 (no DB access), reused by seed (writes the rows) and
                 validate (just the checks - backs `dispatchd validate`)
  status.rs      /team status DB queries + formatting (team_status /
                 format_status_line), incl. each member's deduped sow_ref
                 tag list appended to their line; also team_report /
                 format_report / split_into_messages - the full per-member
                 detail for /team report and its 2000-char message
                 chunking. Also render_aligned_table - a shared GFM-table
                 renderer that pads every column to its widest cell,
                 meant for a `.md` file attachment (alignment only reads
                 right in a fixed-width viewer, not a chat message);
                 used by recap::format_day_table_file and
                 performance::format_performance_file. team_status /
                 team_report take
                 carryover_lookback_days; MemberStatus gains
                 carried_count, MemberReport gains carried
                 (Vec<CarryoverDetail>); matched_update_count is now
                 scoped to today's todo ids so a carry-over update
                 doesn't inflate it
  reminders.rs   reminders_sent/daily_threads DB logic (the ticker's state)
  followups.rs   followups_sent DB logic (missing-todo/update nags), plus
                 members_with_no_activity - no todo AND no update at all
                 today, feeding the ticker's day_summary_missing message
                 rather than the per-kind todo/update follow-ups above.
                 Also missed_submissions logic: record_missed (writes one
                 row per member per kind for a date - 'todo' from
                 members_missing_todo, 'update' from members_missing_update
                 reused directly from the live update_followup nag - a
                 todo left with no report against it, not a blanket
                 zero-updates day, and correctly silent for a member with
                 no todo at all, since that's already the 'todo' miss -
                 called once daily by the ticker alongside day_summary;
                 writes the missed_recorded marker itself, in the same
                 transaction) + backfill_missed (run once at ticker
                 startup: records past unrecorded days within 14 days
                 that had any entry - covers the bot being down at
                 day_summary_time until after midnight)
                 + rederive_member (re-judges one member's recorded days
                 after a leave change). Misses are judged against cut-off
                 instants, not end of day: members_missing_todo /
                 members_missing_update compare entries.created_at
                 (RFC3339 text) with the day's cut-off, skip members on
                 leave (member_leave), and record_missed takes the day's
                 DayCutoffs so a kind waived for bot downtime writes no
                 row)
  recap.rs       resolve_day_or_range (`/recap` start/end validation:
                 none = today, one = that day, both = range, capped at
                 60 days) + resolve_month_or_range (same, but none = the
                 1st of the month to today; `/team performance`, capped at
                 366 days) + month_bounds (the whole current month,
                 `/team leave list`'s no-dates default) +
                 biweekly_range (`/recap-biweekly`'s
                 fixed 14 days ending yesterday) + recap_range/day_rows
                 (per-day todo+latest-update rows, bucketed
                 blocked/no-report/in-progress/done, plus ad-hoc updates
                 as "(unplanned)" rows) + format_day_table_file (one
                 day's rows as a column-aligned GFM table via
                 status::render_aligned_table, for a `.md` attachment)
                 + format_recap_file (joins one table per day for
                 `/recap`, `None` for an empty range). RecapRow also
                 carries sow_ref/github_issue_id/github_pr_id; day_recap
                 (one day's DayRecap) + format_standup_table (the
                 GitHub-mode table: adds SOW + GitHub columns) feed
                 github/report.rs - the classic formatters are untouched
  init.rs        `dispatchd init` subcommand
  sow.rs         SowRef + parse (D.P.a.b and legacy M<a>D<b>, both
                 displayed D.P.a.b), from_labels (`sow:` labels only, most
                 specific wins) and from_title (D.P. form only - "M2" in a
                 title must not match); normalize (display form of a
                 stored sow_ref, free text kept as typed) and
                 strip_from_title
  github/        GitHub App integration (design phases 1-3: auth, sync,
                 reports; todo/progress linking is phase 4).
                 auth.rs - AppKey (PKCS#1/PKCS#8 PEM -> ring RsaKeyPair) +
                 app_jwt (RS256, signed with ring directly: JWT crates' RSA
                 backends mean aws-lc-rs or the `rsa` crate's open RustSec
                 advisory) + InstallationToken::needs_refresh. client.rs -
                 the REST calls (GET /app, /orgs/{org}/installation, POST
                 .../access_tokens, /installation/repositories,
                 /users/{login}), all through one `request` helper, and
                 graphql() -> GraphqlResponse (parse_graphql tolerates
                 node-level FORBIDDEN/NOT_FOUND errors - an item outside
                 the installation, a deleted id - and reports them as
                 `skipped`; anything else fails the call, error text capped
                 at 3 quoted messages), pure parse_* / describe_status,
                 check(). graphql.rs - the four queries (PR search, issue
                 search, issues by id, sub-issues; each defines only the
                 fragments it uses, since GitHub rejects unused ones; an
                 issue is selected with two levels of ancestors, the top
                 one with its own `parent { id }` so a riding-along
                 ancestor never overwrites its link upward) +
                 pure parsers into Issue/PullRequest/Review rows, each
                 Issue tagged with its Origin (SearchHit / Parent / ById /
                 SubIssue); Page::high_water comes from search hits only,
                 Page::resume_from is where the next page's search starts;
                 SubIssueList::complete says whether a deliverable's
                 child list can prove a child was removed; SUB_ISSUE_BATCH
                 = 20 keeps that query under GitHub's 500k-node limit.
                 store.rs - write_batch (one transaction per run: deduped
                 upserts, unlinking children a complete sub-issue list no
                 longer has, then the high-water marks), the
                 github_sync_state bookkeeping (raise_high_water never
                 lowers; record_sync_error caps the stored error at 300
                 chars for /admin status), members_needing_github_id,
                 sync_summary. sync.rs - TokenSource (cached installation
                 token, checked before every request; owns the one HTTP
                 client), collect (the whole fetch pipeline, no DB: one
                 generic fetch_search for both searches, each page a new
                 search from the last one's timestamp - next_position - so
                 mid-run updates can't shift results across a page and
                 GitHub's 1,000-result cap doesn't apply; PR-linked issues
                 by id; sub-issues only for
                 deliverables behind *fresh* rows - parents_to_expand -
                 so the 10-min overlap doesn't re-expand forever),
                 sync_once (collect + write_batch + member id resolution,
                 where one member's failed lookup is logged and skipped;
                 DB lock never held across a request), run_loop (spawned
                 by discord::spawn_github_sync, next to but independent of
                 the ticker; a missing/broken key records "sync not
                 running" for /admin status), preview/format_preview
                 (`github preview` - the same `collect`, counting only
                 deduplicated search hits). report.rs - everything that
                 reads the synced data, pure DB + formatting (tests in
                 report/tests.rs, built on the design doc's example team):
                 window (local days in config.timezone -> a UTC [lo, hi)
                 compared as RFC3339 text); team_activity/team_github
                 (`/team github`, credit to the PR author, a sub-issue to
                 its merged PRs' authors else its assignees, roster-only:
                 viewers/bots/outsiders never count; items listed for one
                 day, counts for a range) + headline (the day summary's
                 names-free team line); day_github (the "GitHub work not on
                 a todo" + "Code reviews" sections) + github_cells (the
                 standup table's GitHub column) + day_summary (headline +
                 file for the ticker); deliverables/recap_file (the
                 person-free per-SOW `/recap` report, include_member_detail
                 appends per-day tables). mod.rs - app_key_pem(), resolved like
                 discord_token. testdata/ holds throwaway test-only keys
  github_login.rs `dispatchd github login/logout` + the GitHub section of
                 `dispatchd status` and `/admin status` (ping_report ->
                 GithubPing, format_github_ping) - same shape as
                 discord_login.rs, whose encrypt_secret it reuses
  discord_login.rs `dispatchd discord login` - prompts, validates against
                 Discord (Http::get_current_user), then shells out to
                 `systemd-creds encrypt` to persist the token; also
                 `dispatchd status`'s Discord-ping half (decrypts the
                 credential directly, since that's a one-off invocation
                 outside systemd's own LoadCredentialEncrypted= decoding).
                 `run()` (login) is `#[cfg(target_os = "linux")]` with a
                 non-Linux stub that bails, same split as `service.rs` -
                 systemd-creds is Linux-only, and the macOS release build
                 has to compile. `ping`/`logout`/`decrypt_cred_file` stay
                 cross-platform (used by `dispatchd status`). `ping_report()`
                 returns a `DiscordPing` struct and `format_ping()` renders
                 it - the same pair `/admin status` reuses (`ping()` is now
                 just `print!(format_ping(ping_report()))`)
  lock.rs        single-instance guard (`std::fs::File::try_lock`), held
                 for the process's lifetime; used by the main run (`<db>.lock`,
                 so two processes never race the ticker), `maintenance run`
                 (`<db>.maintenance.lock` - deliberately separate, since the
                 maintenance timer is meant to run concurrently with the
                 main service, not be blocked by it), `init`
                 (`<config>.lock`), and `service install`
                 (`/etc/dispatchd/service-install.lock`) - each guards only
                 against a second instance of that same subcommand
  maintenance.rs `dispatchd maintenance run` DB logic (weekly prune + VACUUM);
                 keeps reminders_sent rows of type missed_recorded, since
                 /team performance's "Expected" days depend on them
  cutoffs.rs     Rules (from Config: tz, the four times, uptime tolerance
                 of 2 x ticker_interval_seconds), instant (local time ->
                 UTC Timestamp), uptime spans (start_span at ticker
                 startup, touch_span every tick; never pruned) + covered /
                 day_cutoffs (a cut-off is waived when the spans leave a
                 gap in its own window: todo_time -> todo_cutoff for
                 /todo, update_time -> progress_cutoff for /progress),
                 ensure_rules_since / rules_since / rules_apply (app_meta
                 'cutoff_rules_since': the first day the cut-off rules
                 apply - set once, to today, or tomorrow if today's
                 todo_reminder or missed_recorded already went out. Before
                 it day_cutoffs returns the old end-of-day rule (no
                 cut-off, nothing waived) for every caller, the ticker
                 posts no cut-off alert, and the performance report adds
                 its old-rule note)
  leave.rs       member_leave DB logic: per-member leave/sick days
                 (ritual days only; weekends skipped unless
                 run_on_weekends), add/remove/list - backs /team leave
  performance.rs /team performance data + rendering, serenity-free:
                 performance(conn, start, end, github) -> one
                 MemberPerformance per non-viewer member (expected days =
                 missed_recorded days from the member's first activity
                 minus leave/sick; on-time; missed todo/progress dates by
                 discord_user_id; tasks/done/blocked days; optional GitHub
                 activity via github::report::team_activity) and
                 format_performance_file (aligned discipline + output
                 tables, per-member date lists, an old-rule note when the
                 range starts before rules_since)
  upgrade.rs     self-upgrade. Pure helpers (`Version` compare, `sha256_for`
                 / `verify_sha256`, `parse_latest_tag`, `Request` /
                 `StatusLine` serde, `parse_status`) + reqwest/flate2/tar
                 I/O (`fetch_latest_tag`, `check`, `download_and_stage`,
                 `install_staged` - atomic rename over the running binary)
                 + the `dispatchd upgrade` CLI (`run`). `--from-request` is
                 the root-helper mode `dispatchd-upgrade.service` runs: it
                 reads /run/dispatchd/upgrade.request, streams `StatusLine`s
                 to /run/dispatchd/upgrade.status, and always deletes the
                 request (a `RequestGuard` Drop impl) so the `.path` unit
                 re-arms. DISPATCHD_TARGET (baked by build.rs) picks the
                 release asset
  service.rs     `dispatchd service install` (systemd unit, Linux only,
                 requires systemd >= 250 for LoadCredentialEncrypted=;
                 refuses User=root and a binary/dir the service user could
                 modify, since the root upgrade helper executes it; the bot
                 + maintenance units get NoNewPrivileges/PrivateTmp/
                 ProtectSystem=full) and
                 `dispatchd status`'s systemd-side checks. `install()` also
                 writes dispatchd-upgrade.service (root oneshot) +
                 dispatchd-upgrade.path (watches upgrade.request) and adds
                 RuntimeDirectory=dispatchd (+ Preserve=yes) to the main
                 unit - the writable /run/dispatchd the unprivileged bot
                 needs for /admin upgrade, kept across the upgrade restart.
                 `status_report()` returns a `ServiceStatus` struct and
                 `format_status()` renders it - the pair `/admin status`
                 reuses; `status()` is now just their composition. The
                 unit loads the GitHub App key (GITHUB_CRED_PATH) only
                 when that file exists - set_github_credential adds/removes
                 the line (pure, idempotent), sync_github_credential
                 patches an installed unit + daemon-reloads (github
                 login/logout call it), since systemd won't start a unit
                 whose LoadCredentialEncrypted= file is missing
  discord/       serenity Handler, one file per slash command
    mod.rs         EventHandler impl, interaction dispatch, shared helpers
                    (modal_value, get_option_string, get_option_bool,
                    is_unknown_channel_error
                    - the last moved here from ticker.rs, now shared by
                    ticker + team), spawns the ticker alongside the client;
                    calls admin::post_upgrade_confirmation on `ready`.
                    Handler carries carryover_lookback_days, passed to
                    /team status/report, /progress autocomplete, and
                    /todo list
    help.rs        /help - static overview of every command
    admin.rs       the `/admin` command group (members with role=admin):
                    `status` - `dispatchd status`'s systemd + Discord health
                    (via service::status_report / discord_login::ping_report)
                    plus a current-vs-latest version line from
                    upgrade::check; `upgrade` - writes upgrade.request, then
                    tails upgrade.status editing the ephemeral per step for
                    up to 120s, stopping at `Restarting`;
                    post_upgrade_confirmation (run once on `ready`) reads
                    the last terminal StatusLine and, if it carries a
                    non-empty channel_id, posts the cross-restart
                    `✅ dispatchd upgraded ...` message, then clears both
                    files. MANAGE_GUILD-gated + bot-side members::is_admin
                    check per subcommand, same dual gate as `/team`
    todo.rs        /todo add|edit|delete|list|help - add/edit share
                    one modal shape (edit's pre-filled with current
                    values, now incl. an optional SOW Ref field); edit/
                    delete/list all operate on any of today's todos (not
                    just open ones - see entries::list_todos vs
                    list_open_todos); delete is blocked by
                    entries.todo_id's FOREIGN KEY (surfaced as a friendly
                    reply, not a raw DB error) if a /progress report
                    already references the todo. list also appends a
                    read-only "Carried over (still open)" section
                    (format_todo_list) and chunks its reply past 2000
                    chars like /team report (status::split_into_messages);
                    edit/delete ignore carried-over todos. `add` takes an
                    optional `carryover:` (autocomplete over
                    entries::carryover_todos) that opens the modal
                    prefilled and inserts the todo with carried_from set
                    (entries::carry_over_todo - one carry per origin todo)
    progress.rs    /progress add|edit|list|help - `add` (was the flat
                    /progress command) opens the report modal and always
                    inserts a new 'update' row; `edit` (report autocomplete
                    over entries::list_updates + optional status override,
                    "keep current" when omitted) revises one report row in
                    place via entries::update_update and does NOT re-sync
                    to the thread (same one-way rule as /todo edit); `list`
                    is the member's own reports for today. Two modal
                    prefixes: MODAL_PREFIX (add) and EDIT_MODAL_PREFIX
                    (edit, custom_id carries the resolved status + row id).
                    "progress" is the command name, not the 'update' DB row
                    type - entries.type/reminders_sent.type/etc. keep that
                    name, it's the data concept not the command surface.
                    add's autocomplete unions today's open todos with
                    carryover_todos (merge_task_choices, deduped, cap 25).
                    A todo is a carry-over candidate only until it has
                    been carried (entries.carried_from)
    team.rs        the `/team` command group (was team_status.rs):
                    `status` - the old standalone summary command moved
                    here verbatim, one line per member showing who's
                    updated today; `report` - full per-member todo +
                    progress detail, tech-lead-only, ephemeral, split
                    across follow-up messages past 2000 chars (see
                    status::split_into_messages); `remind` - manual
                    tech-lead nudge that @-mentions one member in today's
                    standup thread to submit a /todo or /progress,
                    independent of the automated followup nags; and
                    `skip-meeting` - cancels today's meeting (fixed
                    unmentioned note into the thread + marks meeting_skip
                    and meeting_reminder sent). The parent command is
                    MANAGE_GUILD-gated and every subcommand also does a
                    bot-side members::is_lead check. `send_reminder` and
                    `skip-meeting` share `post_to_standup_thread`, and the
                    module shares is_unknown_channel_error with ticker.rs
                    (via mod.rs). `github` (handle_github) - per-member
                    GitHub activity via github::report::team_github, same
                    dates as /recap, is_lead-gated, ephemeral + chunked;
                    replies "not configured" when [github] is off;
                    `leave add|remove|list` (leave.rs; remove/add
                    re-derive recorded days via followups::rederive_member;
                    list defaults to the whole month, recap::month_bounds,
                    and is chunked); `performance` (handle_performance ->
                    build_performance) - is_lead-gated, ephemeral reply
                    with a `performance-<start>_<end>.md` attachment from
                    performance::format_performance_file, no dates = this
                    month (resolve_month_or_range, 366 days), "No team
                    members to report on." for an empty roster, and
                    pending_today_note (is_ritual_day) when the range
                    includes a today whose missed_recorded marker isn't
                    written yet
    ticker.rs      creates today's thread early (thread_creation_time,
                    default 08:30) ahead of the todo/progress reminders
                    and the pre-meeting ping (fires at meeting_time minus
                    meeting_reminder_lead_minutes, @mentions everyone),
                    then
                    every tick posts any new /todo|/progress submission
                    into it (entries::entries_since + a per-thread sync
                    cursor on daily_threads) since those commands' own
                    replies are ephemeral - the only way the team sees
                    each other's activity; a todo's sync message includes
                    its sow_ref tag (the progress/update one doesn't) and
                    its notes as a blockquote line; a progress sync message
                    quotes the writeup (and the blocker, if any) below the
                    header (SyncEntry.progress); the thread is named
                    "Standup: <date>"; gives up (marks sent/advances the
                    cursor, doesn't
                    retry) on a deleted standup thread instead of
                    retrying every tick; also posts a day_summary at
                    day_summary_time (default 16:00, the same moment
                    meeting_time defaults to) - a short header plus a
                    column-aligned `.md` file attachment (day-summary-
                    <date>.md), the same per-day table /recap renders
                    (recap::recap_range + recap::format_day_table_file)
                    for just today; a file rather than chunked messages
                    since an attachment isn't bound by Discord's 2000-char
                    cap and the columns actually line up
                    (day_summary_content builds the (header, Some(file))
                    pair, or (header, None) when there's no activity yet).
                    With [github] configured, github::report::day_summary
                    builds it instead (github_day_summary_content): the
                    header gains the team headline, the file the SOW/GitHub
                    columns and the two GitHub sections.
                    Fires on the clock like every other entry here, not
                    once everyone's
                    actually submitted - a still-open todo just shows "no
                    report yet". Right after it, a separate
                    day_summary_missing message (own reminders_sent kind,
                    so one failing never blocks the other) @-mentions every
                    member from followups::members_with_no_activity - no
                    todo AND no update at all today, not just a missing
                    update against an existing todo like the followups
                    below - and is skipped entirely (not just left silent,
                    ticker.rs's missing_submissions_message returns None)
                    when nobody qualifies; deliberately blunt wording
                    ("you have not submitted ... This is required daily")
                    since it's the ritual's compliance nag, not a
                    heads-up. Also at day_summary_time, independently of
                    both posts above (its own reminders_sent kind,
                    missed_recorded, and no dependency on the standup
                    thread existing at all): maybe_record_missed calls
                    followups::record_missed to persist the day's misses
                    into missed_submissions, feeding /team performance.
                    The morning ping opens with the standing rules block
                    (standup_message: both cut-offs, leave, carry-over);
                    at todo_cutoff_time and progress_cutoff_time
                    (maybe_fire_cutoff_alert, own reminders_sent kinds
                    todo_cutoff / progress_cutoff) the thread gets an
                    alert @-mentioning whoever is still missing it
                    (cutoff_alert_message), or a no-mention waiver notice
                    when the bot's uptime had a gap before that cut-off.
                    Startup records an uptime span, touched every tick
    recap.rs       /recap start:<date> end:<date> (none = today, one =
                    that day, both = range - recap::resolve_day_or_range)
                    and /recap-biweekly (no options, recap::biweekly_range
                    - what a bare /recap meant before) - tech-lead-only,
                    both share `respond`. Reply shape: a
                    short header (range_label / range_phrase collapse a
                    single day to just the date; "No activity on/between
                    ..." when empty) plus, when there's anything to show, a
                    `recap-<start>_<end>.md` attachment (`recap-<date>.md`
                    for a single day - attachment_name, shared with
                    /team performance)
                    (recap::format_recap_file - one column-aligned day
                    table per day, recap::recap_range) - no chunking, an
                    attachment isn't bound by Discord's 2000-char cap.
                    With [github] configured (Shape::Github) the file is
                    github::report::recap_file instead - per-deliverable,
                    no names - and both commands' include_member_detail
                    bool appends the per-day tables
```

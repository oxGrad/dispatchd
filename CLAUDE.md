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
oxHive reusable pipeline (`oxHive/pipelines`, pinned `@v2`), consumed by
two thin workflows here:

- `.github/workflows/pull-request.yml` (every PR + push to `main`) calls
  `rust-check.yml` (`cargo fmt --check`, `cargo clippy -- -D warnings`,
  and `cargo tarpaulin` coverage gated at `fail-under-coverage: 30` - see
  below) and `rust-audit.yml` (`rustsec/audit-check`). Note the pipeline's
  clippy is not `--all-targets`, so warnings in `#[cfg(test)]` code only
  fail your local `cargo clippy --all-targets`, not CI - keep running it.
- `.github/workflows/release.yml` (on a `v*` tag) runs the same
  `rust-verify-version` -> `rust-check` -> `rust-audit` gates, then a
  bespoke binary build (static musl for x86_64/aarch64/armv7 + macOS
  arm64, `SHA256SUMS`, GitHub Release). `rust-build-binaries.yml` is
  deliberately not used - it's glibc-only and drops armv7 (Raspberry Pi),
  and dispatchd is not a crates.io crate so `rust-publish-crates` is
  skipped too. See `docs/installing.md`.

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
  list. Currently holds four `rustls-webpki 0.102.8` advisories pulled in
  transitively via `serenity` -> `tokio-tungstenite 0.21` -> `rustls
  0.22`; unbumpable until serenity ships a release on `rustls 0.23+`.
  The file documents why each is safe to ignore - revisit when serenity
  updates.

## Running it

```sh
dispatchd init      # writes commented-out config.toml + members.toml
                     # templates to their resolved locations (XDG or the
                     # DISPATCHD_*_PATH env vars below), never overwrites
dispatchd discord login   # (Linux, root) prompts for the bot token (hidden
                     # input), validates it against Discord, then pipes it
                     # into `systemd-creds encrypt --with-key=host` and
                     # writes /etc/dispatchd/discord_token.cred - dispatchd
                     # runs the encryption itself. See docs/discord-setup.md.
dispatchd discord logout  # (Linux, root) removes /etc/dispatchd/discord_token.cred.
                     # Idempotent - a missing credential is not an error.
dispatchd            # loads config, opens/migrates the DB, seeds members.toml
                     # if present, prints a status block, then connects to
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
                     # and VACUUMs. entries is never pruned - it's the
                     # retained history the biweekly recap depends on.
dispatchd status     # reports the systemd side (version, unit
                     # installed/enabled/active, upgrade helper installed,
                     # encrypted token present) and the Discord side
                     # (resolves a token the same way the bot itself would,
                     # pings Discord's API, reports round-trip latency).
dispatchd upgrade    # resolves the latest GitHub release; if newer,
                     # downloads the matching prebuilt, verifies its
                     # SHA-256 against the release SHA256SUMS, atomically
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

Discord application/bot setup (creating the app, intents, invite link,
getting guild/channel IDs) is in `docs/discord-setup.md` - don't duplicate
it here. How a team member actually uses the bot day-to-day (`/todo`,
`/progress`, `/team status` / `report` / `remind`, the reminder/follow-up
timeline) is in `docs/user-guide.md` - point new engineers there rather
than the setup doc.
Installing the `dispatchd` binary itself (prebuilt releases via
`curl | sh`, no Rust toolchain needed - see `install.sh` and
`.github/workflows/release.yml`) is in `docs/installing.md`.

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
the gateway code.

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
  config.rs      Config struct, XDG lookup + env-var overrides, TOML merge
  db/            SQLite connection + embedded migrations (0005 adds
                 members.is_admin)
  entries.rs     todo/update row DB logic, incl. entries.sow_ref - a
                 purely informational, unvalidated cross-reference into
                 an external scope-of-work doc (e.g. "M1D2"), todo-only.
                 update rows: insert_update (append), plus list_updates /
                 update_for_edit / update_update backing /progress
                 list|edit (edit revises status/progress/blocker in place,
                 leaving task + the todo_id link alone)
  members.rs     roster seeding + is_lead check + all_member_ids +
                 roster/name_of (used by /team remind's member autocomplete).
                 Role `admin` is a superset of `lead`: seeding it sets both
                 is_admin=1 and is_lead=1, so every is_lead gate admits
                 admins unchanged; is_admin() is the /admin-only check
  status.rs      /team status DB queries + formatting (team_status /
                 format_status_line), incl. each member's deduped sow_ref
                 tag list appended to their line; also team_report /
                 format_report / split_into_messages - the full per-member
                 detail for /team report and its 2000-char message chunking
  reminders.rs   reminders_sent/daily_threads DB logic (the ticker's state)
  followups.rs   followups_sent DB logic (missing-todo/update nags)
  init.rs        `dispatchd init` subcommand
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
  maintenance.rs `dispatchd maintenance run` DB logic (weekly prune + VACUUM)
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
                 requires systemd >= 250 for LoadCredentialEncrypted=) and
                 `dispatchd status`'s systemd-side checks. `install()` also
                 writes dispatchd-upgrade.service (root oneshot) +
                 dispatchd-upgrade.path (watches upgrade.request) and adds
                 RuntimeDirectory=dispatchd (+ Preserve=yes) to the main
                 unit - the writable /run/dispatchd the unprivileged bot
                 needs for /admin upgrade, kept across the upgrade restart.
                 `status_report()` returns a `ServiceStatus` struct and
                 `format_status()` renders it - the pair `/admin status`
                 reuses; `status()` is now just their composition
  discord/       serenity Handler, one file per slash command
    mod.rs         EventHandler impl, interaction dispatch, shared helpers
                    (modal_value, get_option_string, is_unknown_channel_error
                    - the last moved here from ticker.rs, now shared by
                    ticker + team), spawns the ticker alongside the client;
                    calls admin::post_upgrade_confirmation on `ready`
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
                    already references the todo
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
                    name, it's the data concept not the command surface
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
                    (via mod.rs)
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
                    retrying every tick
```

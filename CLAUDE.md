# dispatchd

A Discord bot that automates a 6-person team's daily standup ritual
(`/todo`, `/progress`, `/team-status`) and stores submissions in SQLite for
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

All four should be clean before considering a change done. There is no
CI configured yet - these are run locally/by-hand each time.

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
                     # installed/enabled/active, encrypted token present)
                     # and the Discord side (resolves a token the same way
                     # the bot itself would, pings Discord's API, reports
                     # round-trip latency).
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
`/progress`, `/team-status`, the reminder/follow-up timeline) is in
`docs/user-guide.md` - point new engineers there rather than the setup doc.

## A note on live Discord testing

Every Discord-facing feature so far (`/ping`, `/todo`, `/progress`,
`/team-status`) has been written without ever exercising a live gateway
connection or a real interaction - the sandbox this was originally built
in has its egress proxy configured to block `discord.com` outright
(confirmed via the proxy's own status endpoint, not assumed). **That
restriction is specific to that sandbox, not a property of this project** -
if you have real network access, actually try connecting and running the
commands rather than assuming the same limitation applies to you. Get a
bot token + guild set up per `docs/discord-setup.md` and confirm end-to-end
before treating a Discord-facing change as verified.

Everything DB-layer (`src/db.rs`, `src/entries.rs`, `src/members.rs`,
`src/status.rs`) and the pure encode/decode helpers in
`src/discord/progress.rs` (`status_code`, `encode_task_for_modal`,
`parse_custom_id`, etc.) are fully unit-tested and don't have this
limitation.

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
  db/            SQLite connection + embedded migrations
  entries.rs     todo/update row DB logic
  members.rs     roster seeding + is_lead check + all_member_ids
  status.rs      /team-status DB queries + formatting
  reminders.rs   reminders_sent/daily_threads DB logic (the ticker's state)
  followups.rs   followups_sent DB logic (missing-todo/update nags)
  init.rs        `dispatchd init` subcommand
  discord_login.rs `dispatchd discord login` - prompts, validates against
                 Discord (Http::get_current_user), then shells out to
                 `systemd-creds encrypt` to persist the token; also
                 `dispatchd status`'s Discord-ping half (decrypts the
                 credential directly, since that's a one-off invocation
                 outside systemd's own LoadCredentialEncrypted= decoding)
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
  service.rs     `dispatchd service install` (systemd unit, Linux only,
                 requires systemd >= 250 for LoadCredentialEncrypted=) and
                 `dispatchd status`'s systemd-side checks
  discord/       serenity Handler, one file per slash command
    mod.rs         EventHandler impl, interaction dispatch, shared helpers
                    (modal_value, get_option_string), spawns the ticker
                    alongside the client
    help.rs        /help - static overview of every command
    todo.rs        /todo create|edit|delete|list|help - create/edit share
                    one modal shape (edit's pre-filled with current
                    values); edit/delete/list all operate on any of
                    today's todos (not just open ones - see
                    entries::list_todos vs list_open_todos); delete is
                    blocked by entries.todo_id's FOREIGN KEY (surfaced as
                    a friendly reply, not a raw DB error) if a /progress
                    report already references the todo
    progress.rs    /progress (autocomplete + modal custom_id encoding) -
                    named for the report it submits, not the underlying
                    'update' DB row type (entries.type/reminders_sent.type/
                    etc. keep that name - it's the data concept, not the
                    command surface)
    team_status.rs /team-status
    ticker.rs      daily standup thread creation + 9am/3pm/4pm reminders;
                    gives up (marks sent, doesn't retry) on a deleted
                    standup thread instead of retrying every tick
```

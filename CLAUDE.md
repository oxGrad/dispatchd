# dispatchd

A Discord bot that automates a 6-person team's daily standup ritual
(`/todo`, `/update`, `/team-status`) and stores submissions in SQLite for
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
```

Config file: `$XDG_CONFIG_HOME/dispatchd/config.toml` (`~/.config/dispatchd/config.toml`
by default). Every key is optional; see `config.example.toml` for the
full set with defaults documented inline.

Env vars (all `DISPATCHD_*`, overriding the XDG-resolved path/value):
- `DISPATCHD_CONFIG_PATH`, `DISPATCHD_DB_PATH`, `DISPATCHD_MEMBERS_PATH`
- `DISPATCHD_DISCORD_TOKEN` - the bot token, never put in `config.toml`
  (it's a secret; `discord_guild_id`/`discord_standup_channel_id` in
  `config.toml` are not secrets and live there instead)

Discord application/bot setup (creating the app, intents, invite link,
getting guild/channel IDs) is in `docs/discord-setup.md` - don't duplicate
it here.

## A note on live Discord testing

Every Discord-facing feature so far (`/ping`, `/todo`, `/update`,
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
`src/discord/update.rs` (`status_code`, `encode_task_for_modal`,
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
  lock.rs        single-instance guard (flock on `<db_path>.lock`), held
                 for the process's lifetime so two dispatchd processes
                 never race the ticker against the same data directory
  maintenance.rs `dispatchd maintenance run` DB logic (weekly prune + VACUUM)
  service.rs     `dispatchd service install` (systemd unit, Linux only)
  discord/       serenity Handler, one file per slash command
    mod.rs         EventHandler impl, interaction dispatch, shared helpers,
                    spawns the ticker alongside the client
    todo.rs        /todo
    update.rs      /update (autocomplete + modal custom_id encoding)
    team_status.rs /team-status
    ticker.rs      daily standup thread creation + 9am/3pm/4pm reminders;
                    gives up (marks sent, doesn't retry) on a deleted
                    standup thread instead of retrying every tick
```

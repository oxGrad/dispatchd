# dispatchd

A Discord bot that runs a small team's daily standup ritual and keeps the
submissions for a later recap. Every weekday it opens a dated thread in
your standup channel, prompts everyone for their todos in the morning and
their progress in the afternoon, nags whoever forgets, and stores it all
in SQLite so the tech lead can pull a biweekly recap from real data
instead of memory.

Built for a fixed 6-person team (one lead, one designer, four engineers),
Rust, running as a systemd service on Linux - a Raspberry Pi is a fine
host.

## Install

```sh
curl -fsSL https://dispatchd.graditya.com | sudo sh
```

Downloads the prebuilt binary for your platform (static `musl` on Linux
x86_64/aarch64/armv7, macOS Apple Silicon), verifies its checksum, and
installs it to `/usr/local/bin` (so `sudo dispatchd ...` and the systemd
unit can find it). No Rust toolchain needed. For a local install without
`sudo`, plus version pinning and the `INSTALL_DIR` override, see
[`docs/installing.md`](docs/installing.md).

To build from source instead: `cargo build --release`.

## Quick start

```sh
dispatchd init      # write config.toml + members.toml templates (never overwrites)
# edit members.toml with your team's Discord user IDs
# create the Discord app, get a token + guild/channel IDs (see docs below)
dispatchd           # run in the foreground; connects to Discord if configured,
                    # otherwise prints a status block and exits 0
```

For the supported production setup - a systemd unit with the bot token
encrypted at rest via `systemd-creds` - see
[`docs/discord-setup.md`](docs/discord-setup.md):

```sh
sudo dispatchd service install   # writes + enables the systemd unit (systemd >= 250)
sudo dispatchd discord login     # prompts for the token, validates it, encrypts it
sudo systemctl start dispatchd
dispatchd status                 # unit state + a live Discord ping
```

## Commands

### In Discord

| Command | Who | What |
| --- | --- | --- |
| `/todo add\|edit\|delete\|list\|help` | everyone | manage your todos for today; optional free-text SOW reference tag |
| `/progress add\|edit\|list\|help` | everyone | report progress (Done / In Progress / Blocked) against a todo or ad-hoc work; `edit` fixes a report, `list` shows today's |
| `/team status` | tech lead | one line per member: how many of today's todos have a progress report |
| `/team report` | tech lead | full detail: everyone's todos, notes, SOW refs, and progress reports for today |
| `/team remind` | tech lead | post a reminder to one member in today's thread to submit a todo / progress update |
| `/team skip-meeting` | tech lead | cancel today's meeting and post a "no meeting today" note to the thread |
| `/help`, `/ping` | everyone | command overview; liveness check |

`/todo` and `/progress` replies are ephemeral - dispatchd separately
mirrors each submission into the day's thread so the team sees the
activity. Full walkthrough in [`docs/user-guide.md`](docs/user-guide.md).

### CLI

| Command | What |
| --- | --- |
| `dispatchd` | load config, open/migrate the DB, seed the roster, print status, connect to Discord if configured |
| `dispatchd init` | write commented `config.toml` + `members.toml` templates to their resolved locations |
| `dispatchd discord login` / `logout` | Linux, root: manage the `systemd-creds`-encrypted bot token |
| `dispatchd service install` | Linux, root: install/enable the systemd unit + weekly maintenance timer |
| `dispatchd maintenance run` | prune old reminder/followup rows and VACUUM (the retained `entries` history is never touched) |
| `dispatchd status` | report the systemd side and the Discord side (token resolution + API ping) |
| `dispatchd --version` | crate version, plus the git short-SHA on non-tagged builds |

## Configuration

- **`config.toml`** - `$XDG_CONFIG_HOME/dispatchd/config.toml` by default.
  Every key is optional; see
  [`config.example.toml`](config.example.toml) for the full set with
  defaults documented inline (timezone, schedule times, ticker interval,
  follow-up delays, `discord_guild_id` / `discord_standup_channel_id`).
- **`members.toml`** - `$XDG_CONFIG_HOME/dispatchd/members.toml` by
  default. The team roster, seeded into the DB on startup and updated in
  place on later runs. See
  [`members.example.toml`](members.example.toml).
- **Environment overrides** - `DISPATCHD_CONFIG_PATH`, `DISPATCHD_DB_PATH`,
  `DISPATCHD_MEMBERS_PATH`, and `DISPATCHD_DISCORD_TOKEN` (the token is a
  secret and never belongs in `config.toml`; it's the fallback used when
  no encrypted credential is present).

## Documentation

| Doc | For |
| --- | --- |
| [`docs/installing.md`](docs/installing.md) | installing the binary (prebuilt releases, `curl \| sh`, version pinning, hosting the installer) |
| [`docs/discord-setup.md`](docs/discord-setup.md) | creating the Discord app, intents, invite link, guild/channel IDs, the systemd + encrypted-token deployment |
| [`docs/user-guide.md`](docs/user-guide.md) | how a team member uses the bot day to day - the ritual timeline and every slash command |
| [`CLAUDE.md`](CLAUDE.md) | architecture, module layout, build/test/CI, and testing conventions |

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets     # must be clean
cargo fmt --check
```

Or with [`just`](https://github.com/casey/just): `just check` runs all
four. CI is the shared [`oxHive/pipelines`](https://github.com/oxHive/pipelines)
reusable workflows (`rust-check`, `rust-audit`) on every PR and push to
`main`; releases fire on a `v*` tag. Releases are cut with
`cargo release` (`just release-patch` / `-minor` / `-major`).

See [`CLAUDE.md`](CLAUDE.md) for the module layout and why the DB logic is
kept separate from the `serenity` code (so it stays unit-testable without
a live gateway).

## License

Apache-2.0 - see [`LICENSE`](LICENSE).

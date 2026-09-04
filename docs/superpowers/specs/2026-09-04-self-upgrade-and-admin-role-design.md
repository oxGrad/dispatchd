# Self-upgrade + `admin` role — design

**Date:** 2026-09-04
**Status:** Approved, ready for implementation plan

## Summary

Add the ability for dispatchd to upgrade its own binary, both from the CLI
and from Discord, and add an `admin` role that is a strict superset of the
tech lead.

- **`dispatchd upgrade`** — a new CLI subcommand. Resolves the latest
  GitHub release, and if newer than the running version, downloads the
  right prebuilt binary, verifies its SHA-256, swaps it in place, and
  restarts `dispatchd.service`. Flags: `--check` (report only),
  `--no-restart` (swap only), `--version <tag>` (pin / downgrade).
- **`admin` role** in `members.toml` — a new valid role. An admin gets
  every tech-lead capability (all of `/team`) plus a new `/admin` command
  group.
- **`/admin status`** — ephemeral; systemd + Discord health (what
  `dispatchd status` prints) plus a current-vs-latest version line.
- **`/admin upgrade`** — triggers a self-upgrade from Discord, streaming
  per-step progress into the ephemeral reply. Because a successful upgrade
  restarts the bot, the freshly-started instance posts a non-ephemeral
  confirmation into the channel the request came from.
- The Discord path needs root (write `/usr/local/bin/dispatchd`,
  `systemctl restart`). The bot stays **unprivileged**; a systemd
  path-activated root helper (`dispatchd-upgrade.service` +
  `dispatchd-upgrade.path`) does the privileged work, installed by
  `dispatchd service install`.

Schema change: one migration adding `members.is_admin`.

## Motivation

- Upgrading today means SSHing to the host and re-running the `curl | sh`
  installer. The admin wants to do it from Discord on a small
  single-tenant VM / Pi.
- "Check for a new version" has no first-class surface at all.
- The tech lead is currently the only privileged role. The person who
  *operates* the bot (owns the host, the Discord app) is a distinct
  concern from the person who *runs standup*, and only the operator
  should be able to trigger an upgrade.

## Decisions (resolved during brainstorming)

1. **Fetch mechanism: native Rust.** Not shelling out to `install.sh`.
   Hit the GitHub releases API, download the tarball + `SHA256SUMS`,
   verify, extract, atomic-rename over the running binary.
2. **HTTP client: reuse `reqwest`** (already a transitive dependency via
   `serenity`). `ureq` is the fallback if Cargo feature unification with
   serenity's `reqwest` turns out to require conflicting features.
3. **Privilege bridge: unprivileged bot + systemd path-activated root
   helper.** Not "run the whole bot as root", not sudoers, not polkit.
4. **Bridge units ship in `dispatchd service install`,** not
   `install.sh`. `install.sh` stays version-agnostic and unit-free.
5. **Success signal across the restart: the new instance posts a
   non-ephemeral confirmation** to the requesting channel. Absence of
   that message is the failure signal.
6. **`/admin status` = full health + version check,** admin-only.
7. **`admin` is a superset of `lead`:** seeding an `admin` sets both
   `is_admin = 1` and `is_lead = 1`, so every existing `members::is_lead`
   gate admits admins with no change to `team.rs`.

## Current state (what exists today)

- **CLI** (`src/main.rs`): `clap` `Command` enum with `Init`, `Discord
  {Login,Logout}`, `Service {Install}`, `Maintenance {Run}`, `Status`.
  `main` is `#[tokio::main]`. `run_status()` calls `service::status()`
  then `discord_login::ping()`.
- **`src/service.rs`**: `UNIT_PATH`
  (`/etc/systemd/system/dispatchd.service`), `MAINTENANCE_SERVICE_PATH`,
  `MAINTENANCE_TIMER_PATH`, `ENV_DIR` (`/etc/dispatchd`), `CRED_PATH`.
  `render_unit(exe, user)` builds the main unit string (pure, unit-tested).
  `install()` (Linux-only, `#[cfg]`-gated with a non-Linux `bail!` stub):
  resolves the exe path + user, checks `systemd_version() >=
  MIN_SYSTEMD_VERSION` (250), writes the 3 unit files, `daemon-reload`,
  `enable dispatchd.service`, `enable --now dispatchd-maintenance.timer`.
  `status()` (Linux-only, non-Linux stub) `println!`s the systemd side.
  `systemctl_query(args)` runs a read-only `systemctl` and returns its
  output as text.
- **`src/discord_login.rs`**: `ping()` `println!`s the Discord side
  (`discord:` header, token resolution, `Http::get_current_user`
  round-trip latency). `decrypt_cred_file(path)` for the one-off status
  invocation outside systemd.
- **`src/members.rs`**: `VALID_ROLES = ["lead","designer","senior",
  "medior","junior"]`. `MembersFile`/`MemberSeed` (serde). `seed(conn)`
  validates each role against `VALID_ROLES` then upserts
  `(discord_user_id, name, role, is_lead)` with `is_lead = role ==
  "lead"`. `is_lead(conn, id) -> Result<bool>` (`false` for unknown id).
  `roster`, `name_of`, `all_member_ids`.
- **`src/db/mod.rs`**: `migrations()` = a `vec![M::up(include_str!(...))]`
  of the 4 files under `src/db/migrations/`. `open()` runs
  `.to_latest()`.
- **`src/db/migrations/0001_initial.sql`**: `members(discord_user_id PK,
  name, role, is_lead BOOLEAN NOT NULL DEFAULT FALSE)`.
- **`src/discord/mod.rs`**: `Handler` holds `guild_id`, `db`, `timezone`.
  `ready` registers a `commands` vec (`ping`, `help`, `todo`, `progress`,
  `team`) via `guild_id.set_commands`. `interaction_create` matches on
  `command.data.name` for `Command`, `Autocomplete`, and `Modal` arms.
  `run(token, guild_id, config, db)` builds the `Client`, spawns
  `ticker::run` if `discord_standup_channel_id` is set, then
  `client.start()`.
- **`src/discord/team.rs`**: `command()` =
  `CreateCommand::new("team").default_member_permissions(MANAGE_GUILD)`
  with `status`/`report`/`remind`/`skip-meeting` subcommands. Each
  handler re-checks `members::is_lead` bot-side and replies ephemerally.
  `subcommand(options)` unwraps the nested subcommand options.
- **`src/discord/help.rs`**: static `HELP_TEXT`, unit test asserts every
  command appears.
- **`build.rs`**: bakes `DISPATCHD_GIT_SHA`, `DISPATCHD_IS_TAGGED`,
  `DISPATCHD_VERSION`.
- **`install.sh`**: detects target triple (`x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`, `armv7-unknown-linux-musleabihf`,
  `aarch64-apple-darwin`), resolves the latest tag from the GitHub API
  (or `$DISPATCHD_VERSION`), downloads `dispatchd-<target>.tar.gz` +
  `SHA256SUMS` from `github.com/oxGrad/dispatchd/releases/download/<tag>/`,
  `sha256sum -c`, `tar -xzf`, `install -m 0755` into `/usr/local/bin`,
  best-effort `restorecon`, then offers a service restart.
- **Release** (`.github/workflows/release.yml`): builds static musl
  binaries for the 3 Linux targets + `aarch64-apple-darwin`, packages
  each as `dispatchd-<target>.tar.gz` (the tarball contains just the
  `dispatchd` binary at the root), generates `SHA256SUMS`
  (`sha256sum -- *.tar.gz`), uploads all to a GitHub Release.

## Design

### 1. Dependencies

Add to `Cargo.toml`:

- `reqwest` — pinned to serenity's major (`0.12`), `default-features =
  false, features = ["rustls-tls"]`. Verify against `Cargo.lock` during
  implementation that this unifies cleanly with serenity's `reqwest`
  feature set; if not, use `ureq = { version = "3", features =
  ["rustls"] }` instead and keep the upgrade module's HTTP calls blocking.
- `serde_json` — already transitive via serenity; make it direct. Used
  for the GitHub API response and the request/status files.
- `sha2` — release-asset checksum verification.
- `flate2` (default `miniz_oxide` backend, pure Rust) + `tar` — unpack
  `dispatchd-<target>.tar.gz`.

`.cargo/audit.toml` — no change expected; re-run `cargo audit` after the
dependency bump and add documented ignores only if a new advisory
appears with no fix available.

### 2. `build.rs` — `DISPATCHD_TARGET`

Add:

```rust
let target = std::env::var("TARGET").unwrap_or_default();
println!("cargo:rustc-env=DISPATCHD_TARGET={target}");
```

Cargo sets `TARGET` for build scripts. This is the exact triple the
running binary was built for, so the upgrade module picks the correct
release asset with no `cfg!` guessing. The release workflow's `cross` /
`cargo build --target` invocations set it correctly; a plain local
`cargo build` gets the host triple, which is right for local use.

Version comparison uses `env!("CARGO_PKG_VERSION")` directly (always
`MAJOR.MINOR.PATCH`, no `v`, no sha suffix).

### 3. `src/upgrade.rs` — shared upgrade logic

**Pure helpers (all unit-tested):**

- `const REPO: &str = "oxGrad/dispatchd";`
- `const RUN_DIR: &str = "/run/dispatchd";`
  `const REQUEST_PATH: &str = "/run/dispatchd/upgrade.request";`
  `const STATUS_PATH: &str = "/run/dispatchd/upgrade.status";`
- `fn asset_name() -> String` → `format!("dispatchd-{}.tar.gz",
  env!("DISPATCHD_TARGET"))`.
- `fn parse_latest_tag(api_json: &str) -> Result<String>` — `serde_json`,
  read `.tag_name` (e.g. `"v0.6.0"`).
- `struct Version([u64; 3])` with `fn parse(s: &str) -> Option<Version>`
  (strip a leading `v`, take the part before the first space, split on
  `.`, parse 3 numbers) and `Ord`. `fn is_newer(latest, current) ->
  bool`.
- `fn sha256_for(sums_file: &str, asset: &str) -> Option<String>` — find
  the `"<hex>  <asset>"` line, return the hex (lowercased).
- `fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool` — `sha2`.
- `Request { requested_by: String, requested_by_name: String, channel_id:
  String, target_version: Option<String>, restart: bool, requested_at:
  String }` — `Serialize`/`Deserialize`.
- `enum StatusLine` (serde tag `step`): `Checking`, `Found { current,
  latest }`, `Downloading { asset }`, `Verified`, `Swapped`,
  `Restarting`, `Done { from, to, channel_id, requested_by,
  requested_by_name, noop: bool }`, `Error { message, channel_id:
  Option<String> }`. One JSON object per line (append-only).
- `fn parse_status(contents: &str) -> Vec<StatusLine>` — parse each
  non-empty line, skip malformed.

**I/O (not unit-tested — same rationale as `src/discord/*` and the live
gateway):**

- `async fn fetch_latest_tag() -> Result<String>` — GET
  `https://api.github.com/repos/{REPO}/releases/latest` with a
  `User-Agent: dispatchd/<version>` header (GitHub requires one), parse.
- `struct UpgradeCheck { current: String, latest: String, update_available:
  bool }`; `async fn check() -> Result<UpgradeCheck>`.
- `async fn download_and_stage(tag: &str, dest_dir: &Path) ->
  Result<PathBuf>` — GET the asset + `SHA256SUMS` from
  `https://github.com/{REPO}/releases/download/{tag}/`, verify the asset's
  hash against its `SHA256SUMS` line, `flate2::read::GzDecoder` +
  `tar::Archive`, extract the entry named `dispatchd` to
  `dest_dir/.dispatchd.upgrade.<pid>`, `chmod 0o755`, return the temp
  path. `dest_dir` is the parent of the resolved `current_exe()` so the
  final rename is same-filesystem/atomic.
- `fn install_staged(staged: &Path, dest: &Path) -> Result<()>` —
  `std::fs::rename(staged, dest)` (Linux replaces a running binary
  fine); best-effort `restorecon <dest>` if `restorecon` is on `PATH`.
  On `EACCES`, error with "cannot write `<dest>` — re-run with sudo".
- `fn resolve_exe() -> Result<PathBuf>` — `current_exe()` then
  `canonicalize()` (follow the `/usr/local/bin/dispatchd` symlink if any).
- `#[cfg(target_os = "linux")] fn restart_dispatchd() -> Result<()>` —
  `systemctl restart dispatchd`. Non-Linux stub: `bail!`.

**CLI entry:**

```rust
pub struct UpgradeArgs {
    pub check: bool,
    pub no_restart: bool,
    pub version: Option<String>,     // "v0.6.0" or "0.6.0"
    pub from_request: bool,          // hidden; helper mode
}

pub async fn run(args: UpgradeArgs) -> anyhow::Result<()>
```

Normal mode (`from_request = false`):

1. `let target = match &args.version { Some(v) => normalize(v), None =>
   fetch_latest_tag().await? };`
2. `current = env!("CARGO_PKG_VERSION")`.
3. If `--check`: print `current` / `target` / "up to date" or "update
   available", return.
4. If `args.version.is_none()` and `!Version::is_newer(target, current)`:
   print "already on the latest version (`<current>`)", return `Ok`.
   (An explicit `--version` always proceeds, enabling downgrades.)
5. `let exe = resolve_exe()?; let dir = exe.parent();`
6. `let staged = download_and_stage(&target, dir).await?;` (prints
   "downloading …", "verifying …").
7. `install_staged(&staged, &exe)?;` prints "installed `<target>` to
   `<exe>`".
8. If `--no-restart`: print `Run \`sudo systemctl restart dispatchd\` to
   apply.` and return.
9. Else `restart_dispatchd()?` (skipped with a note if not on Linux / no
   unit installed / not root — mirror `install.sh`'s "print the command"
   fallback).

Helper mode (`from_request = true`, run by `dispatchd-upgrade.service` as
root):

1. Read + parse `REQUEST_PATH`. Malformed / missing → append an `Error`
   status line, delete `REQUEST_PATH`, exit non-zero.
2. Truncate `STATUS_PATH`, append `Checking`.
3. Resolve `target` (request's `target_version` or `fetch_latest_tag`),
   append `Found { current, latest }`.
4. If not newer and no pin → append `Done { noop: true, .. }`, delete
   `REQUEST_PATH`, exit `0` (no restart).
5. `Downloading` → `download_and_stage` → `Verified` → `install_staged` →
   `Swapped`.
6. Append `Restarting`; **delete `REQUEST_PATH` now** (before the restart,
   so the `.path` unit re-arms and a crash mid-restart can't loop).
7. Append `Done { from, to, channel_id, requested_by, requested_by_name,
   noop: false }`.
8. If `request.restart` → `restart_dispatchd()` (this kills this
   process's parent bot; `Done` is already written).
9. Any error after step 2 → append `Error { message, channel_id: Some(..)
   }`, delete `REQUEST_PATH`, exit non-zero. **`REQUEST_PATH` is deleted
   on every exit path.**

`--from-request` is registered with clap `#[arg(hide = true)]`.

Cross-platform: `download_and_stage` / `install_staged` / the
`--check` and normal swap paths compile everywhere. `restart_dispatchd`
and `--from-request` are effectively Linux-only (`--from-request` on
non-Linux → `bail!`).

### 4. `src/main.rs` — CLI wiring

```rust
enum Command {
    // ...existing...
    /// Download and install the latest dispatchd release
    Upgrade(UpgradeArgs),
}

#[derive(clap::Args)]
struct UpgradeArgs {
    /// Report the current and latest version, then exit
    #[arg(long)]
    check: bool,
    /// Swap the binary but don't restart the service
    #[arg(long)]
    no_restart: bool,
    /// Install a specific tag (allows downgrades), e.g. v0.4.0
    #[arg(long, value_name = "TAG")]
    version: Option<String>,
    #[arg(long, hide = true)]
    from_request: bool,
}
```

`match cli.command { ... Some(Command::Upgrade(a)) => return
upgrade::run(a.into()).await, ... }`. `mod upgrade;` added.

### 5. `src/service.rs` — the privilege bridge

New constants:

```rust
const UPGRADE_SERVICE_PATH: &str = "/etc/systemd/system/dispatchd-upgrade.service";
const UPGRADE_PATH_PATH:    &str = "/etc/systemd/system/dispatchd-upgrade.path";
pub(crate) const RUN_DIR:   &str = "/run/dispatchd";
```

**`render_unit` change** — add to the `[Service]` block:

```
RuntimeDirectory=dispatchd
RuntimeDirectoryPreserve=yes
```

`RuntimeDirectory=dispatchd` makes systemd create `/run/dispatchd`,
owned by the service `User=`, mode 0755, on every start.
`RuntimeDirectoryPreserve=yes` keeps it across the upgrade restart so the
`Done` status line survives for the new instance to read. **This is a
unit-file change → existing deployments must re-run `sudo dispatchd
service install`.** Update the doc comment and the
`render_unit_interpolates_exe_path_and_user` test.

**New pure renderers (unit-tested):**

```rust
fn render_upgrade_service(exe_path: &str) -> String
```
```
[Unit]
Description=dispatchd self-upgrade (triggered by /admin upgrade)

[Service]
Type=oneshot
ExecStart=<exe_path> upgrade --from-request
```
No `User=` → runs as root. No `[Install]` → only ever started by the
`.path` unit.

```rust
fn render_upgrade_path() -> &'static str
```
```
[Unit]
Description=Watch for a dispatchd upgrade request

[Path]
PathExists=/run/dispatchd/upgrade.request
Unit=dispatchd-upgrade.service

[Install]
WantedBy=paths.target
```

**`install()` additions** (Linux block, after the maintenance units):

```rust
std::fs::write(UPGRADE_SERVICE_PATH, render_upgrade_service(exe))?;
std::fs::write(UPGRADE_PATH_PATH, render_upgrade_path())?;
println!("wrote {UPGRADE_SERVICE_PATH} and {UPGRADE_PATH_PATH}");
// ...after daemon-reload + the existing enables:
run_systemctl(&["enable", "--now", "dispatchd-upgrade.path"])?;
println!("dispatchd-upgrade.path installed and started (enables /admin upgrade).");
```

`install()` already holds `INSTALL_LOCK_PATH` and is idempotent — the two
extra `std::fs::write`s and one `enable` fit that.

### 6. `src/service.rs` + `src/discord_login.rs` — structured status

`/admin status` needs the same data `dispatchd status` prints. Refactor
both to return structs; keep the CLI functions as thin formatters.

`src/service.rs`:

```rust
pub struct ServiceStatus {
    pub systemd_version: Option<u32>,
    pub min_systemd_version: u32,
    pub unit_installed: bool,
    pub unit_enabled: Option<String>,   // "enabled" / "disabled" / ...
    pub unit_active: Option<String>,    // "active" / "inactive" / ...
    pub upgrade_helper_installed: bool,  // UPGRADE_PATH_PATH exists
    pub cred_present: bool,
}
#[cfg(target_os = "linux")] pub fn status_report() -> ServiceStatus
pub fn format_status(r: &ServiceStatus) -> String   // what status() prints today
```

`status()` becomes `println!("{}", format_status(&status_report()))`.
Non-Linux: `status_report()` returns an all-`None`/`false` value and
`format_status` renders "only supported on Linux" as today.

`src/discord_login.rs`:

```rust
pub struct DiscordPing {
    pub token_found: bool,
    pub result: Option<Result<(String /*name*/, String /*id*/, u128 /*ms*/), String>>,
}
pub async fn ping_report() -> DiscordPing
pub fn format_ping(p: &DiscordPing) -> String
```

`ping()` becomes `println!("{}", format_ping(&ping_report().await))`.

`format_status` / `format_ping` are pure and unit-tested against
representative struct values (installed+active, not-installed, ping-ok,
ping-failed, no-token).

### 7. `admin` role — schema + `members.rs`

**`src/db/migrations/0005_members_is_admin.sql`:**

```sql
ALTER TABLE members ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT FALSE;
```

Register in `db/mod.rs::migrations()`. Add a `db/mod.rs` test: open a DB
at schema 0004 (or just assert a fresh open has the column and existing
rows default to 0 — a fresh `open()` + insert a member without
`is_admin` + read it back as `false`).

**`src/members.rs`:**

- `VALID_ROLES` → add `"admin"`.
- `seed()` upsert: compute `let is_lead = matches!(member.role.as_str(),
  "lead" | "admin"); let is_admin = member.role == "admin";` and write
  both columns (`ON CONFLICT ... DO UPDATE SET ... is_lead =
  excluded.is_lead, is_admin = excluded.is_admin`).
- New `pub fn is_admin(conn: &Connection, discord_user_id: &str) ->
  Result<bool>` — mirrors `is_lead` (`false` for unknown id).
- Update the `is_lead` doc comment: "true when the member has tech-lead
  privileges — role `lead` **or** `admin`".
- Tests: seeding `role = "admin"` sets `is_admin = 1` **and** `is_lead =
  1`; `is_admin` is `false` for a `lead`, a `senior`, and an unknown id;
  existing `valid_file_seeds_all_members_with_correct_is_lead`-style
  coverage extended for admin.

**`members.example.toml`** — add to the role comment (`admin | lead |
designer | senior | medior | junior`) and add a commented example:

```toml
# [[members]]
# discord_user_id = "000000000000000000"
# name = "Bot Operator's Name"
# role = "admin"
```

Note in the comment that `admin` has every `/team` power plus `/admin`.

### 8. `src/discord/admin.rs` — the `/admin` command

Structure mirrors `team.rs`.

```rust
pub fn command() -> CreateCommand {
    CreateCommand::new("admin")
        .description("Bot-operator tools: status and self-upgrade")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .add_option(sub("status", "systemd + Discord health and the version check"))
        .add_option(
            CreateCommandOption::new(SubCommand, "upgrade", "Upgrade dispatchd to the latest release")
                .add_sub_option(CreateCommandOption::new(String, "version",
                    "Install a specific tag (allows downgrade), e.g. v0.4.0").required(false))
                .add_sub_option(CreateCommandOption::new(Boolean, "restart",
                    "Restart the service after upgrading (default true)").required(false)),
        )
        .add_option(sub("help", "What the /admin commands do"))
}

pub fn subcommand(options) -> Option<(&str, &[CommandDataOption])>  // same as team.rs
```

**Permission gate:** every subcommand does a bot-side `members::is_admin`
check (the `MANAGE_GUILD` `default_member_permissions` only hides the
command in the client; the bot-side check is the source of truth, same
pattern as `team.rs`). Denied → ephemeral
`"⛔ This command is restricted to bot operators (admin role)."`.

Shared, unit-tested pure helper:

```rust
fn permission_denied_reply() -> &'static str
```

**`/admin help`** — static ephemeral text listing the three subcommands.

**`/admin status`** (`handle_status`):

1. `members::is_admin` gate.
2. `let svc = crate::service::status_report();`
   `let ping = crate::discord_login::ping_report().await;`
   `let check = crate::upgrade::check().await;` (each failure degrades to
   a line like `version: unknown (network error)` rather than aborting).
3. Ephemeral reply: a fenced block combining
   `service::format_status(&svc)` + `discord_login::format_ping(&ping)` +
   the version line
   (`0.5.0 — update available: 0.6.0` / `0.5.0 (up to date)`), plus
   `upgrade helper: installed` / `not installed — run \`sudo dispatchd
   service install\``.

**`/admin upgrade`** (`handle_upgrade`):

1. `members::is_admin` gate.
2. `#[cfg(not(target_os = "linux"))]` builds still compile; the handler
   checks at runtime: if `/run/dispatchd` doesn't exist →
   ephemeral `"⚠️ The upgrade helper isn't installed. Run \`sudo
   dispatchd service install\` on the host once, then retry."` and stop.
3. If `upgrade::REQUEST_PATH` already exists → ephemeral `"⚠️ An upgrade
   is already in progress."` and stop.
4. Initial ephemeral `create_response`: `"🔎 Checking for updates…"`.
5. Write `REQUEST_PATH` (`serde_json::to_string(&Request { requested_by:
   user.id, requested_by_name: user.name, channel_id:
   command.channel_id, target_version: opts.version, restart:
   opts.restart.unwrap_or(true), requested_at: now })`). Write to a
   temp path in `/run/dispatchd` then rename onto `REQUEST_PATH` so the
   `.path` unit never sees a partial file.
6. Poll loop: every ~1.5 s for up to 120 s, read + `parse_status`, and
   `command.edit_response` with the rendered step list (`✓`/`↻` per
   line). Stop the loop when a `Done` or `Error` line appears, or on
   `Restarting`.
   - On `Restarting`: final `edit_response` with
     `"↻ Restarting dispatchd now — the new instance will confirm in
     this channel."` Return.
   - On `Error`: `edit_response` with `"❌ Upgrade failed: <message>"`.
   - On `Done { noop: true }`: `"✅ Already on the latest version
     (<current>)."`.
   - Timeout with no terminal line: `"⚠️ No progress after 120s —
     check \`journalctl -u dispatchd-upgrade\` on the host."`.

**New-instance confirmation** — in `discord/mod.rs`, not `admin.rs` (it
runs once at startup, not per-interaction): see section 9.

### 9. `src/discord/mod.rs` — registration + startup confirmation

- `mod admin;`
- `ready` handler: push `admin::command()` into the `commands` vec.
- `interaction_create`:
  - `"admin" => match admin::subcommand(&command.data.options) { Some(("status", _)) => admin::handle_status(&ctx, &command, &self.db).await, Some(("upgrade", opts)) => admin::handle_upgrade(&ctx, &command, opts, &self.db).await, Some(("help", _)) => admin::handle_help(&ctx, &command).await, _ => {} }`
  - no autocomplete, no modal for `/admin`.
- **Startup confirmation:** in `ready` (after `set_commands`), call a new
  `admin::post_upgrade_confirmation(&ctx).await`. It:
  1. Reads `upgrade::STATUS_PATH`; if absent, return.
  2. `parse_status`; find a terminal `Done { noop: false, .. }` line.
  3. If found and `Version::parse(to) == Version::parse(current)` (we are
     now running the upgraded binary): send a **non-ephemeral**
     `ChannelId::new(channel_id).send_message` →
     `"✅ dispatchd upgraded v<from> → v<to> (requested by <@requested_by>)."`
  4. If `Done` but versions mismatch, or an `Error` line with a
     `channel_id`: send `"⚠️ dispatchd restarted but the upgrade may not
     have completed — running v<current>. Check \`journalctl -u
     dispatchd-upgrade\`."`
  5. Delete `STATUS_PATH` (and `REQUEST_PATH` if somehow still present)
     regardless, so it fires exactly once. `/run/dispatchd` is owned by
     the bot user, so unlinking a root-written file there succeeds (dir
     is writable, no sticky bit).

  Guard against acting on a stale file from a manual/CLI upgrade: only
  proceed when the `Done`/`Error` line carries a non-empty `channel_id`
  (the CLI path never sets one).

### 10. `src/discord/help.rs`

Add to `HELP_TEXT` and the test's needle list:

```
`/admin status` - (admin only) systemd + Discord health and version check
`/admin upgrade` - (admin only) upgrade dispatchd to the latest release
`/admin help` - admin-specific help
```

### 11. Docs

- **`docs/installing.md`** — under "Upgrading", add `dispatchd upgrade`
  as the recommended path (`sudo dispatchd upgrade`), documenting
  `--check`, `--no-restart`, `--version`. Keep the `curl | sh` method as
  the alternative. Note that `dispatchd upgrade` covers the "binary
  changed" case but the "systemd unit changed" caveat still needs
  `service install`.
- **`docs/user-guide.md`** — a `## Bot operator (`admin` role)` section:
  what `admin` grants (all of `/team` + `/admin`), `/admin status`,
  `/admin upgrade` and its Discord progress/confirmation flow.
- **`docs/discord-setup.md`** — mention `admin` in the roster role list;
  note that enabling `/admin upgrade` needs one `sudo dispatchd service
  install` (also true for anyone upgrading an existing deployment to this
  version).
- **`CLAUDE.md`** — "Running it" gets `dispatchd upgrade`; "Project
  layout" gets `src/upgrade.rs`, `src/discord/admin.rs`, the
  `dispatchd-upgrade.service`/`.path` units under `service.rs`, the
  `members.is_admin` column, and migration `0005`. The `service.rs` and
  `discord_login.rs` entries note the new `*_report()` / `format_*`
  split.

### 12. Testing

Follows the project split (pure logic unit-tested; serenity handlers and
network I/O left to manual verification, per the CLAUDE.md testing
notes).

**Unit-tested:**

- `upgrade.rs`: `asset_name` shape; `parse_latest_tag` (well-formed,
  missing key); `Version::parse` (`"0.5.0"`, `"v0.6.0"`, `"0.5.0
  (abc1234)"`, junk) and ordering; `is_newer` (older/equal/newer,
  and the `--version` pin path is a caller concern); `sha256_for`
  (present, absent, wrong-name-substring); `verify_sha256` (match /
  mismatch); `Request` and `StatusLine` round-trip through
  `serde_json`; `parse_status` skips malformed lines and finds the
  terminal line.
- `service.rs`: `render_upgrade_service` (has `ExecStart ... upgrade
  --from-request`, `Type=oneshot`, no `User=`, no `[Install]`);
  `render_upgrade_path` (`PathExists=`, `Unit=`, `WantedBy=paths.target`);
  `render_unit` now also asserts `RuntimeDirectory=dispatchd` +
  `RuntimeDirectoryPreserve=yes`; `format_status` for
  installed/enabled/active, not-installed, and helper-installed vs not.
- `discord_login.rs`: `format_ping` for ok / failed / no-token.
- `members.rs`: `admin` seeds `is_admin=1` and `is_lead=1`; `is_admin`
  false for lead / senior / unknown; re-seed `admin → senior` clears
  both flags.
- `db/mod.rs`: fresh open exposes `is_admin` defaulting to 0 for a row
  inserted without it.
- `discord/admin.rs`: `subcommand` parsing; `permission_denied_reply`
  wording; the status-block formatter given fixture structs; the
  step-list renderer given a `Vec<StatusLine>`.
- `discord/help.rs`: existing test extended with the 3 `/admin` needles.

**Manual verification (documented in the PR, not automated):**

- `sudo dispatchd upgrade --check` against the real repo.
- `sudo dispatchd service install` writes and enables the two new units;
  `systemctl list-unit-files | grep dispatchd-upgrade`.
- End-to-end `/admin upgrade` on a real host between two tagged releases:
  progress messages stream, the bot restarts, the new instance posts the
  confirmation.
- `/admin status` output.
- A forced helper failure (bad `--version`) leaves no `upgrade.request`
  behind and reports `Error` in the ephemeral.

## Out of scope

- Auto-upgrade / scheduled upgrade. `/admin upgrade` and `dispatchd
  upgrade` are always explicitly invoked.
- Rollback beyond `--version <older tag>`.
- Changing how `install.sh` works, or hosting/CI changes.
- Verifying release-artifact signatures beyond the existing `SHA256SUMS`
  (there is no signing in the release pipeline today).
- A `lead`-visible `/admin status`. Admin-only.
- Windows.

## Risks / notes

- **`.path` retrigger loop** — if the helper exits without deleting
  `REQUEST_PATH`, systemd restarts `dispatchd-upgrade.service`
  immediately, forever. Every helper exit path must `remove_file` the
  request. Covered by a dedicated unit test on the helper's cleanup
  contract (extract the "always delete request" into a small guard
  type / `Drop`, testable without systemd).
- **`reqwest` feature unification** — must be checked against
  `Cargo.lock` early; `ureq` fallback is pre-approved.
- **Old deployments** — `/admin upgrade` before `service install` is
  re-run has no `/run/dispatchd` and no `.path` unit; handled by the
  explicit "helper not installed" reply and the `/admin status` line.
- **`RuntimeDirectoryPreserve=yes`** — the status file lingers in
  `/run/dispatchd` until the next reboot or until the new instance
  deletes it. The new instance always deletes it in step 9.5, and it's
  tmpfs, so a missed delete self-heals on reboot.
- **GitHub API rate limit** — unauthenticated, 60 req/hr per IP. `check()`
  and `/admin status` each cost one request; fine for this usage. A
  failure degrades to "version: unknown", never an abort.
- **macOS release build** must still compile — `upgrade.rs` I/O is
  cross-platform; only `restart_dispatchd` and `--from-request`'s
  systemd assumptions are gated.

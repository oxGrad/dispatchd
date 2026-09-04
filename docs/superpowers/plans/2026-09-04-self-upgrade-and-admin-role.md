# Self-upgrade + `admin` role Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let dispatchd upgrade its own binary from the CLI and from Discord, and add an `admin` roster role that is a strict superset of the tech lead.

**Architecture:** A new `src/upgrade.rs` module owns all upgrade logic — pure helpers (version parsing, checksum verification, request/status-file serde) plus reqwest/tar I/O — behind a `dispatchd upgrade` CLI subcommand. The Discord path keeps the bot unprivileged: `/admin upgrade` writes a request file into `/run/dispatchd/`, a systemd `.path` unit fires a root `dispatchd-upgrade.service` oneshot that runs `dispatchd upgrade --from-request`, and the freshly-restarted bot posts a completion message. The `admin` role rides on a new `members.is_admin` column while also setting `is_lead=1` so every existing `/team` gate admits admins unchanged.

**Tech Stack:** Rust 2024, `serenity` 0.12 + `tokio`, `rusqlite` + `rusqlite_migration`, `clap` derive, `reqwest` (rustls, already transitive via serenity), `sha2`, `flate2` + `tar`, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-09-04-self-upgrade-and-admin-role-design.md`

## Global Constraints

- Repo slug is `oxGrad/dispatchd`. Release assets are named `dispatchd-<target-triple>.tar.gz`; each tarball contains the `dispatchd` binary at its root. `SHA256SUMS` lines are sha256sum-style: `<64 hex chars><two spaces><filename>`.
- Target triples in play: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `armv7-unknown-linux-musleabihf`, `aarch64-apple-darwin`.
- `cargo fmt --check`, `cargo clippy --all-targets` (no warnings), and `cargo test` must all be clean before a task is done. `cargo build --release` must succeed (exercises the full serenity/tokio tree). Coverage gate: `fail-under-coverage: 30` — keep pure logic unit-tested.
- systemd floor stays `MIN_SYSTEMD_VERSION = 250` (unchanged; do not touch).
- The bot process runs unprivileged (`User=` in the unit). Only `dispatchd-upgrade.service` (no `User=`) runs as root. Nothing in the bot may assume root.
- Every exit path of the `--from-request` helper MUST delete `/run/dispatchd/upgrade.request` — otherwise the `.path` unit re-triggers in a tight loop.
- Linux-only code paths use `#[cfg(target_os = "linux")]` with a non-Linux stub that `bail!`s, matching `service.rs` / `discord_login.rs`. The macOS release build must compile.
- Tests that mutate `DISPATCHD_*` / `XDG_*` / `CREDENTIALS_DIRECTORY` env vars take `crate::test_support::ENV_LOCK`. DB-backed tests use a real `tempfile::tempdir()` path, never `:memory:`.
- Commit messages: conventional-commits prefix (`feat:`, `fix:`, `docs:`, `chore:`, `test:`). No `Co-Authored-By` or "Generated with" trailers.

---

## File Structure

**Created:**
- `src/upgrade.rs` — all upgrade logic: pure helpers + reqwest/tar I/O + the `dispatchd upgrade` CLI entry (normal + `--from-request` helper modes).
- `src/discord/admin.rs` — the `/admin` command group (`status`, `upgrade`, `help`), mirroring `src/discord/team.rs`.
- `src/db/migrations/0005_members_is_admin.sql` — `ALTER TABLE members ADD COLUMN is_admin`.

**Modified:**
- `build.rs` — bake `DISPATCHD_TARGET`.
- `Cargo.toml` — add `reqwest`, `serde_json`, `sha2`, `flate2`, `tar`; add `time` to tokio features.
- `src/main.rs` — `mod upgrade;`, `Command::Upgrade(UpgradeArgs)`, dispatch.
- `src/members.rs` — `"admin"` in `VALID_ROLES`; seed `is_admin` + `is_lead`; new `is_admin()`.
- `src/db/mod.rs` — register migration `0005`.
- `src/service.rs` — `RuntimeDirectory=` in `render_unit`; `render_upgrade_service` / `render_upgrade_path`; write+enable the two units in `install()`; `ServiceStatus` + `status_report()` + `format_status()`.
- `src/discord_login.rs` — `DiscordPing` + `ping_report()` + `format_ping()`.
- `src/discord/mod.rs` — register `admin::command()`; dispatch `"admin"`; call `admin::post_upgrade_confirmation()` on `ready`.
- `src/discord/help.rs` — three `/admin` lines + test needles.
- `members.example.toml` — `admin` role option + example.
- `docs/installing.md`, `docs/user-guide.md`, `docs/discord-setup.md`, `CLAUDE.md` — document the new surface.

---

## Task 1: `admin` role — schema + `members.rs`

**Files:**
- Create: `src/db/migrations/0005_members_is_admin.sql`
- Modify: `src/db/mod.rs:7-14` (migrations vec), `src/members.rs:11` (`VALID_ROLES`), `src/members.rs:77-92` (`seed` upsert), `src/members.rs:96-105` (add `is_admin` after `is_lead`)
- Modify: `members.example.toml`
- Test: inline `#[cfg(test)] mod tests` in `src/members.rs` and `src/db/mod.rs`

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces:
  - `members::is_admin(conn: &rusqlite::Connection, discord_user_id: &str) -> anyhow::Result<bool>` — `false` for an unknown id.
  - `members` table gains `is_admin BOOLEAN NOT NULL DEFAULT FALSE`; seeding `role = "admin"` sets `is_admin = 1` AND `is_lead = 1`.

- [ ] **Step 1: Write the migration file**

Create `src/db/migrations/0005_members_is_admin.sql`:

```sql
-- 'admin' role: a strict superset of the tech lead. Seeded rows with
-- role='admin' get is_admin=1 AND is_lead=1, so every existing is_lead
-- permission check admits admins with no code change.
ALTER TABLE members ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT FALSE;
```

- [ ] **Step 2: Register the migration**

In `src/db/mod.rs`, add to the `migrations()` vec after the `0004` line:

```rust
        M::up(include_str!("migrations/0004_sow_ref.sql")),
        M::up(include_str!("migrations/0005_members_is_admin.sql")),
    ])
```

- [ ] **Step 3: Write the failing db test**

Add to `src/db/mod.rs`'s `mod tests`:

```rust
    #[test]
    fn members_has_is_admin_defaulting_to_false() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("dispatchd.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES ('1', 'A', 'senior', 0)",
            [],
        )
        .unwrap();
        let is_admin: bool = conn
            .query_row("SELECT is_admin FROM members WHERE discord_user_id = '1'", [], |r| r.get(0))
            .unwrap();
        assert!(!is_admin);
    }
```

- [ ] **Step 4: Run — expect FAIL then PASS**

Run: `cargo test -p dispatchd db::tests::members_has_is_admin_defaulting_to_false`
Expected: after Steps 1-2 it PASSES; if you ran it before Step 2 it fails with "no such column: is_admin". Also run `cargo test db::tests::fresh_open_creates_all_tables` — still passes (table set unchanged).

- [ ] **Step 5: Add `"admin"` to `VALID_ROLES`**

`src/members.rs:11`:

```rust
const VALID_ROLES: &[&str] = &["admin", "lead", "designer", "senior", "medior", "junior"];
```

- [ ] **Step 6: Write failing `members.rs` tests**

Add to `src/members.rs`'s `mod tests`:

```rust
    #[test]
    fn admin_role_seeds_both_is_admin_and_is_lead() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Ops"
            role = "admin"
            "#,
        )
        .unwrap();

        let (is_lead, is_admin): (bool, bool) = conn
            .query_row(
                "SELECT is_lead, is_admin FROM members WHERE discord_user_id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(is_lead, "admin must inherit lead privileges");
        assert!(is_admin);
    }

    #[test]
    fn is_admin_is_true_only_for_admins() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Ops"
            role = "admin"

            [[members]]
            discord_user_id = "2"
            name = "Lead"
            role = "lead"
            "#,
        )
        .unwrap();

        assert!(is_admin(&conn, "1").unwrap());
        assert!(!is_admin(&conn, "2").unwrap());
        assert!(!is_admin(&conn, "999").unwrap());
    }

    #[test]
    fn reseeding_admin_to_senior_clears_both_flags() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(&conn, "[[members]]\ndiscord_user_id = \"1\"\nname = \"X\"\nrole = \"admin\"\n").unwrap();
        seed_from(&conn, "[[members]]\ndiscord_user_id = \"1\"\nname = \"X\"\nrole = \"senior\"\n").unwrap();
        let (is_lead, is_admin): (bool, bool) = conn
            .query_row("SELECT is_lead, is_admin FROM members WHERE discord_user_id = '1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(!is_lead);
        assert!(!is_admin);
    }
```

- [ ] **Step 7: Run — expect FAIL**

Run: `cargo test -p dispatchd members::tests::is_admin_is_true_only_for_admins`
Expected: FAIL — `is_admin` not found; `seed` still writes only 4 columns.

- [ ] **Step 8: Update `seed()` to write `is_admin`**

`src/members.rs`, in the second `for member in &file.members` loop, replace the body:

```rust
    for member in &file.members {
        let is_lead = matches!(member.role.as_str(), "lead" | "admin");
        let is_admin = member.role == "admin";
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead, is_admin)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(discord_user_id) DO UPDATE SET
                 name = excluded.name,
                 role = excluded.role,
                 is_lead = excluded.is_lead,
                 is_admin = excluded.is_admin",
            rusqlite::params![member.discord_user_id, member.name, member.role, is_lead, is_admin],
        )
        .with_context(|| format!("failed to upsert member {:?}", member.discord_user_id))?;
    }
```

- [ ] **Step 9: Add `is_admin()` and update the `is_lead` doc comment**

`src/members.rs`, immediately after the `is_lead` fn:

```rust
/// `true` when the member has bot-operator privileges (role `admin`).
/// `false` for an unknown `discord_user_id`, not an error - the bot-side
/// source-of-truth check for `/admin`.
pub fn is_admin(conn: &Connection, discord_user_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT is_admin FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}
```

Change the `is_lead` doc comment first line to:

```rust
/// `true` when the member has tech-lead privileges - role `lead` or
/// `admin`. `false` for an unknown `discord_user_id`, not an error - the
/// bot-side source-of-truth check for `/team`.
```

- [ ] **Step 10: Update `members.example.toml`**

Change the role line in the header comment:

```
# role must be one of: admin | lead | designer | senior | medior | junior
# 'admin' has every /team capability plus the /admin command group
# (see docs/user-guide.md).
```

Add, as the first commented `[[members]]` block (before the lead example):

```toml
# [[members]]
# discord_user_id = "000000000000000000"
# name = "Bot Operator's Name"
# role = "admin"
```

- [ ] **Step 11: Run the full suite + lint**

Run: `cargo test -p dispatchd members:: db::` then `cargo clippy --all-targets` then `cargo fmt --check`
Expected: all PASS, clippy clean.

- [ ] **Step 12: Commit**

```bash
git add src/db/migrations/0005_members_is_admin.sql src/db/mod.rs src/members.rs members.example.toml
git commit -m "feat: add admin role (is_admin column, superset of lead)"
```

---

## Task 2: `upgrade.rs` pure helpers + `build.rs` target

**Files:**
- Modify: `build.rs`
- Modify: `Cargo.toml` (add `serde_json`, `sha2`)
- Create: `src/upgrade.rs` (pure section only)
- Modify: `src/main.rs:1-13` (add `mod upgrade;` in the module list)
- Test: inline `#[cfg(test)] mod tests` in `src/upgrade.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (all in `crate::upgrade`):
  - `const REPO: &str` = `"oxGrad/dispatchd"`
  - `const RUN_DIR: &str` = `"/run/dispatchd"`, `const REQUEST_PATH: &str` = `"/run/dispatchd/upgrade.request"`, `const STATUS_PATH: &str` = `"/run/dispatchd/upgrade.status"`
  - `fn asset_name() -> String` → `"dispatchd-<DISPATCHD_TARGET>.tar.gz"`
  - `fn current_version() -> &'static str` → `env!("CARGO_PKG_VERSION")`
  - `fn parse_latest_tag(api_json: &str) -> anyhow::Result<String>`
  - `struct Version([u64; 3])` with `fn parse(&str) -> Option<Version>`, derives `PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy`; `fn is_newer(latest: &str, current: &str) -> bool`
  - `fn sha256_for(sums_file: &str, asset: &str) -> Option<String>`
  - `fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool`
  - `struct Request { requested_by: String, requested_by_name: String, channel_id: String, target_version: Option<String>, restart: bool, requested_at: String }` — `Serialize + Deserialize + Debug + Clone`
  - `enum StatusLine` (serde-tagged on `"step"`): variants `Checking`, `Found { current: String, latest: String }`, `Downloading { asset: String }`, `Verified`, `Swapped`, `Restarting`, `Done { from: String, to: String, channel_id: String, requested_by: String, requested_by_name: String, noop: bool }`, `Error { message: String, channel_id: String }` — `Serialize + Deserialize + Debug + Clone + PartialEq`
  - `fn parse_status(contents: &str) -> Vec<StatusLine>`

- [ ] **Step 1: Add dependencies**

`Cargo.toml` `[dependencies]`, keeping alphabetical order:

```toml
serde_json = "1.0"
sha2 = "0.10"
```

(`serde_json` and `sha2`'s `digest` are already in `Cargo.lock` transitively; this just makes them direct.)

- [ ] **Step 2: Bake `DISPATCHD_TARGET` in `build.rs`**

Add before the final `println!` block in `build.rs`:

```rust
    // The exact target triple this binary is built for - `dispatchd
    // upgrade` uses it to pick the matching release asset
    // (dispatchd-<triple>.tar.gz). Cargo sets TARGET for build scripts.
    let target = std::env::var("TARGET").unwrap_or_default();
```

and add to the emitted vars:

```rust
    println!("cargo:rustc-env=DISPATCHD_TARGET={target}");
```

- [ ] **Step 3: Create `src/upgrade.rs` with the pure helpers**

```rust
//! Self-upgrade: resolve the latest GitHub release, download the matching
//! prebuilt binary, verify its SHA-256, swap it in, restart the service.
//! Driven by `dispatchd upgrade` (CLI) and, via `--from-request`, by the
//! root `dispatchd-upgrade.service` oneshot that `/admin upgrade` triggers.
//!
//! The pure helpers here (version math, checksum parsing, request/status
//! serde) are unit-tested. The reqwest/tar I/O is not, same as the
//! `src/discord/*` gateway code - see CLAUDE.md's testing notes.

use serde::{Deserialize, Serialize};

pub const REPO: &str = "oxGrad/dispatchd";
pub const RUN_DIR: &str = "/run/dispatchd";
pub const REQUEST_PATH: &str = "/run/dispatchd/upgrade.request";
pub const STATUS_PATH: &str = "/run/dispatchd/upgrade.status";

/// The crate version this binary was built from (e.g. "0.5.0"), no `v`.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release asset this binary needs, e.g.
/// "dispatchd-aarch64-unknown-linux-musl.tar.gz".
pub fn asset_name() -> String {
    format!("dispatchd-{}.tar.gz", env!("DISPATCHD_TARGET"))
}

/// Pulls `tag_name` (e.g. "v0.6.0") out of the GitHub
/// `releases/latest` JSON response.
pub fn parse_latest_tag(api_json: &str) -> anyhow::Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(api_json).map_err(|e| anyhow::anyhow!("bad releases JSON: {e}"))?;
    v.get("tag_name")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no tag_name in releases response"))
}

/// A `MAJOR.MINOR.PATCH` version, tolerant of a leading `v` and a trailing
/// ` (sha)` suffix (which `--version` prints for non-tagged builds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u64; 3]);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let core = s.split_whitespace().next().unwrap_or(s);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some(Version([major, minor, patch]))
    }
}

/// `true` when `latest` is a strictly higher version than `current`.
/// `false` if either fails to parse (caller treats that as "don't
/// auto-upgrade").
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (Version::parse(latest), Version::parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Finds `asset`'s hash in a `sha256sum`-style file
/// ("<hex><two spaces><name>"), lowercased. `None` if not listed.
pub fn sha256_for(sums_file: &str, asset: &str) -> Option<String> {
    sums_file.lines().find_map(|line| {
        let (hex, name) = line.split_once("  ")?;
        (name.trim() == asset).then(|| hex.trim().to_lowercase())
    })
}

/// `true` when `bytes` hashes to `expected_hex` (case-insensitive).
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool {
    use sha2::{Digest, Sha256};
    let got = Sha256::digest(bytes);
    let got_hex = got.iter().map(|b| format!("{b:02x}")).collect::<String>();
    got_hex.eq_ignore_ascii_case(expected_hex.trim())
}

/// What `/admin upgrade` writes to `REQUEST_PATH` for the root helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub requested_by: String,
    pub requested_by_name: String,
    pub channel_id: String,
    pub target_version: Option<String>,
    pub restart: bool,
    pub requested_at: String,
}

/// One progress line the helper appends to `STATUS_PATH` (one JSON object
/// per line). The bot tails these; the post-restart instance reads the
/// terminal `Done`/`Error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StatusLine {
    Checking,
    Found { current: String, latest: String },
    Downloading { asset: String },
    Verified,
    Swapped,
    Restarting,
    Done {
        from: String,
        to: String,
        channel_id: String,
        requested_by: String,
        requested_by_name: String,
        noop: bool,
    },
    Error { message: String, channel_id: String },
}

impl StatusLine {
    /// `Done` / `Error` end the sequence.
    pub fn is_terminal(&self) -> bool {
        matches!(self, StatusLine::Done { .. } | StatusLine::Error { .. })
    }
}

/// Parses `STATUS_PATH`'s contents, skipping blank and unparseable lines.
pub fn parse_status(contents: &str) -> Vec<StatusLine> {
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<StatusLine>(l).ok())
        .collect()
}
```

- [ ] **Step 4: Register the module**

`src/main.rs`, add `mod upgrade;` in alphabetical position (after `mod status;`... actually after `mod service;` and before `mod status;` — place as `mod upgrade;` after `mod status;` to keep it sorted):

```rust
mod status;
mod upgrade;
```

- [ ] **Step 5: Write the unit tests**

Add to `src/upgrade.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_has_the_target_triple() {
        let name = asset_name();
        assert!(name.starts_with("dispatchd-"));
        assert!(name.ends_with(".tar.gz"));
    }

    #[test]
    fn parse_latest_tag_reads_tag_name() {
        let json = r#"{"tag_name":"v0.6.0","name":"v0.6.0","draft":false}"#;
        assert_eq!(parse_latest_tag(json).unwrap(), "v0.6.0");
    }

    #[test]
    fn parse_latest_tag_errors_without_the_key() {
        assert!(parse_latest_tag(r#"{"message":"Not Found"}"#).is_err());
        assert!(parse_latest_tag("not json").is_err());
    }

    #[test]
    fn version_parse_tolerates_v_prefix_and_sha_suffix() {
        assert_eq!(Version::parse("0.5.0"), Version::parse("v0.5.0"));
        assert_eq!(Version::parse("0.5.0 (abc1234)"), Version::parse("0.5.0"));
        assert!(Version::parse("nope").is_none());
        assert!(Version::parse("1.2").is_none());
    }

    #[test]
    fn version_ordering() {
        assert!(Version::parse("0.6.0") > Version::parse("0.5.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        assert!(Version::parse("0.5.1") > Version::parse("0.5.0"));
    }

    #[test]
    fn is_newer_only_when_strictly_higher_and_parseable() {
        assert!(is_newer("v0.6.0", "0.5.0"));
        assert!(!is_newer("v0.5.0", "0.5.0"));
        assert!(!is_newer("v0.4.0", "0.5.0"));
        assert!(!is_newer("garbage", "0.5.0"));
    }

    #[test]
    fn sha256_for_matches_the_exact_asset_line() {
        let sums = "\
aaaa  dispatchd-x86_64-unknown-linux-musl.tar.gz
bbbb  dispatchd-aarch64-unknown-linux-musl.tar.gz
";
        assert_eq!(
            sha256_for(sums, "dispatchd-aarch64-unknown-linux-musl.tar.gz").as_deref(),
            Some("bbbb")
        );
        assert_eq!(sha256_for(sums, "dispatchd-armv7-unknown-linux-musleabihf.tar.gz"), None);
    }

    #[test]
    fn verify_sha256_checks_the_digest() {
        // echo -n "hello" | sha256sum
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(b"hello", expected));
        assert!(verify_sha256(b"hello", &expected.to_uppercase()));
        assert!(!verify_sha256(b"world", expected));
    }

    #[test]
    fn request_round_trips_through_json() {
        let req = Request {
            requested_by: "111".into(),
            requested_by_name: "Ops".into(),
            channel_id: "222".into(),
            target_version: Some("v0.6.0".into()),
            restart: true,
            requested_at: "2026-09-04T00:00:00Z".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(back.channel_id, "222");
        assert_eq!(back.target_version.as_deref(), Some("v0.6.0"));
        assert!(back.restart);
    }

    #[test]
    fn status_lines_round_trip_and_parse_skips_junk() {
        let lines = [
            StatusLine::Checking,
            StatusLine::Found { current: "0.5.0".into(), latest: "0.6.0".into() },
            StatusLine::Downloading { asset: "dispatchd-x.tar.gz".into() },
            StatusLine::Verified,
            StatusLine::Swapped,
            StatusLine::Restarting,
        ];
        let mut buf = String::new();
        for l in &lines {
            buf.push_str(&serde_json::to_string(l).unwrap());
            buf.push('\n');
        }
        buf.push_str("this is not json\n\n");
        let parsed = parse_status(&buf);
        assert_eq!(parsed, lines);
    }

    #[test]
    fn done_and_error_are_terminal() {
        assert!(StatusLine::Done {
            from: "0.5.0".into(),
            to: "0.6.0".into(),
            channel_id: "1".into(),
            requested_by: "2".into(),
            requested_by_name: "Ops".into(),
            noop: false,
        }
        .is_terminal());
        assert!(StatusLine::Error { message: "x".into(), channel_id: "1".into() }.is_terminal());
        assert!(!StatusLine::Verified.is_terminal());
    }
}
```

- [ ] **Step 6: Run tests + lint + release build**

Run: `cargo test -p dispatchd upgrade::` then `cargo clippy --all-targets` then `cargo fmt --check` then `cargo build --release`
Expected: all PASS. (`cargo build` re-runs `build.rs`; confirm no error about `DISPATCHD_TARGET`.)

- [ ] **Step 7: Commit**

```bash
git add build.rs Cargo.toml Cargo.lock src/upgrade.rs src/main.rs
git commit -m "feat: upgrade.rs pure helpers (version math, checksum, request/status serde)"
```

---

## Task 3: `upgrade.rs` I/O + `dispatchd upgrade` CLI (normal mode)

**Files:**
- Modify: `Cargo.toml` (add `reqwest`, `flate2`, `tar`; add `time` to `tokio` features)
- Modify: `src/upgrade.rs` (add I/O fns + `UpgradeArgs` + `run()` normal path)
- Modify: `src/main.rs` (`Command::Upgrade`, `UpgradeArgs`, dispatch)
- Test: `src/upgrade.rs` `mod tests` (staging/install with temp files)

**Interfaces:**
- Consumes: everything from Task 2.
- Produces (in `crate::upgrade`):
  - `pub struct UpgradeArgs { pub check: bool, pub no_restart: bool, pub version: Option<String>, pub from_request: bool }`
  - `pub async fn run(args: UpgradeArgs) -> anyhow::Result<()>` — this task implements only the `from_request == false` path; Task 4 adds the helper path.
  - `struct UpgradeCheck { current: String, latest: String, update_available: bool }`
  - `async fn check() -> anyhow::Result<UpgradeCheck>`
  - `async fn fetch_latest_tag() -> anyhow::Result<String>`
  - `async fn download_and_stage(tag: &str, dest_dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf>`
  - `fn install_staged(staged: &std::path::Path, dest: &std::path::Path) -> anyhow::Result<()>`
  - `fn resolve_exe() -> anyhow::Result<std::path::PathBuf>`
  - `fn normalize_tag(v: &str) -> String` — ensure a leading `v` for URL building
  - `#[cfg(target_os = "linux")] fn restart_dispatchd() -> anyhow::Result<()>` + non-Linux stub

- [ ] **Step 1: Add dependencies**

`Cargo.toml` `[dependencies]` (alphabetical):

```toml
flate2 = "1.0"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
tar = "0.4"
```

and change the `tokio` line to add `time`:

```toml
tokio = { version = "1.53.1", features = ["rt-multi-thread", "macros", "time"] }
```

Run `cargo build` now. If it fails on a `reqwest` feature conflict with serenity's copy, switch `reqwest`'s features to `["json", "rustls-tls-webpki-roots"]`; if it still conflicts, replace `reqwest` with `ureq = { version = "3", features = ["rustls"] }` and make `fetch_latest_tag` / `download_and_stage` blocking (wrap the `run()` calls in `tokio::task::spawn_blocking`). Record which path you took in the commit message.

- [ ] **Step 2: Write failing tests for `install_staged`**

Add to `src/upgrade.rs` `mod tests`:

```rust
    #[test]
    fn install_staged_replaces_the_destination_and_sets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dispatchd");
        std::fs::write(&dest, b"OLD").unwrap();
        let staged = dir.path().join(".dispatchd.upgrade.test");
        std::fs::write(&staged, b"NEW").unwrap();

        install_staged(&staged, &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        assert!(!staged.exists(), "staged file is consumed by the rename");
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn normalize_tag_adds_a_leading_v() {
        assert_eq!(normalize_tag("0.6.0"), "v0.6.0");
        assert_eq!(normalize_tag("v0.6.0"), "v0.6.0");
    }
```

- [ ] **Step 3: Run — expect FAIL**

Run: `cargo test -p dispatchd upgrade::tests::install_staged_replaces_the_destination_and_sets_mode`
Expected: FAIL — `install_staged` / `normalize_tag` not defined.

- [ ] **Step 4: Implement the I/O functions**

Add to `src/upgrade.rs` (above `mod tests`):

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn user_agent() -> String {
    format!("dispatchd/{}", current_version())
}

/// Ensures a leading `v` so release URLs
/// (`/releases/download/v0.6.0/...`) build correctly.
fn normalize_tag(v: &str) -> String {
    if v.starts_with('v') { v.to_string() } else { format!("v{v}") }
}

/// GETs the GitHub `releases/latest` endpoint and returns its `tag_name`.
async fn fetch_latest_tag() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::USER_AGENT, user_agent())
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("failed to reach the GitHub releases API")?
        .error_for_status()
        .context("GitHub releases API returned an error status")?
        .text()
        .await
        .context("failed to read the GitHub releases API response")?;
    parse_latest_tag(&body)
}

pub struct UpgradeCheck {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
}

/// Resolves the latest release tag and compares it to the running version.
pub async fn check() -> Result<UpgradeCheck> {
    let latest = fetch_latest_tag().await?;
    let current = current_version().to_string();
    let update_available = is_newer(&latest, &current);
    Ok(UpgradeCheck { current, latest, update_available })
}

/// Downloads `dispatchd-<target>.tar.gz` + `SHA256SUMS` for `tag`, verifies
/// the checksum, extracts the `dispatchd` entry to a uniquely-named temp
/// file inside `dest_dir` (same filesystem as the real binary, so the
/// later rename is atomic), chmod 0755. Returns the staged path.
async fn download_and_stage(tag: &str, dest_dir: &Path) -> Result<PathBuf> {
    let tag = normalize_tag(tag);
    let asset = asset_name();
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let client = reqwest::Client::new();

    let tarball = client
        .get(format!("{base}/{asset}"))
        .header(reqwest::header::USER_AGENT, user_agent())
        .send()
        .await
        .with_context(|| format!("failed to download {asset} for {tag}"))?
        .error_for_status()
        .with_context(|| format!("no {asset} in release {tag}"))?
        .bytes()
        .await
        .context("failed to read the release tarball")?;

    let sums = client
        .get(format!("{base}/SHA256SUMS"))
        .header(reqwest::header::USER_AGENT, user_agent())
        .send()
        .await
        .context("failed to download SHA256SUMS")?
        .error_for_status()
        .context("SHA256SUMS missing from the release")?
        .text()
        .await
        .context("failed to read SHA256SUMS")?;

    let expected = sha256_for(&sums, &asset)
        .with_context(|| format!("no checksum entry for {asset} in SHA256SUMS"))?;
    if !verify_sha256(&tarball, &expected) {
        anyhow::bail!("checksum verification failed for {asset}");
    }

    let staged = dest_dir.join(format!(".dispatchd.upgrade.{}", std::process::id()));
    {
        let gz = flate2::read::GzDecoder::new(&tarball[..]);
        let mut archive = tar::Archive::new(gz);
        let mut wrote = false;
        for entry in archive.entries().context("corrupt release tarball")? {
            let mut entry = entry.context("corrupt release tarball entry")?;
            let path = entry.path().context("bad path in release tarball")?;
            if path.file_name().and_then(|n| n.to_str()) == Some("dispatchd") {
                let mut out = std::fs::File::create(&staged)
                    .with_context(|| format!("failed to create {}", staged.display()))?;
                std::io::copy(&mut entry, &mut out).context("failed to unpack the binary")?;
                wrote = true;
                break;
            }
        }
        if !wrote {
            anyhow::bail!("release tarball did not contain a `dispatchd` binary");
        }
    }
    set_executable(&staged)?;
    Ok(staged)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).context("failed to chmod the staged binary")
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Atomically moves `staged` onto `dest`. On Linux this is fine even while
/// `dest` is the running binary - the old inode keeps serving until the
/// process restarts (same as `install.sh`). Best-effort `restorecon`.
pub fn install_staged(staged: &Path, dest: &Path) -> Result<()> {
    set_executable(staged)?;
    std::fs::rename(staged, dest).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "cannot write {} - re-run with sudo (sudo dispatchd upgrade)",
                dest.display()
            )
        } else {
            anyhow::anyhow!("failed to install {}: {e}", dest.display())
        }
    })?;
    if which_restorecon() {
        let _ = std::process::Command::new("restorecon").arg(dest).status();
    }
    Ok(())
}

fn which_restorecon() -> bool {
    std::process::Command::new("restorecon")
        .arg("-h")
        .output()
        .map(|_| true)
        .unwrap_or(false)
}

/// The running binary's real path (following the
/// `/usr/local/bin/dispatchd` symlink if there is one).
pub fn resolve_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to resolve dispatchd's own path")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

#[cfg(target_os = "linux")]
fn restart_dispatchd() -> Result<()> {
    let status = std::process::Command::new("systemctl")
        .args(["restart", "dispatchd"])
        .status()
        .context("failed to run `systemctl restart dispatchd`")?;
    if !status.success() {
        anyhow::bail!("`systemctl restart dispatchd` failed ({status})");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn restart_dispatchd() -> Result<()> {
    anyhow::bail!("service restart is only supported on Linux (systemd)")
}
```

- [ ] **Step 5: Implement `UpgradeArgs` + `run()` (normal path)**

Add to `src/upgrade.rs`:

```rust
#[derive(Debug, Clone)]
pub struct UpgradeArgs {
    pub check: bool,
    pub no_restart: bool,
    pub version: Option<String>,
    pub from_request: bool,
}

/// `dispatchd upgrade`. `from_request` (the root helper mode) is handled
/// in `run_from_request`; this function is the interactive/CLI path.
pub async fn run(args: UpgradeArgs) -> Result<()> {
    if args.from_request {
        return run_from_request(args).await;
    }

    let current = current_version().to_string();
    let target = match &args.version {
        Some(v) => normalize_tag(v),
        None => fetch_latest_tag().await?,
    };

    if args.check {
        if args.version.is_some() {
            println!("current: {current}\nrequested: {target}");
        } else if is_newer(&target, &current) {
            println!("current: {current}\nlatest:  {target}\nupdate available - run `dispatchd upgrade`");
        } else {
            println!("current: {current}\nlatest:  {target}\nup to date");
        }
        return Ok(());
    }

    if args.version.is_none() && !is_newer(&target, &current) {
        println!("already on the latest version ({current})");
        return Ok(());
    }

    let exe = resolve_exe()?;
    let dir = exe.parent().context("dispatchd's path has no parent directory")?;

    println!("downloading {} ...", asset_name());
    let staged = download_and_stage(&target, dir).await?;
    println!("verified. installing to {} ...", exe.display());
    install_staged(&staged, &exe)?;
    println!("installed {target}.");

    if args.no_restart {
        println!("run `sudo systemctl restart dispatchd` to apply.");
        return Ok(());
    }
    match restart_dispatchd() {
        Ok(()) => println!("restarted dispatchd.service (now running {target})."),
        Err(e) => {
            println!("binary installed, but the restart didn't run: {e}");
            println!("run `sudo systemctl restart dispatchd` yourself.");
        }
    }
    Ok(())
}

// Placeholder until Task 4; keeps `run()` compiling.
async fn run_from_request(_args: UpgradeArgs) -> Result<()> {
    anyhow::bail!("--from-request is implemented in a later task")
}
```

- [ ] **Step 6: Wire the CLI in `src/main.rs`**

Add to the imports: `use clap::{Args, Parser, Subcommand};`

Add a variant to `enum Command`:

```rust
    /// Download and install the latest dispatchd release
    Upgrade(UpgradeArgs),
```

Add the args struct near the other `#[derive(Subcommand)]` enums:

```rust
#[derive(Args)]
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
    /// Internal: run as the root dispatchd-upgrade.service helper
    #[arg(long, hide = true)]
    from_request: bool,
}

impl From<UpgradeArgs> for upgrade::UpgradeArgs {
    fn from(a: UpgradeArgs) -> Self {
        upgrade::UpgradeArgs {
            check: a.check,
            no_restart: a.no_restart,
            version: a.version,
            from_request: a.from_request,
        }
    }
}
```

Add the dispatch arm in `main`, alongside the others:

```rust
        Some(Command::Upgrade(args)) => return upgrade::run(args.into()).await,
```

- [ ] **Step 7: Run tests + lint + release build**

Run: `cargo test -p dispatchd upgrade::` then `cargo clippy --all-targets` then `cargo fmt --check` then `cargo build --release`
Expected: PASS. `cargo run -- upgrade --check` should print current/latest against the real repo (network permitting) — if offline, it errors cleanly, which is acceptable.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/upgrade.rs src/main.rs
git commit -m "feat: dispatchd upgrade CLI (check, download, verify, swap, restart)"
```

---

## Task 4: `upgrade.rs` `--from-request` helper mode

**Files:**
- Modify: `src/upgrade.rs` (replace the `run_from_request` placeholder; add `RequestGuard`)
- Test: `src/upgrade.rs` `mod tests`

**Interfaces:**
- Consumes: Task 2 (`Request`, `StatusLine`, `REQUEST_PATH`, `STATUS_PATH`), Task 3 (`download_and_stage`, `install_staged`, `resolve_exe`, `fetch_latest_tag`, `normalize_tag`, `restart_dispatchd`).
- Produces:
  - `struct RequestGuard { path: PathBuf }` — `Drop` unlinks `path`; `fn disarm(self)` / take-by-value to skip. Actually simpler: `RequestGuard` always deletes on drop; that's the contract.
  - `async fn run_from_request(args: UpgradeArgs) -> Result<()>` — reads `REQUEST_PATH`, appends `StatusLine`s to `STATUS_PATH`, performs the upgrade, deletes `REQUEST_PATH` unconditionally, restarts unless `request.restart == false`.
  - `fn append_status(line: &StatusLine)` — best-effort append one JSON line + `\n` to `STATUS_PATH`.

- [ ] **Step 1: Write the failing test for the cleanup guard**

Add to `src/upgrade.rs` `mod tests`:

```rust
    #[test]
    fn request_guard_deletes_the_file_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let req = dir.path().join("upgrade.request");
        std::fs::write(&req, "{}").unwrap();
        {
            let _g = RequestGuard::new(req.clone());
            assert!(req.exists());
        }
        assert!(!req.exists(), "guard must unlink the request on drop");
    }

    #[test]
    fn request_guard_drop_is_fine_when_file_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let req = dir.path().join("upgrade.request");
        {
            let _g = RequestGuard::new(req.clone());
            // never created
        }
        assert!(!req.exists());
    }

    #[test]
    fn append_status_writes_one_json_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let status = dir.path().join("upgrade.status");
        append_status_to(&status, &StatusLine::Checking);
        append_status_to(&status, &StatusLine::Verified);
        let parsed = parse_status(&std::fs::read_to_string(&status).unwrap());
        assert_eq!(parsed, vec![StatusLine::Checking, StatusLine::Verified]);
    }
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p dispatchd upgrade::tests::request_guard_deletes_the_file_on_drop`
Expected: FAIL — `RequestGuard` / `append_status_to` undefined.

- [ ] **Step 3: Implement the guard, `append_status`, and `run_from_request`**

Replace the `run_from_request` placeholder in `src/upgrade.rs` with:

```rust
/// Deletes its path on drop - guarantees `REQUEST_PATH` is removed on
/// every exit path of the helper, so the `.path` unit re-arms instead of
/// re-triggering in a loop.
struct RequestGuard {
    path: PathBuf,
}

impl RequestGuard {
    fn new(path: PathBuf) -> Self {
        RequestGuard { path }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Appends one `StatusLine` as JSON + newline to `path`. Best-effort - a
/// failed status write must not abort the upgrade.
fn append_status_to(path: &Path, line: &StatusLine) {
    use std::io::Write;
    if let Ok(json) = serde_json::to_string(line) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{json}");
        }
    }
}

fn append_status(line: &StatusLine) {
    append_status_to(Path::new(STATUS_PATH), line);
}

/// The `dispatchd-upgrade.service` (root oneshot) entry point. Reads the
/// request `/admin upgrade` wrote, streams progress to `STATUS_PATH`, does
/// the upgrade, and (unless the request said not to) restarts the bot.
/// `REQUEST_PATH` is deleted no matter how this returns.
async fn run_from_request(_args: UpgradeArgs) -> Result<()> {
    let _guard = RequestGuard::new(PathBuf::from(REQUEST_PATH));

    let raw = std::fs::read_to_string(REQUEST_PATH)
        .with_context(|| format!("no upgrade request at {REQUEST_PATH}"))?;
    let request: Request = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(e) => {
            append_status(&StatusLine::Error {
                message: format!("unreadable upgrade request: {e}"),
                channel_id: String::new(),
            });
            anyhow::bail!("unreadable upgrade request: {e}");
        }
    };

    // Fresh status file for this run.
    let _ = std::fs::write(STATUS_PATH, b"");
    let chan = request.channel_id.clone();

    let result = perform_from_request(&request).await;
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            append_status(&StatusLine::Error { message: e.to_string(), channel_id: chan });
            Err(e)
        }
    }
}

async fn perform_from_request(request: &Request) -> Result<()> {
    let current = current_version().to_string();

    append_status(&StatusLine::Checking);
    let target = match &request.target_version {
        Some(v) => normalize_tag(v),
        None => fetch_latest_tag().await?,
    };
    append_status(&StatusLine::Found { current: current.clone(), latest: target.clone() });

    if request.target_version.is_none() && !is_newer(&target, &current) {
        append_status(&StatusLine::Done {
            from: current.clone(),
            to: current.clone(),
            channel_id: request.channel_id.clone(),
            requested_by: request.requested_by.clone(),
            requested_by_name: request.requested_by_name.clone(),
            noop: true,
        });
        return Ok(());
    }

    let exe = resolve_exe()?;
    let dir = exe.parent().context("dispatchd's path has no parent directory")?;

    append_status(&StatusLine::Downloading { asset: asset_name() });
    let staged = download_and_stage(&target, dir).await?;
    append_status(&StatusLine::Verified);
    install_staged(&staged, &exe)?;
    append_status(&StatusLine::Swapped);

    append_status(&StatusLine::Restarting);
    // Delete the request now, before the restart, so a crash mid-restart
    // can't leave a re-triggering request behind. The guard is a backstop.
    let _ = std::fs::remove_file(REQUEST_PATH);

    append_status(&StatusLine::Done {
        from: current,
        to: target.trim_start_matches('v').to_string(),
        channel_id: request.channel_id.clone(),
        requested_by: request.requested_by.clone(),
        requested_by_name: request.requested_by_name.clone(),
        noop: false,
    });

    if request.restart {
        restart_dispatchd()?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p dispatchd upgrade::` then `cargo clippy --all-targets`
Expected: PASS, clippy clean. (`_args` is unused in `run_from_request` — prefix keeps clippy quiet; leave it, the signature is symmetric with `run`.)

- [ ] **Step 5: Commit**

```bash
git add src/upgrade.rs
git commit -m "feat: dispatchd upgrade --from-request helper mode with request cleanup guard"
```

---

## Task 5: systemd privilege bridge in `service.rs`

**Files:**
- Modify: `src/service.rs` — new constants; `render_unit` gains `RuntimeDirectory=`; `render_upgrade_service` / `render_upgrade_path`; `install()` writes + enables the two units.
- Test: `src/service.rs` `mod tests`

**Interfaces:**
- Consumes: nothing from other tasks (`upgrade::REQUEST_PATH` value is duplicated here as a literal in the `.path` unit string — that's fine, systemd units are strings).
- Produces:
  - `fn render_upgrade_service(exe_path: &str) -> String`
  - `fn render_upgrade_path() -> &'static str`
  - `pub(crate) const UPGRADE_PATH_PATH: &str = "/etc/systemd/system/dispatchd-upgrade.path"` (Task 6 reads it via `Path::exists`)
  - `render_unit` output now contains `RuntimeDirectory=dispatchd` and `RuntimeDirectoryPreserve=yes`

- [ ] **Step 1: Write failing tests**

Add to `src/service.rs` `mod tests`:

```rust
    #[test]
    fn render_unit_declares_a_preserved_runtime_directory() {
        let unit = render_unit("/usr/local/bin/dispatchd", "pi");
        assert!(unit.contains("RuntimeDirectory=dispatchd"));
        assert!(unit.contains("RuntimeDirectoryPreserve=yes"));
    }

    #[test]
    fn render_upgrade_service_is_a_rootless_oneshot_helper() {
        let unit = render_upgrade_service("/usr/local/bin/dispatchd");
        assert!(unit.contains("ExecStart=/usr/local/bin/dispatchd upgrade --from-request"));
        assert!(unit.contains("Type=oneshot"));
        assert!(!unit.contains("User="), "helper runs as root");
        assert!(!unit.contains("[Install]"), "only ever started by the .path unit");
    }

    #[test]
    fn render_upgrade_path_watches_the_request_file() {
        let path = render_upgrade_path();
        assert!(path.contains("PathExists=/run/dispatchd/upgrade.request"));
        assert!(path.contains("Unit=dispatchd-upgrade.service"));
        assert!(path.contains("WantedBy=paths.target"));
    }
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p dispatchd service::tests::render_upgrade_path_watches_the_request_file`
Expected: FAIL — functions undefined.

- [ ] **Step 3: Add constants**

`src/service.rs`, near the other path consts:

```rust
const UPGRADE_SERVICE_PATH: &str = "/etc/systemd/system/dispatchd-upgrade.service";
pub(crate) const UPGRADE_PATH_PATH: &str = "/etc/systemd/system/dispatchd-upgrade.path";
```

- [ ] **Step 4: Add `RuntimeDirectory=` to `render_unit`**

In `render_unit`, inside the `[Service]` block, add the two lines after `LoadCredentialEncrypted=...`:

```rust
         LoadCredentialEncrypted=discord_token:{CRED_PATH}\n\
         RuntimeDirectory=dispatchd\n\
         RuntimeDirectoryPreserve=yes\n\
         Restart=on-failure\n\
```

Update the `render_unit` doc comment to note: `RuntimeDirectory=dispatchd` gives the unprivileged bot a writable `/run/dispatchd` for the `/admin upgrade` request/status files; `Preserve=yes` keeps it across the upgrade restart.

- [ ] **Step 5: Add the two renderers**

`src/service.rs`, after `render_maintenance_timer`:

```rust
/// The root oneshot the `dispatchd-upgrade.path` unit triggers when
/// `/admin upgrade` drops a request file. No `User=` - it needs root to
/// overwrite the binary in `/usr/local/bin` and `systemctl restart`. No
/// `[Install]` section: it is never enabled or started directly.
fn render_upgrade_service(exe_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=dispatchd self-upgrade (triggered by /admin upgrade)\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exe_path} upgrade --from-request\n"
    )
}

/// Watches for the request file the unprivileged bot writes into its
/// `RuntimeDirectory`. When it appears, systemd starts
/// `dispatchd-upgrade.service`; the helper deletes the file when done, so
/// this re-arms.
fn render_upgrade_path() -> &'static str {
    "[Unit]\n\
     Description=Watch for a dispatchd upgrade request\n\
     \n\
     [Path]\n\
     PathExists=/run/dispatchd/upgrade.request\n\
     Unit=dispatchd-upgrade.service\n\
     \n\
     [Install]\n\
     WantedBy=paths.target\n"
}
```

- [ ] **Step 6: Write + enable the units in `install()`**

In `install()` (the `#[cfg(target_os = "linux")]` one), after the block that writes the maintenance service/timer and before `run_systemctl(&["daemon-reload"])?;`:

```rust
    std::fs::write(UPGRADE_SERVICE_PATH, render_upgrade_service(exe))
        .with_context(|| format!("failed to write {UPGRADE_SERVICE_PATH}"))?;
    std::fs::write(UPGRADE_PATH_PATH, render_upgrade_path())
        .with_context(|| format!("failed to write {UPGRADE_PATH_PATH}"))?;
    println!("wrote {UPGRADE_SERVICE_PATH} and {UPGRADE_PATH_PATH}");
```

After the existing `run_systemctl(&["enable", "--now", "dispatchd-maintenance.timer"])?;`:

```rust
    // Path-activated: sits idle until `/admin upgrade` writes a request.
    run_systemctl(&["enable", "--now", "dispatchd-upgrade.path"])?;
```

And add to the closing `println!` block:

```rust
    println!("dispatchd-upgrade.path installed and started (enables /admin upgrade).");
```

- [ ] **Step 7: Run tests + lint**

Run: `cargo test -p dispatchd service::` then `cargo clippy --all-targets` then `cargo fmt --check`
Expected: PASS. The pre-existing `render_unit_interpolates_exe_path_and_user` test still passes (it only asserts `contains`).

- [ ] **Step 8: Commit**

```bash
git add src/service.rs
git commit -m "feat: install dispatchd-upgrade.path/.service bridge + RuntimeDirectory"
```

---

## Task 6: structured status (`service.rs` + `discord_login.rs`)

**Files:**
- Modify: `src/service.rs` — `ServiceStatus`, `status_report()`, `format_status()`; rewrite `status()` as a formatter.
- Modify: `src/discord_login.rs` — `DiscordPing`, `ping_report()`, `format_ping()`; rewrite `ping()` as a formatter.
- Modify: `src/main.rs` — `run_status()` still calls `service::status()` + `discord_login::ping()` (no change needed if those keep their signatures).
- Test: both files' `mod tests`.

**Interfaces:**
- Consumes: Task 5 (`UPGRADE_PATH_PATH`).
- Produces:
  - `src/service.rs`: `pub struct ServiceStatus { pub systemd_version: Option<u32>, pub min_systemd_version: u32, pub unit_installed: bool, pub unit_enabled: Option<String>, pub unit_active: Option<String>, pub upgrade_helper_installed: bool, pub cred_present: bool }`; `#[cfg(target_os = "linux")] pub fn status_report() -> ServiceStatus` + non-Linux stub returning an all-`None`/`false` value with `min_systemd_version: MIN_SYSTEMD_VERSION`; `pub fn format_status(r: &ServiceStatus) -> String`.
  - `src/discord_login.rs`: `pub struct DiscordPing { pub token_found: bool, pub result: Option<Result<PingOk, String>> }` where `pub struct PingOk { pub name: String, pub id: String, pub latency_ms: u128 }`; `pub async fn ping_report() -> DiscordPing`; `pub fn format_ping(p: &DiscordPing) -> String`.

- [ ] **Step 1: Write failing `format_status` tests**

Add to `src/service.rs` `mod tests`:

```rust
    fn base_status() -> ServiceStatus {
        ServiceStatus {
            systemd_version: Some(252),
            min_systemd_version: MIN_SYSTEMD_VERSION,
            unit_installed: true,
            unit_enabled: Some("enabled".into()),
            unit_active: Some("active".into()),
            upgrade_helper_installed: true,
            cred_present: true,
        }
    }

    #[test]
    fn format_status_reports_a_healthy_install() {
        let out = format_status(&base_status());
        assert!(out.contains("dispatchd.service:"));
        assert!(out.contains("enabled=enabled"));
        assert!(out.contains("active=active"));
        assert!(out.contains("upgrade helper:") && out.contains("installed"));
        assert!(out.contains("encrypted credential present"));
    }

    #[test]
    fn format_status_reports_missing_pieces() {
        let s = ServiceStatus {
            unit_installed: false,
            unit_enabled: None,
            unit_active: None,
            upgrade_helper_installed: false,
            cred_present: false,
            ..base_status()
        };
        let out = format_status(&s);
        assert!(out.contains("not installed"));
        assert!(out.contains("service install"));
    }
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p dispatchd service::tests::format_status_reports_a_healthy_install`
Expected: FAIL — `ServiceStatus` / `format_status` undefined.

- [ ] **Step 3: Implement `ServiceStatus`, `status_report`, `format_status`**

`src/service.rs`. Add the struct (module-level):

```rust
pub struct ServiceStatus {
    pub systemd_version: Option<u32>,
    pub min_systemd_version: u32,
    pub unit_installed: bool,
    pub unit_enabled: Option<String>,
    pub unit_active: Option<String>,
    pub upgrade_helper_installed: bool,
    pub cred_present: bool,
}
```

Replace the body of the Linux `status()` with a `status_report()` that gathers, plus a `format_status()`:

```rust
#[cfg(target_os = "linux")]
pub fn status_report() -> ServiceStatus {
    let unit_installed = std::path::Path::new(UNIT_PATH).exists();
    ServiceStatus {
        systemd_version: systemd_version().ok(),
        min_systemd_version: MIN_SYSTEMD_VERSION,
        unit_installed,
        unit_enabled: unit_installed
            .then(|| systemctl_query(&["is-enabled", "dispatchd.service"])),
        unit_active: unit_installed
            .then(|| systemctl_query(&["is-active", "dispatchd.service"])),
        upgrade_helper_installed: std::path::Path::new(UPGRADE_PATH_PATH).exists(),
        cred_present: std::path::Path::new(CRED_PATH).exists(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn status_report() -> ServiceStatus {
    ServiceStatus {
        systemd_version: None,
        min_systemd_version: MIN_SYSTEMD_VERSION,
        unit_installed: false,
        unit_enabled: None,
        unit_active: None,
        upgrade_helper_installed: false,
        cred_present: false,
    }
}

pub fn format_status(r: &ServiceStatus) -> String {
    let mut out = String::from("systemd:\n");
    match r.systemd_version {
        Some(v) if v >= r.min_systemd_version => {
            out.push_str(&format!("  version:              {v} (>= {}, ok)\n", r.min_systemd_version))
        }
        Some(v) => out.push_str(&format!(
            "  version:              {v} (< {} - token encryption unavailable)\n",
            r.min_systemd_version
        )),
        None => out.push_str("  version:              unknown\n"),
    }
    if r.unit_installed {
        out.push_str(&format!(
            "  dispatchd.service:    installed, enabled={}, active={}\n",
            r.unit_enabled.as_deref().unwrap_or("unknown"),
            r.unit_active.as_deref().unwrap_or("unknown"),
        ));
    } else {
        out.push_str("  dispatchd.service:    not installed - run: sudo dispatchd service install\n");
    }
    if r.upgrade_helper_installed {
        out.push_str("  upgrade helper:       installed\n");
    } else {
        out.push_str("  upgrade helper:       not installed - run: sudo dispatchd service install\n");
    }
    if r.cred_present {
        out.push_str(&format!("  discord token:        encrypted credential present ({CRED_PATH})\n"));
    } else {
        out.push_str("  discord token:        not set - run: sudo dispatchd discord login\n");
    }
    out
}
```

Now make the CLI `status()` a thin wrapper. Replace the Linux `status()` fn and the non-Linux stub with:

```rust
pub fn status() -> anyhow::Result<()> {
    print!("{}", format_status(&status_report()));
    Ok(())
}
```

(Delete the old `#[cfg(not(target_os = "linux"))] pub fn status()` — the single cross-platform wrapper above replaces both, since `status_report()` is the thing that's `#[cfg]`-split now. Keep `systemctl_query` and `systemd_version` Linux-only as they are.)

- [ ] **Step 4: Write failing `format_ping` tests**

Add to `src/discord_login.rs` `mod tests`:

```rust
    #[test]
    fn format_ping_ok() {
        let p = DiscordPing {
            token_found: true,
            result: Some(Ok(PingOk { name: "bot".into(), id: "42".into(), latency_ms: 84 })),
        };
        let out = format_ping(&p);
        assert!(out.contains("ok - logged in as bot (42), 84ms"));
    }

    #[test]
    fn format_ping_failed() {
        let p = DiscordPing { token_found: true, result: Some(Err("401 Unauthorized".into())) };
        assert!(format_ping(&p).contains("failed - 401 Unauthorized"));
    }

    #[test]
    fn format_ping_no_token() {
        let p = DiscordPing { token_found: false, result: None };
        assert!(format_ping(&p).contains("not found"));
    }
```

- [ ] **Step 5: Run — expect FAIL**

Run: `cargo test -p dispatchd discord_login::tests::format_ping_ok`
Expected: FAIL — types undefined.

- [ ] **Step 6: Implement `DiscordPing`, `ping_report`, `format_ping`**

`src/discord_login.rs`:

```rust
pub struct PingOk {
    pub name: String,
    pub id: String,
    pub latency_ms: u128,
}

pub struct DiscordPing {
    pub token_found: bool,
    pub result: Option<Result<PingOk, String>>,
}

pub async fn ping_report() -> DiscordPing {
    let token = match crate::discord_token().or_else(|| decrypt_cred_file(crate::service::CRED_PATH)) {
        Some(t) => t,
        None => return DiscordPing { token_found: false, result: None },
    };
    let start = std::time::Instant::now();
    let http = serenity::http::Http::new(&token);
    let result = match http.get_current_user().await {
        Ok(user) => Ok(PingOk {
            name: user.name.clone(),
            id: user.id.to_string(),
            latency_ms: start.elapsed().as_millis(),
        }),
        Err(e) => Err(e.to_string()),
    };
    DiscordPing { token_found: true, result: Some(result) }
}

pub fn format_ping(p: &DiscordPing) -> String {
    let mut out = String::from("discord:\n");
    if !p.token_found {
        out.push_str("  token:                not found - run: sudo dispatchd discord login\n");
        return out;
    }
    match &p.result {
        Some(Ok(ok)) => out.push_str(&format!(
            "  ping:                 ok - logged in as {} ({}), {}ms\n",
            ok.name, ok.id, ok.latency_ms
        )),
        Some(Err(e)) => out.push_str(&format!("  ping:                 failed - {e}\n")),
        None => out.push_str("  ping:                 not attempted\n"),
    }
    out
}
```

Replace `ping()`'s body with:

```rust
pub async fn ping() {
    print!("{}", format_ping(&ping_report().await));
}
```

- [ ] **Step 7: Run tests + lint + release build**

Run: `cargo test -p dispatchd service:: discord_login::` then `cargo clippy --all-targets` then `cargo fmt --check` then `cargo build --release`
Expected: PASS. `cargo run -- status` output matches the old format plus the new `upgrade helper:` line.

- [ ] **Step 8: Commit**

```bash
git add src/service.rs src/discord_login.rs
git commit -m "refactor: structured status/ping reports for reuse by /admin status"
```

---

## Task 7: `/admin` command — `status` + `help`

**Files:**
- Create: `src/discord/admin.rs`
- Modify: `src/discord/mod.rs` — `mod admin;`, register `admin::command()`, dispatch `"admin"` (status + help only for now).
- Modify: `src/discord/help.rs` — add `/admin` lines + test needles.
- Test: `src/discord/admin.rs` `mod tests`.

**Interfaces:**
- Consumes: Task 1 (`members::is_admin`), Task 6 (`service::status_report`/`format_status`, `discord_login::ping_report`/`format_ping`), Task 3 (`upgrade::check`, `upgrade::current_version`).
- Produces (in `crate::discord::admin`):
  - `pub fn command() -> CreateCommand`
  - `pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])>`
  - `pub async fn handle_status(ctx, command, db)`
  - `pub async fn handle_help(ctx, command)`
  - `fn permission_denied_reply() -> &'static str`
  - `fn format_admin_status(svc: &ServiceStatus, ping: &DiscordPing, version_line: &str) -> String`
  - `fn version_line(check: Result<upgrade::UpgradeCheck, String>) -> String`

- [ ] **Step 1: Write failing tests**

Create `src/discord/admin.rs` with just the test module first (so it compiles once stubs exist), or write tests alongside. Add to the file's eventual `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_names_the_admin_role() {
        let msg = permission_denied_reply();
        assert!(msg.contains("admin"));
        assert!(msg.starts_with('⛔'));
    }

    #[test]
    fn version_line_flags_an_available_update() {
        let line = version_line(Ok(crate::upgrade::UpgradeCheck {
            current: "0.5.0".into(),
            latest: "v0.6.0".into(),
            update_available: true,
        }));
        assert!(line.contains("0.5.0"));
        assert!(line.contains("0.6.0"));
        assert!(line.to_lowercase().contains("update available"));
    }

    #[test]
    fn version_line_says_up_to_date() {
        let line = version_line(Ok(crate::upgrade::UpgradeCheck {
            current: "0.6.0".into(),
            latest: "v0.6.0".into(),
            update_available: false,
        }));
        assert!(line.to_lowercase().contains("up to date"));
    }

    #[test]
    fn version_line_degrades_on_error() {
        let line = version_line(Err("network unreachable".into()));
        assert!(line.to_lowercase().contains("unknown"));
    }

    #[test]
    fn subcommand_unwraps_the_nested_options() {
        // mirrors team.rs's subcommand test style; build a SubCommand option
        // and assert the name comes back. (Use the same helper shape as
        // team.rs tests if present; otherwise a direct CommandDataOption.)
    }
}
```

For `subcommand_unwraps_the_nested_options`: check `src/discord/team.rs`'s test module for an existing pattern; if `team.rs` has no such test, drop this test case (the function is a 3-line copy of `team::subcommand` and is covered there).

- [ ] **Step 2: Write `src/discord/admin.rs`**

```rust
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serenity::all::{
    CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    Context as SerenityContext, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, Permissions,
};

use crate::discord_login::{self, DiscordPing};
use crate::service::{self, ServiceStatus};
use crate::{members, upgrade};

const ADMIN_HELP_TEXT: &str = "\
**/admin subcommands** (bot-operator only)
`/admin status` - systemd + Discord health and the version check
`/admin upgrade [version] [restart]` - upgrade dispatchd to the latest release
`/admin help` - show this message";

pub fn command() -> CreateCommand {
    CreateCommand::new("admin")
        .description("Bot-operator tools: status and self-upgrade")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "status",
            "systemd + Discord health and the version check",
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "upgrade",
                "Upgrade dispatchd to the latest release",
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "version",
                    "Install a specific tag (allows downgrade), e.g. v0.4.0",
                )
                .required(false),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::Boolean,
                    "restart",
                    "Restart the service after upgrading (default true)",
                )
                .required(false),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "help",
            "Show /admin's subcommands",
        ))
}

/// `(subcommand_name, its_options)` - same shape as `team::subcommand`.
pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])> {
    let top = options.first()?;
    match &top.value {
        CommandDataOptionValue::SubCommand(nested) => Some((top.name.as_str(), nested)),
        _ => None,
    }
}

fn permission_denied_reply() -> &'static str {
    "⛔ This command is restricted to bot operators (the `admin` role in members.toml)."
}

fn version_line(check: Result<upgrade::UpgradeCheck, String>) -> String {
    match check {
        Ok(c) if c.update_available => format!(
            "  version:              {} — update available: {}",
            c.current,
            c.latest.trim_start_matches('v')
        ),
        Ok(c) => format!("  version:              {} (up to date)", c.current),
        Err(e) => format!(
            "  version:              {} — latest unknown ({e})",
            upgrade::current_version()
        ),
    }
}

fn format_admin_status(svc: &ServiceStatus, ping: &DiscordPing, version_line: &str) -> String {
    format!(
        "```\n{}{}\n{}```",
        service::format_status(svc),
        version_line,
        discord_login::format_ping(ping),
    )
}

async fn ephemeral(ctx: &SerenityContext, command: &CommandInteraction, content: impl Into<String>) {
    let reply = CreateInteractionResponseMessage::new().content(content).ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /admin: {e}");
    }
}

pub async fn handle_help(ctx: &SerenityContext, command: &CommandInteraction) {
    ephemeral(ctx, command, ADMIN_HELP_TEXT).await;
}

pub async fn handle_status(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
) {
    let discord_user_id = command.user.id.to_string();
    {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_admin(&conn, &discord_user_id) {
            Ok(true) => {}
            Ok(false) => return ephemeral(ctx, command, permission_denied_reply()).await,
            Err(e) => {
                eprintln!("failed to check is_admin: {e}");
                return ephemeral(ctx, command, "⚠️ Something went wrong checking permissions.").await;
            }
        }
    }

    let svc = service::status_report();
    let ping = discord_login::ping_report().await;
    let check = upgrade::check().await.map_err(|e| e.to_string());
    let body = format_admin_status(&svc, &ping, &version_line(check));
    ephemeral(ctx, command, body).await;
}
```

- [ ] **Step 3: Register in `src/discord/mod.rs`**

- Add `mod admin;` at the top with the other `mod` lines.
- In `ready`, add `admin::command()` to the `commands` vec.
- In `interaction_create`'s `Interaction::Command` match, add an arm:

```rust
                "admin" => match admin::subcommand(&command.data.options) {
                    Some(("status", _)) => admin::handle_status(&ctx, &command, &self.db).await,
                    Some(("help", _)) => admin::handle_help(&ctx, &command).await,
                    _ => {}
                },
```

(The `"upgrade"` sub-arm is added in Task 8.)

- [ ] **Step 4: Update `src/discord/help.rs`**

Add before the `/ping` line in `HELP_TEXT`:

```
`/admin status` - (admin only) systemd + Discord health and version check
`/admin upgrade` - (admin only) upgrade dispatchd to the latest release
`/admin help` - admin-specific help
```

Add to the test's needle array: `"/admin status"`, `"/admin upgrade"`, `"/admin help"`.

- [ ] **Step 5: Run tests + lint + release build**

Run: `cargo test -p dispatchd admin:: help::` then `cargo clippy --all-targets` then `cargo fmt --check` then `cargo build --release`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/discord/admin.rs src/discord/mod.rs src/discord/help.rs
git commit -m "feat: /admin status + /admin help (admin-gated)"
```

---

## Task 8: `/admin upgrade` + post-restart confirmation

**Files:**
- Modify: `src/discord/admin.rs` — `handle_upgrade`, `post_upgrade_confirmation`, the step-list renderer.
- Modify: `src/discord/mod.rs` — dispatch `"admin" … "upgrade"`; call `admin::post_upgrade_confirmation(&ctx)` at the end of `ready`.
- Test: `src/discord/admin.rs` `mod tests` — the step-list renderer.

**Interfaces:**
- Consumes: Task 2 (`upgrade::REQUEST_PATH`, `STATUS_PATH`, `RUN_DIR`, `Request`, `StatusLine`, `parse_status`, `Version`), Task 3 (`upgrade::current_version`).
- Produces:
  - `pub async fn handle_upgrade(ctx, command, options: &[CommandDataOption], db)`
  - `pub async fn post_upgrade_confirmation(ctx: &SerenityContext)`
  - `fn render_steps(lines: &[StatusLine]) -> String`

- [ ] **Step 1: Write the failing renderer test**

Add to `src/discord/admin.rs` `mod tests`:

```rust
    #[test]
    fn render_steps_marks_done_and_running_lines() {
        use crate::upgrade::StatusLine;
        let lines = vec![
            StatusLine::Checking,
            StatusLine::Found { current: "0.5.0".into(), latest: "v0.6.0".into() },
            StatusLine::Downloading { asset: "dispatchd-x.tar.gz".into() },
            StatusLine::Verified,
            StatusLine::Swapped,
            StatusLine::Restarting,
        ];
        let out = render_steps(&lines);
        assert!(out.contains("✓"));
        assert!(out.contains("0.6.0"));
        assert!(out.contains("Restarting"));
    }

    #[test]
    fn render_steps_shows_errors() {
        use crate::upgrade::StatusLine;
        let out = render_steps(&[StatusLine::Error {
            message: "checksum verification failed".into(),
            channel_id: "1".into(),
        }]);
        assert!(out.contains("❌"));
        assert!(out.contains("checksum verification failed"));
    }
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p dispatchd admin::tests::render_steps_marks_done_and_running_lines`
Expected: FAIL — `render_steps` undefined.

- [ ] **Step 3: Implement `render_steps`, `handle_upgrade`, `post_upgrade_confirmation`**

Add to `src/discord/admin.rs`:

```rust
use std::path::Path;

use serenity::all::{ChannelId, CreateMessage, EditInteractionResponse};

use crate::upgrade::{Request, StatusLine};

/// Renders the tail of the helper's progress into the ephemeral reply.
fn render_steps(lines: &[StatusLine]) -> String {
    let mut out = String::from("**Upgrade progress**\n");
    for line in lines {
        match line {
            StatusLine::Checking => out.push_str("✓ checking for updates\n"),
            StatusLine::Found { current, latest } => {
                out.push_str(&format!("✓ latest is {} (current {current})\n", latest.trim_start_matches('v')))
            }
            StatusLine::Downloading { asset } => out.push_str(&format!("✓ downloading {asset}\n")),
            StatusLine::Verified => out.push_str("✓ checksum verified\n"),
            StatusLine::Swapped => out.push_str("✓ binary swapped\n"),
            StatusLine::Restarting => {
                out.push_str("↻ Restarting dispatchd now — the new instance will confirm in this channel.\n")
            }
            StatusLine::Done { noop: true, .. } => {
                out.push_str("✅ Already on the latest version.\n")
            }
            StatusLine::Done { from, to, .. } => {
                out.push_str(&format!("✅ upgraded {from} → {to}\n"))
            }
            StatusLine::Error { message, .. } => out.push_str(&format!("❌ {message}\n")),
        }
    }
    out
}

pub async fn handle_upgrade(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
) {
    let discord_user_id = command.user.id.to_string();
    {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_admin(&conn, &discord_user_id) {
            Ok(true) => {}
            Ok(false) => return ephemeral(ctx, command, permission_denied_reply()).await,
            Err(e) => {
                eprintln!("failed to check is_admin: {e}");
                return ephemeral(ctx, command, "⚠️ Something went wrong checking permissions.").await;
            }
        }
    }

    if !Path::new(upgrade::RUN_DIR).exists() {
        return ephemeral(
            ctx,
            command,
            "⚠️ The upgrade helper isn't installed. Run `sudo dispatchd service install` on the host once, then retry.",
        )
        .await;
    }
    if Path::new(upgrade::REQUEST_PATH).exists() {
        return ephemeral(ctx, command, "⚠️ An upgrade is already in progress.").await;
    }

    let version = super::get_option_string(options, "version");
    let restart = options
        .iter()
        .find(|o| o.name == "restart")
        .and_then(|o| match o.value {
            CommandDataOptionValue::Boolean(b) => Some(b),
            _ => None,
        })
        .unwrap_or(true);

    ephemeral(ctx, command, "🔎 Checking for updates…").await;

    let request = Request {
        requested_by: discord_user_id,
        requested_by_name: command.user.name.clone(),
        channel_id: command.channel_id.to_string(),
        target_version: version,
        restart,
        requested_at: chrono::Utc::now().to_rfc3339(),
    };
    let tmp = format!("{}/.upgrade.request.{}", upgrade::RUN_DIR, std::process::id());
    let write = serde_json::to_string(&request)
        .map_err(|e| e.to_string())
        .and_then(|json| std::fs::write(&tmp, json).map_err(|e| e.to_string()))
        .and_then(|()| std::fs::rename(&tmp, upgrade::REQUEST_PATH).map_err(|e| e.to_string()));
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return edit(ctx, command, format!("⚠️ Couldn't queue the upgrade: {e}")).await;
    }

    // Poll the helper's status file for up to ~120s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let lines = std::fs::read_to_string(upgrade::STATUS_PATH)
            .map(|c| upgrade::parse_status(&c))
            .unwrap_or_default();

        if !lines.is_empty() {
            edit(ctx, command, render_steps(&lines)).await;
        }
        if lines.iter().any(|l| matches!(l, StatusLine::Restarting) || l.is_terminal()) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            edit(
                ctx,
                command,
                "⚠️ No progress after 120s — check `journalctl -u dispatchd-upgrade` on the host.",
            )
            .await;
            return;
        }
    }
}

async fn edit(ctx: &SerenityContext, command: &CommandInteraction, content: impl Into<String>) {
    if let Err(e) = command
        .edit_response(&ctx.http, EditInteractionResponse::new().content(content))
        .await
    {
        eprintln!("failed to edit /admin upgrade reply: {e}");
    }
}

/// Called once on `ready`. If the previous instance was upgraded via
/// `/admin upgrade`, post a confirmation into the requesting channel and
/// clear the status/request files so it fires exactly once.
pub async fn post_upgrade_confirmation(ctx: &SerenityContext) {
    let Ok(contents) = std::fs::read_to_string(upgrade::STATUS_PATH) else {
        return;
    };
    let lines = upgrade::parse_status(&contents);
    let terminal = lines.iter().rev().find(|l| l.is_terminal());

    let msg = match terminal {
        Some(StatusLine::Done { from, to, channel_id, requested_by, noop: false, .. })
            if !channel_id.is_empty() =>
        {
            let running = upgrade::current_version();
            if upgrade::Version::parse(to) == upgrade::Version::parse(running) {
                Some((
                    channel_id.clone(),
                    format!("✅ dispatchd upgraded v{from} → v{to} (requested by <@{requested_by}>)."),
                ))
            } else {
                Some((
                    channel_id.clone(),
                    format!(
                        "⚠️ dispatchd restarted but the upgrade may not have completed — running v{running}. Check `journalctl -u dispatchd-upgrade`."
                    ),
                ))
            }
        }
        Some(StatusLine::Error { message, channel_id }) if !channel_id.is_empty() => Some((
            channel_id.clone(),
            format!("❌ dispatchd upgrade failed: {message}"),
        )),
        _ => None,
    };

    if let Some((channel_id, text)) = msg {
        if let Ok(raw) = channel_id.parse::<u64>() {
            if let Err(e) = ChannelId::new(raw)
                .send_message(&ctx.http, CreateMessage::new().content(text))
                .await
            {
                eprintln!("failed to post upgrade confirmation: {e}");
            }
        }
    }

    let _ = std::fs::remove_file(upgrade::STATUS_PATH);
    let _ = std::fs::remove_file(upgrade::REQUEST_PATH);
}
```

Note: `Version` must be `pub` in `upgrade.rs` — change `pub struct Version` (already `pub` per Task 2 interface) and confirm `pub fn parse`. Also make `StatusLine`'s `Done`/`Error` fields accessible (they are, being an enum). Add `use serde` already present.

- [ ] **Step 4: Dispatch + startup hook in `src/discord/mod.rs`**

Add the `"upgrade"` arm to the `"admin"` match:

```rust
                    Some(("upgrade", opts)) => {
                        admin::handle_upgrade(&ctx, &command, opts, &self.db).await
                    }
```

At the end of the `ready` fn (after `set_commands`):

```rust
        admin::post_upgrade_confirmation(&ctx).await;
```

- [ ] **Step 5: `pub` the `Version` type + confirm `tokio` `time` feature**

In `src/upgrade.rs` ensure `pub struct Version(...)` and `pub fn parse`. Confirm `Cargo.toml` `tokio` has `"time"` (added in Task 3 — if you took the `ureq` fallback there, it's still needed here for `tokio::time::sleep`).

- [ ] **Step 6: Run tests + lint + release build**

Run: `cargo test -p dispatchd admin:: upgrade::` then `cargo clippy --all-targets` then `cargo fmt --check` then `cargo build --release`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/discord/admin.rs src/discord/mod.rs src/upgrade.rs Cargo.toml
git commit -m "feat: /admin upgrade with streamed progress + post-restart confirmation"
```

---

## Task 9: Documentation

**Files:**
- Modify: `docs/installing.md`, `docs/user-guide.md`, `docs/discord-setup.md`, `CLAUDE.md`

No tests. One commit.

- [ ] **Step 1: `docs/installing.md` — "Upgrading" section**

At the top of the `## Upgrading` section, before the "Re-run the installer" text, add:

```markdown
### `dispatchd upgrade` (recommended)

```sh
sudo dispatchd upgrade            # download latest, verify, swap, restart
sudo dispatchd upgrade --check    # report current vs latest, do nothing
sudo dispatchd upgrade --no-restart
sudo dispatchd upgrade --version v0.4.0   # pin / downgrade
```

`dispatchd upgrade` resolves the latest GitHub release, and if it's newer
than the running binary, downloads the right prebuilt for this machine,
verifies its SHA-256 against the release `SHA256SUMS`, swaps it in place,
and restarts `dispatchd.service`. `--no-restart` skips the restart (it
prints the command). It needs root to write `/usr/local/bin/dispatchd`
and to restart the service, hence `sudo`.

As with the installer, this covers the "binary changed" case. If a
release note says the **systemd unit itself** changed, still run
`sudo dispatchd service install` (see below).
```

Keep the existing `curl | sh` method as the paragraph that follows ("Or re-run the installer:").

Add one line to the `sudo dispatchd service install` exception paragraph: "The `dispatchd-upgrade.path`/`.service` helper units (which back Discord's `/admin upgrade`) are also (re)written by `service install` — an existing deployment needs one `sudo dispatchd service install` after upgrading to the version that introduced them."

- [ ] **Step 2: `docs/user-guide.md` — new section**

Add a section (after the tech-lead `/team` material):

```markdown
## Bot operator — the `admin` role

A member with `role = "admin"` in `members.toml` has **every `/team`
capability** (they count as a tech lead for all of those) **plus** the
`/admin` command group. `/admin` is for whoever operates the bot's host.

### `/admin status`

Ephemeral. Reports systemd health (unit installed / enabled / active),
whether the upgrade helper is installed, Discord connectivity and
latency, and a version line: the running version vs the latest GitHub
release (`up to date` or `update available: vX.Y.Z`).

### `/admin upgrade [version] [restart]`

Upgrades dispatchd to the latest release (or the `version` you pin, which
allows downgrades). `restart` defaults to true.

The bot itself is unprivileged, so it hands the actual work to a
root systemd helper (installed by `sudo dispatchd service install`). You
see per-step progress in the ephemeral reply:

```
✓ checking for updates
✓ latest is 0.6.0 (current 0.5.0)
✓ downloading dispatchd-aarch64-unknown-linux-musl.tar.gz
✓ checksum verified
✓ binary swapped
↻ Restarting dispatchd now — the new instance will confirm in this channel.
```

Because a successful upgrade restarts the bot, the **freshly-started
instance** posts a public confirmation into the channel you ran the
command in:

```
✅ dispatchd upgraded v0.5.0 → v0.6.0 (requested by @you).
```

If that confirmation never appears after "Restarting dispatchd now",
the upgrade failed — check `journalctl -u dispatchd-upgrade` on the host.

If `/admin upgrade` replies that the helper isn't installed, run
`sudo dispatchd service install` on the host once (also needed on any
deployment that predates this feature).
```

- [ ] **Step 3: `docs/discord-setup.md`**

Wherever roles are listed, change to `admin | lead | designer | senior | medior | junior` and add a sentence: "`admin` is a superset of `lead` — same standup tooling plus `/admin` (see the user guide). Enabling `/admin upgrade` needs one `sudo dispatchd service install` (it installs the `dispatchd-upgrade.path` helper)."

- [ ] **Step 4: `CLAUDE.md`**

- "Running it" block: add
  ```
  dispatchd upgrade    # resolve the latest GitHub release; if newer,
                       # download the matching prebuilt, verify its
                       # SHA-256, swap the binary, restart dispatchd.
                       # --check (report only), --no-restart, --version
                       # <tag> (pin/downgrade). Needs root. Also the
                       # engine behind Discord's /admin upgrade, via the
                       # hidden `upgrade --from-request` helper mode.
  ```
- "Project layout": add
  - `src/upgrade.rs` — self-upgrade: pure helpers (`Version`, `sha256_for`, `verify_sha256`, `parse_latest_tag`, `Request`/`StatusLine` serde) + reqwest/tar I/O (`check`, `download_and_stage`, `install_staged`) + the `dispatchd upgrade` CLI. `--from-request` is the root-helper mode driven by `dispatchd-upgrade.service`; it streams `StatusLine`s to `/run/dispatchd/upgrade.status` and always deletes `/run/dispatchd/upgrade.request` (a `Drop` guard) so the `.path` unit re-arms.
  - under `discord/`: `admin.rs — /admin status|upgrade|help (MANAGE_GUILD-gated + members::is_admin). status renders service::status_report + discord_login::ping_report + upgrade::check. upgrade writes the request file, tails the status file, edits the ephemeral per step; post_upgrade_confirmation (called from mod.rs on ready) posts the cross-restart success message.`
- `members.rs` line: note `is_admin` column + `is_admin()` check; `admin` role seeds `is_admin=1` and `is_lead=1`.
- `service.rs` line: note `dispatchd-upgrade.service`/`.path` written by `service install`, `RuntimeDirectory=dispatchd` on the main unit, and the `status_report()`/`format_status()` split (shared with `/admin status`).
- `discord_login.rs` line: note the `ping_report()`/`format_ping()` split.
- `db/` line: migration `0005_members_is_admin.sql`.

- [ ] **Step 5: Verify + commit**

Run: `cargo test` (full suite), `cargo clippy --all-targets`, `cargo fmt --check`, `cargo build --release` — all clean.

```bash
git add docs/ CLAUDE.md
git commit -m "docs: dispatchd upgrade + /admin + admin role"
```

---

## Manual verification (record results in the PR description)

Not automatable (no live gateway / no root in CI — same constraint as the
existing serenity code):

1. `cargo run -- upgrade --check` prints current vs the real latest release.
2. On a Linux host: `sudo dispatchd service install`, then
   `systemctl list-unit-files | grep dispatchd-upgrade` shows
   `dispatchd-upgrade.path` enabled and `dispatchd-upgrade.service` static.
   `ls -ld /run/dispatchd` shows it owned by the service user.
3. `sudo dispatchd upgrade --version <an older tag>` downgrades and restarts;
   `dispatchd --version` confirms.
4. End-to-end `/admin upgrade` between two real tags: progress streams in
   the ephemeral, the bot restarts, the new instance posts the public
   `✅ dispatchd upgraded …` line. Run `/admin status` before and after.
5. Force a helper failure: `/admin upgrade version:v99.0.0` →
   ephemeral shows `❌ …`, `ls /run/dispatchd/` shows no `upgrade.request`
   left behind, `systemctl status dispatchd-upgrade` is not looping.
6. `/admin status` and `/admin upgrade` invoked by a non-admin member →
   `⛔` reply.

---

## Self-review notes

- **Spec §1 (deps):** Task 2 (`serde_json`, `sha2`), Task 3 (`reqwest`,
  `flate2`, `tar`, tokio `time`). `.cargo/audit.toml` — re-run `cargo
  audit` after Task 3; add documented ignores only if a new unfixable
  advisory appears (noted in Task 3 Step 1 / manual verification).
- **Spec §2 (build.rs):** Task 2 Step 2.
- **Spec §3 (upgrade.rs):** pure = Task 2; I/O + CLI = Task 3; helper
  mode = Task 4.
- **Spec §4 (main.rs):** Task 3 Step 6.
- **Spec §5 (bridge):** Task 5.
- **Spec §6 (structured status):** Task 6.
- **Spec §7 (schema + members):** Task 1.
- **Spec §8 (`/admin`):** status/help = Task 7; upgrade + confirmation =
  Task 8.
- **Spec §9 (mod.rs):** Task 7 Step 3 + Task 8 Step 4.
- **Spec §10 (help.rs):** Task 7 Step 4.
- **Spec §11 (docs):** Task 9.
- **Spec §12 (testing):** each task's test steps; manual list above.
- **Spec "risks":** `.path` retrigger loop → `RequestGuard` (Task 4,
  tested); `reqwest` unification → Task 3 Step 1 fallback; old deployments
  → Task 8 `handle_upgrade` helper-not-installed branch; stale status file
  from a CLI upgrade → `post_upgrade_confirmation` requires a non-empty
  `channel_id` (Task 8).
```

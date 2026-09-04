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
// consumed by Task 8 (admin upgrade tailing)
#[expect(dead_code)]
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
    Found {
        current: String,
        latest: String,
    },
    Downloading {
        asset: String,
    },
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
    Error {
        message: String,
        channel_id: String,
    },
}

impl StatusLine {
    /// `Done` / `Error` end the sequence.
    // consumed by Task 8 (post_upgrade_confirmation / tail loop); exercised
    // by the unit tests, so the guard is non-test builds only
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn is_terminal(&self) -> bool {
        matches!(self, StatusLine::Done { .. } | StatusLine::Error { .. })
    }
}

/// Parses `STATUS_PATH`'s contents, skipping blank and unparseable lines.
// consumed by Task 8 (admin upgrade tailing); exercised by the unit tests,
// so the guard is non-test builds only
#[cfg_attr(not(test), expect(dead_code))]
pub fn parse_status(contents: &str) -> Vec<StatusLine> {
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<StatusLine>(l).ok())
        .collect()
}

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn user_agent() -> String {
    format!("dispatchd/{}", current_version())
}

/// Ensures a leading `v` so release URLs
/// (`/releases/download/v0.6.0/...`) build correctly.
fn normalize_tag(v: &str) -> String {
    if v.starts_with('v') {
        v.to_string()
    } else {
        format!("v{v}")
    }
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

// consumed by Task 7 (/admin status version_line)
#[expect(dead_code)]
pub struct UpgradeCheck {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
}

/// Resolves the latest release tag and compares it to the running version.
// consumed by Task 7 (/admin status)
#[expect(dead_code)]
pub async fn check() -> Result<UpgradeCheck> {
    let latest = fetch_latest_tag().await?;
    let current = current_version().to_string();
    let update_available = is_newer(&latest, &current);
    Ok(UpgradeCheck {
        current,
        latest,
        update_available,
    })
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

    // The two-space separator is `sha256sum` text-mode output, which is what
    // dispatchd's own release pipeline writes - not an arbitrary format.
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
            println!(
                "current: {current}\nlatest:  {target}\nupdate available - run `dispatchd upgrade`"
            );
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
    let dir = exe
        .parent()
        .context("dispatchd's path has no parent directory")?;

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
    if let Ok(json) = serde_json::to_string(line)
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(f, "{json}");
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
            append_status(&StatusLine::Error {
                message: e.to_string(),
                channel_id: chan,
            });
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
    append_status(&StatusLine::Found {
        current: current.clone(),
        latest: target.clone(),
    });

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
    let dir = exe
        .parent()
        .context("dispatchd's path has no parent directory")?;

    append_status(&StatusLine::Downloading {
        asset: asset_name(),
    });
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
        assert_eq!(
            sha256_for(sums, "dispatchd-armv7-unknown-linux-musleabihf.tar.gz"),
            None
        );
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
            StatusLine::Found {
                current: "0.5.0".into(),
                latest: "0.6.0".into(),
            },
            StatusLine::Downloading {
                asset: "dispatchd-x.tar.gz".into(),
            },
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

    #[test]
    fn done_and_error_are_terminal() {
        assert!(
            StatusLine::Done {
                from: "0.5.0".into(),
                to: "0.6.0".into(),
                channel_id: "1".into(),
                requested_by: "2".into(),
                requested_by_name: "Ops".into(),
                noop: false,
            }
            .is_terminal()
        );
        assert!(
            StatusLine::Error {
                message: "x".into(),
                channel_id: "1".into()
            }
            .is_terminal()
        );
        assert!(!StatusLine::Verified.is_terminal());
    }
}

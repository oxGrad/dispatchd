//! Self-upgrade: resolve the latest GitHub release, download the matching
//! prebuilt binary, verify its SHA-256, swap it in, restart the service.
//! Driven by `dispatchd upgrade` (CLI) and, via `--from-request`, by the
//! root `dispatchd-upgrade.service` oneshot that `/admin upgrade` triggers.
//!
//! The pure helpers here (version math, checksum parsing, request/status
//! serde) are unit-tested. The reqwest/tar I/O is not, same as the
//! `src/discord/*` gateway code - see CLAUDE.md's testing notes.

// The reqwest/tar I/O and the `dispatchd upgrade` CLI that consume these
// helpers land in a follow-up change; until then the pure helpers here are
// only exercised by the unit tests below.
#![allow(dead_code)]

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

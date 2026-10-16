use std::path::Path;

use anyhow::{Context, Result};

use crate::config::GithubConfig;
use crate::github::auth::AppKey;
use crate::github::client::{self, AppCheck};
use crate::service::{GITHUB_CRED_PATH, UnitSync};

/// `dispatchd github login --key <file.pem>`: validates the GitHub App
/// private key end-to-end against the `[github]` App and org in
/// config.toml (`client::check`), then encrypts it at rest via
/// `systemd-creds` - the same `--with-key=host` scheme as `discord login` -
/// and patches the installed unit to load it. The `.pem` itself is never
/// copied anywhere in plaintext.
///
/// Linux only, like `discord login`; see the stub below.
#[cfg(target_os = "linux")]
pub async fn run(key_path: &Path) -> Result<()> {
    let version = crate::service::systemd_version()?;
    if version < crate::service::MIN_SYSTEMD_VERSION {
        anyhow::bail!(
            "dispatchd github login requires systemd >= {} (systemd-creds, for encrypting \
             the key at rest) but found systemd {version} - there is no plaintext fallback.",
            crate::service::MIN_SYSTEMD_VERSION
        );
    }

    let (config, config_path) = crate::config::Config::load_for_sudo()?;
    let github = config.github.with_context(|| {
        let looked_in = config_path
            .map(|p| format!(" (read {})", p.display()))
            .unwrap_or_else(|| " (no config.toml found)".to_string());
        format!(
            "no GitHub App configured{looked_in} - set app_id and org under [github] in \
             config.toml first (see docs/github-setup.md), or point DISPATCHD_CONFIG_PATH \
             at it: sudo DISPATCHD_CONFIG_PATH=/path/to/config.toml dispatchd github login ..."
        )
    })?;

    let pem = std::fs::read_to_string(key_path)
        .with_context(|| format!("failed to read {}", key_path.display()))?;
    let key = AppKey::from_pem(&pem)
        .with_context(|| format!("{} is not a usable App private key", key_path.display()))?;

    let check = client::check(github.app_id, &github.org, &key)
        .await
        .context("failed to validate the key with GitHub")?;
    println!("{}", describe_check(&check, &github.org));

    std::fs::create_dir_all(crate::service::ENV_DIR).with_context(|| {
        format!(
            "failed to create {} (are you root? try: sudo dispatchd github login)",
            crate::service::ENV_DIR
        )
    })?;
    crate::discord_login::encrypt_secret(crate::github::CRED_NAME, &pem, GITHUB_CRED_PATH)?;
    drop(pem);
    println!("Key encrypted and saved to {GITHUB_CRED_PATH}.");

    match crate::service::sync_github_credential(true)? {
        UnitSync::Updated => println!("Updated dispatchd.service to load it."),
        UnitSync::Unchanged => {}
        UnitSync::NotInstalled => println!(
            "dispatchd.service isn't installed yet - `sudo dispatchd service install` will \
             pick the key up."
        ),
    }
    println!(
        "Run `sudo systemctl restart dispatchd` to pick it up. You can now delete {} - \
         dispatchd only uses the encrypted copy.",
        key_path.display()
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub async fn run(_key_path: &Path) -> Result<()> {
    anyhow::bail!("`dispatchd github login` is only supported on Linux (systemd-creds)")
}

/// One line summarising a successful `client::check`.
fn describe_check(check: &AppCheck, org: &str) -> String {
    format!(
        "GitHub App {:?} ({}) is installed on {org} ({} repositories), {} repo{} visible.",
        check.app.name,
        check.app.id,
        check.installation.repository_selection,
        check.repo_count,
        if check.repo_count == 1 { "" } else { "s" },
    )
}

/// `dispatchd github logout`: drops the credential line from the installed
/// unit first (so it's never left pointing at a missing file, which would
/// stop it starting), then removes the encrypted key. Idempotent, like
/// `discord logout`.
pub fn logout() -> Result<()> {
    if crate::service::sync_github_credential(false)? == UnitSync::Updated {
        println!("Removed the GitHub App key from dispatchd.service.");
    }
    logout_at(GITHUB_CRED_PATH)
}

fn logout_at(cred_path: &str) -> Result<()> {
    if !Path::new(cred_path).exists() {
        println!("no encrypted GitHub App key found at {cred_path} - nothing to do.");
        return Ok(());
    }
    std::fs::remove_file(cred_path).with_context(|| {
        format!("failed to remove {cred_path} (are you root? try: sudo dispatchd github logout)")
    })?;
    println!("removed {cred_path}.");
    println!(
        "dispatchd.service (if running) keeps the old key in memory until restarted - run \
         `sudo systemctl restart dispatchd` to drop it now."
    );
    Ok(())
}

/// `dispatchd github preview --days N`: the sync's queries, run once
/// against the real org, printed instead of stored (`sync::preview`). Reads
/// the key the same way `status` does, so it works under `sudo` with the
/// encrypted credential or unprivileged with `DISPATCHD_GITHUB_APP_KEY_PATH`.
pub async fn preview(days: u32) -> Result<()> {
    let (config, _) = crate::config::Config::load_for_sudo()?;
    let github = config.github.context(
        "no GitHub App configured - set app_id and org under [github] in config.toml \
         (see docs/github-setup.md)",
    )?;
    let pem = crate::github::app_key_pem()
        .or_else(|| crate::discord_login::decrypt_cred_file(GITHUB_CRED_PATH))
        .context(
            "no GitHub App key found - run `sudo dispatchd github login --key <file.pem>` \
             first (or set DISPATCHD_GITHUB_APP_KEY_PATH)",
        )?;
    let key = AppKey::from_pem(&pem).context("the stored GitHub App key is unusable")?;
    print!(
        "{}",
        crate::github::sync::preview(&github, key, days).await?
    );
    Ok(())
}

pub struct GithubPing {
    /// `None` = no `[github]` section; nothing else is attempted.
    pub app: Option<(u64, String)>,
    pub key_found: bool,
    pub result: Option<Result<AppCheck, String>>,
}

/// `dispatchd status`'s GitHub section, as structured data (rendered by
/// `format_github_ping`, reused by `/admin status`). Resolves the key the
/// way the bot does (`github::app_key_pem`), or by decrypting the
/// credential file directly for a one-off root invocation, then runs the
/// same round-trip as `github login`.
pub async fn ping_report(github: Option<&GithubConfig>) -> GithubPing {
    let Some(github) = github else {
        return GithubPing {
            app: None,
            key_found: false,
            result: None,
        };
    };
    let app = Some((github.app_id, github.org.clone()));
    let Some(pem) = crate::github::app_key_pem()
        .or_else(|| crate::discord_login::decrypt_cred_file(GITHUB_CRED_PATH))
    else {
        return GithubPing {
            app,
            key_found: false,
            result: None,
        };
    };
    let result = match AppKey::from_pem(&pem) {
        Ok(key) => client::check(github.app_id, &github.org, &key)
            .await
            .map_err(|e| format!("{e:#}")),
        Err(e) => Err(format!("the stored key is unusable: {e:#}")),
    };
    GithubPing {
        app,
        key_found: true,
        result: Some(result),
    }
}

pub fn format_github_ping(p: &GithubPing) -> String {
    let mut out = String::from("github:\n");
    let Some((app_id, org)) = &p.app else {
        out.push_str(
            "  app:                  not configured (optional - see docs/github-setup.md)\n",
        );
        return out;
    };
    out.push_str(&format!("  app:                  {app_id} on {org}\n"));
    if !p.key_found {
        out.push_str(
            "  key:                  not found - run: sudo dispatchd github login --key <app-key.pem>\n",
        );
        return out;
    }
    match &p.result {
        Some(Ok(check)) => out.push_str(&format!(
            "  api:                  ok - {:?} installed ({} repositories), {} repo{} visible, {}ms\n",
            check.app.name,
            check.installation.repository_selection,
            check.repo_count,
            if check.repo_count == 1 { "" } else { "s" },
            check.latency_ms,
        )),
        Some(Err(e)) => out.push_str(&format!("  api:                  failed - {e}\n")),
        None => out.push_str("  api:                  not attempted\n"),
    }
    out
}

/// `/admin status`'s sync line(s), appended under the `github:` section:
/// when it last ran, what's stored, and the last error if the most recent
/// run failed. Only the bot itself can show this - it reads the DB.
pub fn format_sync_summary(s: &crate::github::store::SyncSummary) -> String {
    let mut out = match &s.last_sync_at {
        Some(at) => format!(
            "  sync:                 last ok {at} - {} PRs, {} issues, {} reviews stored\n",
            s.prs, s.issues, s.reviews
        ),
        None => "  sync:                 no successful sync yet\n".to_string(),
    };
    if let Some(error) = &s.last_error {
        // Already capped when stored (`store::record_sync_error`); capped
        // again here in case an older build stored a longer one.
        out.push_str(&format!(
            "  sync error:           {}\n",
            crate::github::store::truncate(error, crate::github::store::MAX_SYNC_ERROR_CHARS)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::client::{AppInfo, Installation};
    use crate::github::store::SyncSummary;

    #[test]
    fn format_sync_summary_reports_last_run_and_any_error() {
        let ok = SyncSummary {
            last_sync_at: Some("2026-10-16T02:00:00Z".into()),
            last_error: None,
            prs: 42,
            issues: 30,
            reviews: 77,
        };
        assert_eq!(
            format_sync_summary(&ok),
            "  sync:                 last ok 2026-10-16T02:00:00Z - 42 PRs, 30 issues, 77 reviews stored\n"
        );
        let failing = SyncSummary {
            last_sync_at: None,
            last_error: Some("2026-10-16T02:10:00Z: PR search failed".into()),
            prs: 0,
            issues: 0,
            reviews: 0,
        };
        let out = format_sync_summary(&failing);
        assert!(out.contains("no successful sync yet"), "{out}");
        assert!(
            out.contains("sync error:           2026-10-16T02:10:00Z: PR search failed"),
            "{out}"
        );
    }

    fn sample_check() -> AppCheck {
        AppCheck {
            app: AppInfo {
                id: 123456,
                slug: "dispatchd-oxgrad".into(),
                name: "dispatchd".into(),
            },
            installation: Installation {
                id: 987,
                repository_selection: "all".into(),
            },
            repo_count: 14,
            latency_ms: 182,
        }
    }

    #[test]
    fn describe_check_summarises_the_installation() {
        assert_eq!(
            describe_check(&sample_check(), "oxgrad"),
            "GitHub App \"dispatchd\" (123456) is installed on oxgrad (all repositories), 14 repos visible."
        );
    }

    #[test]
    fn format_github_ping_not_configured_stops_early() {
        let out = format_github_ping(&GithubPing {
            app: None,
            key_found: false,
            result: None,
        });
        assert_eq!(
            out,
            "github:\n  app:                  not configured (optional - see docs/github-setup.md)\n"
        );
    }

    #[test]
    fn format_github_ping_without_a_key_says_how_to_add_one() {
        let out = format_github_ping(&GithubPing {
            app: Some((123456, "oxgrad".into())),
            key_found: false,
            result: None,
        });
        assert!(
            out.contains("app:                  123456 on oxgrad"),
            "{out}"
        );
        assert!(out.contains("sudo dispatchd github login --key"), "{out}");
    }

    #[test]
    fn format_github_ping_ok_and_failed() {
        let ok = format_github_ping(&GithubPing {
            app: Some((123456, "oxgrad".into())),
            key_found: true,
            result: Some(Ok(sample_check())),
        });
        assert!(
            ok.contains("ok - \"dispatchd\" installed (all repositories), 14 repos visible, 182ms"),
            "{ok}"
        );

        let failed = format_github_ping(&GithubPing {
            app: Some((123456, "oxgrad".into())),
            key_found: true,
            result: Some(Err("GitHub rejected the App JWT (401)".into())),
        });
        assert!(
            failed.contains("failed - GitHub rejected the App JWT (401)"),
            "{failed}"
        );
    }

    #[test]
    fn logout_at_removes_an_existing_key_and_tolerates_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github_app_key.cred");
        std::fs::write(&path, "encrypted").unwrap();

        logout_at(path.to_str().unwrap()).unwrap();
        assert!(!path.exists());
        logout_at(path.to_str().unwrap()).unwrap();
    }
}

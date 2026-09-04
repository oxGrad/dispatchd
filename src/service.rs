// This module is systemd glue plus a handful of pure, platform-agnostic
// renderers/parsers that stay unit-tested everywhere. On non-Linux only the
// `#[cfg(not(target_os = "linux"))]` stubs of `install`/`status` are live, so
// the paths, renderers, and version parser are all legitimately unused there.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

const UNIT_PATH: &str = "/etc/systemd/system/dispatchd.service";
const MAINTENANCE_SERVICE_PATH: &str = "/etc/systemd/system/dispatchd-maintenance.service";
const MAINTENANCE_TIMER_PATH: &str = "/etc/systemd/system/dispatchd-maintenance.timer";
pub(crate) const ENV_DIR: &str = "/etc/dispatchd";
const INSTALL_LOCK_PATH: &str = "/etc/dispatchd/service-install.lock";
/// Where the systemd-creds-encrypted Discord token lives. Written by
/// `dispatchd discord login` (see `discord_login.rs`), and referenced from
/// the unit via `LoadCredentialEncrypted=`, which decrypts it into
/// `$CREDENTIALS_DIRECTORY/discord_token` (tmpfs, root-only) for the
/// duration of the service's run - dispatchd itself never sees the
/// plaintext token touch disk. See `main.rs::discord_token`.
pub(crate) const CRED_PATH: &str = "/etc/dispatchd/discord_token.cred";
/// `LoadCredentialEncrypted=` (and `systemd-creds` itself) landed in this
/// release - there's no plaintext fallback for older systemd, so both
/// `install()` and `discord_login::run()` refuse to proceed rather than
/// silently handling the token unencrypted.
pub(crate) const MIN_SYSTEMD_VERSION: u32 = 250;

const UPGRADE_SERVICE_PATH: &str = "/etc/systemd/system/dispatchd-upgrade.service";
pub(crate) const UPGRADE_PATH_PATH: &str = "/etc/systemd/system/dispatchd-upgrade.path";

/// Pure string rendering, no I/O - compiles and is unit-tested on any
/// platform even though what it produces is Linux/systemd-specific.
///
/// `RuntimeDirectory=dispatchd` gives the unprivileged bot a writable
/// `/run/dispatchd` for the `/admin upgrade` request/status files;
/// `RuntimeDirectoryPreserve=yes` keeps it across the upgrade restart so
/// the status file survives for the bot to read back afterward.
fn render_unit(exe_path: &str, user: &str) -> String {
    format!(
        "[Unit]\n\
         Description=dispatchd - Discord standup bot\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         User={user}\n\
         ExecStart={exe_path}\n\
         LoadCredentialEncrypted=discord_token:{CRED_PATH}\n\
         RuntimeDirectory=dispatchd\n\
         RuntimeDirectoryPreserve=yes\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// The oneshot service the maintenance timer below actually triggers -
/// the handover doc's weekly `DELETE ... VACUUM` cron.
fn render_maintenance_service(exe_path: &str, user: &str) -> String {
    format!(
        "[Unit]\n\
         Description=dispatchd weekly database maintenance\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         User={user}\n\
         ExecStart={exe_path} maintenance run\n"
    )
}

/// `Persistent=true` means a missed run (e.g. the Pi was powered off)
/// fires as soon as it's back up, instead of waiting for the next
/// scheduled week - matching the doc's "weekly cron" intent even across
/// downtime, the same reconciliation philosophy used throughout this
/// project.
fn render_maintenance_timer() -> &'static str {
    "[Unit]\n\
     Description=Run dispatchd weekly database maintenance\n\
     \n\
     [Timer]\n\
     OnCalendar=weekly\n\
     Persistent=true\n\
     \n\
     [Install]\n\
     WantedBy=timers.target\n"
}

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

/// Parses the version number out of `systemctl --version`'s first line,
/// e.g. `"systemd 252 (252.22-1~deb12u1)\n+PAM +AUDIT ..."` -> `252`. Split
/// out from `systemd_version` so the parsing logic is unit-tested without
/// actually shelling out.
fn parse_systemd_version(version_output: &str) -> anyhow::Result<u32> {
    use anyhow::Context;

    let first_line = version_output.lines().next().unwrap_or("");
    first_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u32>().ok())
        .with_context(|| format!("could not parse systemd version from: {first_line:?}"))
}

#[cfg(target_os = "linux")]
pub(crate) fn systemd_version() -> anyhow::Result<u32> {
    use anyhow::Context;

    let output = std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .context("failed to run `systemctl --version` - is systemd installed?")?;
    parse_systemd_version(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "linux")]
pub fn install() -> anyhow::Result<()> {
    use anyhow::Context;

    let exe =
        std::env::current_exe().context("failed to resolve dispatchd's own executable path")?;
    let exe = exe
        .to_str()
        .context("dispatchd's executable path is not valid UTF-8")?;

    let user = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .context("could not determine which user to run the service as (set $USER)")?;

    let version = systemd_version()?;
    if version < MIN_SYSTEMD_VERSION {
        anyhow::bail!(
            "dispatchd service install requires systemd >= {MIN_SYSTEMD_VERSION} \
             (LoadCredentialEncrypted=/systemd-creds, for encrypting the Discord token \
             at rest) but found systemd {version} - there is no plaintext fallback. \
             Upgrade the OS (e.g. Raspberry Pi OS Bookworm or newer) and retry."
        );
    }

    std::fs::create_dir_all(ENV_DIR).with_context(|| {
        format!("failed to create {ENV_DIR} (are you root? try: sudo dispatchd service install)")
    })?;

    // Guards against two concurrent `service install` runs interleaving
    // their writes to the same unit files.
    let _singleton = crate::lock::acquire(std::path::Path::new(INSTALL_LOCK_PATH))?;

    if std::path::Path::new(CRED_PATH).exists() {
        println!("{CRED_PATH} already exists, leaving it untouched");
    } else {
        println!("no encrypted Discord token found at {CRED_PATH} yet - run:");
        println!("  sudo dispatchd discord login");
        println!("once dispatchd.service is installed, to set it up.");
    }

    let unit = render_unit(exe, &user);
    std::fs::write(UNIT_PATH, unit).with_context(|| {
        format!("failed to write {UNIT_PATH} (are you root? try: sudo dispatchd service install)")
    })?;
    println!("wrote {UNIT_PATH}");

    let maintenance_service = render_maintenance_service(exe, &user);
    std::fs::write(MAINTENANCE_SERVICE_PATH, maintenance_service)
        .with_context(|| format!("failed to write {MAINTENANCE_SERVICE_PATH}"))?;
    std::fs::write(MAINTENANCE_TIMER_PATH, render_maintenance_timer())
        .with_context(|| format!("failed to write {MAINTENANCE_TIMER_PATH}"))?;
    println!("wrote {MAINTENANCE_SERVICE_PATH} and {MAINTENANCE_TIMER_PATH}");

    std::fs::write(UPGRADE_SERVICE_PATH, render_upgrade_service(exe))
        .with_context(|| format!("failed to write {UPGRADE_SERVICE_PATH}"))?;
    std::fs::write(UPGRADE_PATH_PATH, render_upgrade_path())
        .with_context(|| format!("failed to write {UPGRADE_PATH_PATH}"))?;
    println!("wrote {UPGRADE_SERVICE_PATH} and {UPGRADE_PATH_PATH}");

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "dispatchd.service"])?;
    // The maintenance timer only prunes old reminders_sent/followups_sent
    // rows and VACUUMs - safe to start immediately, unlike the main
    // service, which shouldn't run before Discord is actually configured.
    run_systemctl(&["enable", "--now", "dispatchd-maintenance.timer"])?;
    // Path-activated: sits idle until `/admin upgrade` writes a request.
    run_systemctl(&["enable", "--now", "dispatchd-upgrade.path"])?;

    println!("dispatchd.service installed and enabled to start at boot.");
    println!("dispatchd-maintenance.timer installed and started (runs weekly).");
    println!("dispatchd-upgrade.path installed and started (enables /admin upgrade).");
    println!(
        "Once {CRED_PATH} (see `dispatchd discord login`) and config.toml/members.toml (see \
         `dispatchd init`) are set up, start the bot with:"
    );
    println!("  sudo systemctl start dispatchd");
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> anyhow::Result<()> {
    use anyhow::Context;

    let status = std::process::Command::new("systemctl")
        .args(args)
        .status()
        .context("failed to run systemctl - is it installed?")?;
    if !status.success() {
        anyhow::bail!("systemctl {} failed", args.join(" "));
    }
    Ok(())
}

/// `systemctl is-enabled`/`is-active` for `dispatchd.service` - a non-zero
/// exit status is a normal, expected outcome here (e.g. "inactive" or
/// "disabled"), not a failure, so this reports the state as text rather
/// than propagating an error the way `run_systemctl` does for
/// fire-and-forget mutating commands.
#[cfg(target_os = "linux")]
fn systemctl_query(args: &[&str]) -> String {
    match std::process::Command::new("systemctl").args(args).output() {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            } else {
                trimmed.to_string()
            }
        }
        Err(e) => format!("unknown ({e})"),
    }
}

/// `dispatchd status`'s systemd half, as structured data: whether this host
/// can even support token encryption, and whether the service is
/// installed/enabled/active. Gathered here, rendered by `format_status` -
/// so `/admin status` (which can't `println!`) can reuse the same data.
pub struct ServiceStatus {
    /// `false` on non-Linux, where none of the systemd checks can run.
    pub supported: bool,
    pub systemd_version: Option<u32>,
    pub min_systemd_version: u32,
    pub unit_installed: bool,
    pub unit_enabled: Option<String>,
    pub unit_active: Option<String>,
    pub upgrade_helper_installed: bool,
    pub cred_present: bool,
}

#[cfg(target_os = "linux")]
pub fn status_report() -> ServiceStatus {
    let unit_installed = std::path::Path::new(UNIT_PATH).exists();
    ServiceStatus {
        supported: true,
        systemd_version: systemd_version().ok(),
        min_systemd_version: MIN_SYSTEMD_VERSION,
        unit_installed,
        unit_enabled: unit_installed.then(|| systemctl_query(&["is-enabled", "dispatchd.service"])),
        unit_active: unit_installed.then(|| systemctl_query(&["is-active", "dispatchd.service"])),
        upgrade_helper_installed: std::path::Path::new(UPGRADE_PATH_PATH).exists(),
        cred_present: std::path::Path::new(CRED_PATH).exists(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn status_report() -> ServiceStatus {
    ServiceStatus {
        supported: false,
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
    if !r.supported {
        return String::from("systemd:\n  systemd status checks are only supported on Linux\n");
    }
    let mut out = String::from("systemd:\n");
    match r.systemd_version {
        Some(v) if v >= r.min_systemd_version => out.push_str(&format!(
            "  version:              {v} (>= {}, ok)\n",
            r.min_systemd_version
        )),
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
        out.push_str(
            "  dispatchd.service:    not installed - run: sudo dispatchd service install\n",
        );
    }
    if r.upgrade_helper_installed {
        out.push_str("  upgrade helper:       installed\n");
    } else {
        out.push_str(
            "  upgrade helper:       not installed - run: sudo dispatchd service install\n",
        );
    }
    if r.cred_present {
        out.push_str(&format!(
            "  discord token:        encrypted credential present ({CRED_PATH})\n"
        ));
    } else {
        out.push_str("  discord token:        not set - run: sudo dispatchd discord login\n");
    }
    out
}

/// `dispatchd status`'s systemd half: whether this host can even support
/// token encryption, and whether the service is installed/enabled/active.
pub fn status() -> anyhow::Result<()> {
    print!("{}", format_status(&status_report()));
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn install() -> anyhow::Result<()> {
    anyhow::bail!("`dispatchd service install` is only supported on Linux (systemd)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_unit_interpolates_exe_path_and_user() {
        let unit = render_unit("/usr/local/bin/dispatchd", "pi");
        assert!(unit.contains("ExecStart=/usr/local/bin/dispatchd"));
        assert!(unit.contains("User=pi"));
        assert!(
            unit.contains(
                "LoadCredentialEncrypted=discord_token:/etc/dispatchd/discord_token.cred"
            )
        );
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn parse_systemd_version_reads_the_number_from_the_first_line() {
        // Debian Bookworm's actual `systemctl --version` output shape.
        assert_eq!(
            parse_systemd_version("systemd 252 (252.22-1~deb12u1)\n+PAM +AUDIT +SELINUX +APPARMOR")
                .unwrap(),
            252
        );
    }

    #[test]
    fn parse_systemd_version_rejects_unparseable_output() {
        assert!(parse_systemd_version("not systemd output at all").is_err());
        assert!(parse_systemd_version("").is_err());
    }

    #[test]
    fn min_systemd_version_matches_the_confirmed_requirement() {
        // LoadCredentialEncrypted=/systemd-creds landed in systemd 250 -
        // regression guard against silently loosening this without also
        // updating the bail-out messages and docs.
        assert_eq!(MIN_SYSTEMD_VERSION, 250);
    }

    #[test]
    fn render_maintenance_service_interpolates_exe_path_and_user() {
        let unit = render_maintenance_service("/usr/local/bin/dispatchd", "pi");
        assert!(unit.contains("ExecStart=/usr/local/bin/dispatchd maintenance run"));
        assert!(unit.contains("User=pi"));
        assert!(unit.contains("Type=oneshot"));
    }

    #[test]
    fn render_maintenance_timer_runs_weekly_and_catches_up() {
        let timer = render_maintenance_timer();
        assert!(timer.contains("OnCalendar=weekly"));
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains("WantedBy=timers.target"));
    }

    #[test]
    fn render_unit_declares_a_preserved_runtime_directory() {
        let unit = render_unit("/usr/local/bin/dispatchd", "pi");
        assert!(unit.contains("RuntimeDirectory=dispatchd"));
        assert!(unit.contains("RuntimeDirectoryPreserve=yes"));
    }

    #[test]
    fn render_upgrade_service_is_a_root_oneshot_helper() {
        let unit = render_upgrade_service("/usr/local/bin/dispatchd");
        assert!(unit.contains("ExecStart=/usr/local/bin/dispatchd upgrade --from-request"));
        assert!(unit.contains("Type=oneshot"));
        assert!(!unit.contains("User="), "helper runs as root");
        assert!(
            !unit.contains("[Install]"),
            "only ever started by the .path unit"
        );
    }

    #[test]
    fn render_upgrade_path_watches_the_request_file() {
        let path = render_upgrade_path();
        assert!(path.contains("PathExists=/run/dispatchd/upgrade.request"));
        assert!(path.contains("Unit=dispatchd-upgrade.service"));
        assert!(path.contains("WantedBy=paths.target"));
    }

    fn base_status() -> ServiceStatus {
        ServiceStatus {
            supported: true,
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

    #[test]
    fn format_status_on_an_unsupported_platform_says_so() {
        let s = ServiceStatus {
            supported: false,
            ..base_status()
        };
        let out = format_status(&s);
        assert_eq!(
            out,
            "systemd:\n  systemd status checks are only supported on Linux\n"
        );
    }
}

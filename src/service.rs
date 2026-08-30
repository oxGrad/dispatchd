const UNIT_PATH: &str = "/etc/systemd/system/dispatchd.service";
const MAINTENANCE_SERVICE_PATH: &str = "/etc/systemd/system/dispatchd-maintenance.service";
const MAINTENANCE_TIMER_PATH: &str = "/etc/systemd/system/dispatchd-maintenance.timer";
const ENV_DIR: &str = "/etc/dispatchd";
const INSTALL_LOCK_PATH: &str = "/etc/dispatchd/service-install.lock";
/// Where the systemd-creds-encrypted Discord token lives. Referenced from
/// the unit via `LoadCredentialEncrypted=`, which decrypts it into
/// `$CREDENTIALS_DIRECTORY/discord_token` (tmpfs, root-only) for the
/// duration of the service's run - dispatchd itself never sees the
/// plaintext token touch disk. See `main.rs::discord_token`.
const CRED_PATH: &str = "/etc/dispatchd/discord_token.cred";
/// `LoadCredentialEncrypted=` (and `systemd-creds` itself) landed in this
/// release - there's no plaintext fallback for older systemd, so
/// `install()` refuses to proceed rather than silently writing the token
/// unencrypted.
const MIN_SYSTEMD_VERSION: u32 = 250;

/// Pure string rendering, no I/O - compiles and is unit-tested on any
/// platform even though what it produces is Linux/systemd-specific.
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
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
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
fn systemd_version() -> anyhow::Result<u32> {
    use anyhow::Context;

    let output = std::process::Command::new("systemctl")
        .arg("--version")
        .output()
        .context("failed to run `systemctl --version` - is systemd installed?")?;
    parse_systemd_version(&String::from_utf8_lossy(&output.stdout))
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
        println!("no encrypted Discord token found at {CRED_PATH} - dispatchd never handles the");
        println!("raw token itself, so encrypt it yourself, then (re)start the service:");
        println!();
        println!("  sudo systemd-creds encrypt --name=discord_token --with-key=host - {CRED_PATH}");
        println!();
        println!("  (paste the bot token from docs/discord-setup.md, then press Ctrl-D;");
        println!("   --with-key=host is used because Raspberry Pi boards have no TPM2 - this");
        println!("   protects an offline copy of the SD card, not root on the live Pi.)");
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

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "dispatchd.service"])?;
    // The maintenance timer only prunes old reminders_sent/followups_sent
    // rows and VACUUMs - safe to start immediately, unlike the main
    // service, which shouldn't run before Discord is actually configured.
    run_systemctl(&["enable", "--now", "dispatchd-maintenance.timer"])?;

    println!("dispatchd.service installed and enabled to start at boot.");
    println!("dispatchd-maintenance.timer installed and started (runs weekly).");
    println!(
        "Once {CRED_PATH} and config.toml/members.toml (see `dispatchd init`) are set up, start the bot with:"
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
    fn min_systemd_version_matches_the_todo_confirmed_requirement() {
        // LoadCredentialEncrypted=/systemd-creds landed in systemd 250 -
        // regression guard against silently loosening this without also
        // updating the bail-out message and docs.
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
}

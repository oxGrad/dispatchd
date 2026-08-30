const UNIT_PATH: &str = "/etc/systemd/system/dispatchd.service";
const MAINTENANCE_SERVICE_PATH: &str = "/etc/systemd/system/dispatchd-maintenance.service";
const MAINTENANCE_TIMER_PATH: &str = "/etc/systemd/system/dispatchd-maintenance.timer";
const ENV_DIR: &str = "/etc/dispatchd";
const ENV_FILE: &str = "/etc/dispatchd/dispatchd.env";
const ENV_TEMPLATE: &str = "# dispatchd secrets - fill in and (re)start the service.\n\
# mode 600, root-owned; never commit this file.\n\
DISPATCHD_DISCORD_TOKEN=\n";

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
         EnvironmentFile=-{ENV_FILE}\n\
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

    std::fs::create_dir_all(ENV_DIR).with_context(|| {
        format!("failed to create {ENV_DIR} (are you root? try: sudo dispatchd service install)")
    })?;

    if std::path::Path::new(ENV_FILE).exists() {
        println!("{ENV_FILE} already exists, leaving it untouched");
    } else {
        std::fs::write(ENV_FILE, ENV_TEMPLATE)
            .with_context(|| format!("failed to write {ENV_FILE}"))?;
        let mut perms = std::fs::metadata(ENV_FILE)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o600);
        std::fs::set_permissions(ENV_FILE, perms)
            .with_context(|| format!("failed to set permissions on {ENV_FILE}"))?;
        println!(
            "created {ENV_FILE} (mode 600) - fill in DISPATCHD_DISCORD_TOKEN before starting the service"
        );
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
        "Once {ENV_FILE} and config.toml/members.toml (see `dispatchd init`) are set up, start the bot with:"
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
        assert!(unit.contains("EnvironmentFile=-/etc/dispatchd/dispatchd.env"));
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(unit.contains("Restart=on-failure"));
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

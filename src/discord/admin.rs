use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serenity::all::{
    ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    Context as SerenityContext, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditInteractionResponse, Permissions,
};

use crate::discord_login::{self, DiscordPing};
use crate::service::{self, ServiceStatus};
use crate::upgrade::{Request, StatusLine};
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

/// Discord nests a subcommand's own options one level under an entry named
/// after the subcommand - this pulls out `(subcommand_name, its_options)`.
/// (Same shape as `team::subcommand`.)
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
            "  version:              {} - update available: {}",
            c.current,
            c.latest.trim_start_matches('v')
        ),
        Ok(c) => format!("  version:              {} (up to date)", c.current),
        Err(e) => format!(
            "  version:              {} - latest unknown ({e})",
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

async fn ephemeral(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    content: impl Into<String>,
) {
    let reply = CreateInteractionResponseMessage::new()
        .content(content)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /admin: {e}");
    }
}

pub async fn handle_help(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
) {
    let discord_user_id = command.user.id.to_string();
    let allowed = {
        let conn = db.lock().expect("db mutex poisoned");
        members::is_admin(&conn, &discord_user_id)
    };
    match allowed {
        Ok(true) => {}
        Ok(false) => return ephemeral(ctx, command, permission_denied_reply()).await,
        Err(e) => {
            eprintln!("failed to check is_admin: {e}");
            return ephemeral(
                ctx,
                command,
                "⚠️ Something went wrong checking permissions.",
            )
            .await;
        }
    }
    ephemeral(ctx, command, ADMIN_HELP_TEXT).await;
}

pub async fn handle_status(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
) {
    let discord_user_id = command.user.id.to_string();
    let allowed = {
        let conn = db.lock().expect("db mutex poisoned");
        members::is_admin(&conn, &discord_user_id)
    };
    match allowed {
        Ok(true) => {}
        Ok(false) => return ephemeral(ctx, command, permission_denied_reply()).await,
        Err(e) => {
            eprintln!("failed to check is_admin: {e}");
            return ephemeral(
                ctx,
                command,
                "⚠️ Something went wrong checking permissions.",
            )
            .await;
        }
    }

    // Defer before the network round-trips (systemd probe + Discord ping +
    // GitHub release check) - together they can easily exceed Discord's
    // 3-second initial-response window.
    let defer =
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true));
    if let Err(e) = command.create_response(&ctx.http, defer).await {
        eprintln!("failed to defer /admin status: {e}");
        return;
    }

    let svc = service::status_report();
    let ping = discord_login::ping_report().await;
    let check = upgrade::check().await.map_err(|e| e.to_string());
    let body = format_admin_status(&svc, &ping, &version_line(check));
    edit(ctx, command, body).await;
}

/// Renders the tail of the helper's progress into the ephemeral reply.
fn render_steps(lines: &[StatusLine]) -> String {
    let mut out = String::from("**Upgrade progress**\n");
    for line in lines {
        match line {
            StatusLine::Checking => out.push_str("✓ checking for updates\n"),
            StatusLine::Found { current, latest } => out.push_str(&format!(
                "✓ latest is {} (current {current})\n",
                latest.trim_start_matches('v')
            )),
            StatusLine::Downloading { asset } => out.push_str(&format!("✓ downloading {asset}\n")),
            StatusLine::Verified => out.push_str("✓ checksum verified\n"),
            StatusLine::Swapped => out.push_str("✓ binary swapped\n"),
            StatusLine::Restarting => out.push_str(
                "↻ Restarting dispatchd now — the new instance will confirm in this channel.\n",
            ),
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
    let allowed = {
        let conn = db.lock().expect("db mutex poisoned");
        members::is_admin(&conn, &discord_user_id)
    };
    match allowed {
        Ok(true) => {}
        Ok(false) => return ephemeral(ctx, command, permission_denied_reply()).await,
        Err(e) => {
            eprintln!("failed to check is_admin: {e}");
            return ephemeral(
                ctx,
                command,
                "⚠️ Something went wrong checking permissions.",
            )
            .await;
        }
    }

    if !Path::new(upgrade::RUN_DIR).exists() {
        return ephemeral(
            ctx,
            command,
            "⚠️ The upgrade helper isn't installed. Run `sudo dispatchd service install` and then `sudo systemctl restart dispatchd` on the host, then retry.",
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

    // Clear any status file left by a previous run so the poll loop below
    // can't latch onto its stale terminal line.
    let _ = std::fs::remove_file(upgrade::STATUS_PATH);

    let request = Request {
        requested_by: discord_user_id,
        requested_by_name: command.user.name.clone(),
        channel_id: command.channel_id.to_string(),
        target_version: version,
        restart,
        requested_at: chrono::Utc::now().to_rfc3339(),
    };
    let tmp = format!(
        "{}/.upgrade.request.{}",
        upgrade::RUN_DIR,
        std::process::id()
    );
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
        // A `Restarting` line means the new instance will read this file to
        // post its confirmation - leave it in place. Any other terminal
        // line (noop `Done`, `Error`) is fully handled here, so clear it.
        if lines.iter().any(|l| matches!(l, StatusLine::Restarting)) {
            return;
        }
        if lines.iter().any(|l| l.is_terminal()) {
            let _ = std::fs::remove_file(upgrade::STATUS_PATH);
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
        Some(StatusLine::Done {
            from,
            to,
            channel_id,
            requested_by,
            noop: false,
            ..
        }) if !channel_id.is_empty() => {
            let running = upgrade::current_version();
            if upgrade::Version::parse(to) == upgrade::Version::parse(running) {
                Some((
                    channel_id.clone(),
                    format!(
                        "✅ dispatchd upgraded v{from} → v{to} (requested by <@{requested_by}>)."
                    ),
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
        Some(StatusLine::Error {
            message,
            channel_id,
        }) if !channel_id.is_empty() => Some((
            channel_id.clone(),
            format!("❌ dispatchd upgrade failed: {message}"),
        )),
        _ => None,
    };

    if let Some((channel_id, text)) = msg
        && let Ok(raw) = channel_id.parse::<u64>()
        && let Err(e) = ChannelId::new(raw)
            .send_message(&ctx.http, CreateMessage::new().content(text))
            .await
    {
        eprintln!("failed to post upgrade confirmation: {e}");
    }

    let _ = std::fs::remove_file(upgrade::STATUS_PATH);
    let _ = std::fs::remove_file(upgrade::REQUEST_PATH);
}

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
    fn render_steps_marks_done_and_running_lines() {
        use crate::upgrade::StatusLine;
        let lines = vec![
            StatusLine::Checking,
            StatusLine::Found {
                current: "0.5.0".into(),
                latest: "v0.6.0".into(),
            },
            StatusLine::Downloading {
                asset: "dispatchd-x.tar.gz".into(),
            },
            StatusLine::Verified,
            StatusLine::Swapped,
            StatusLine::Restarting,
        ];
        let out = render_steps(&lines);
        assert!(out.contains('✓'));
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
        assert!(out.contains('❌'));
        assert!(out.contains("checksum verification failed"));
    }

    #[test]
    fn format_admin_status_wraps_all_three_sections_in_one_code_block() {
        let svc = ServiceStatus {
            supported: true,
            systemd_version: Some(252),
            min_systemd_version: 250,
            unit_installed: true,
            unit_enabled: Some("enabled".into()),
            unit_active: Some("active".into()),
            upgrade_helper_installed: true,
            cred_present: true,
        };
        let ping = DiscordPing {
            token_found: false,
            result: None,
        };
        let out = format_admin_status(&svc, &ping, "  version:              0.6.0 (up to date)");
        assert!(out.starts_with("```\n"));
        assert!(out.ends_with("```"));
        assert!(out.contains("systemd:"));
        assert!(out.contains("version:              0.6.0 (up to date)"));
        assert!(out.contains("discord:"));
    }
}

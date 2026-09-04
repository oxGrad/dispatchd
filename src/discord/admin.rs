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

pub async fn handle_help(ctx: &SerenityContext, command: &CommandInteraction) {
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

    let svc = service::status_report();
    let ping = discord_login::ping_report().await;
    let check = upgrade::check().await.map_err(|e| e.to_string());
    let body = format_admin_status(&svc, &ping, &version_line(check));
    ephemeral(ctx, command, body).await;
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
    fn format_admin_status_wraps_all_three_sections_in_one_code_block() {
        let svc = ServiceStatus {
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

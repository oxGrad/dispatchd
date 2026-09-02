use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    Context as SerenityContext, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, Permissions,
};

use crate::{entries, members, status};

pub fn command() -> CreateCommand {
    CreateCommand::new("team")
        .description("Tech-lead tools: status summary, full report, manual reminders")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "status",
            "One-line-per-member update summary for today",
        ))
}

/// Discord nests a subcommand's own options one level under an entry named
/// after the subcommand - this pulls out `(subcommand_name, its_options)`.
/// (Same shape as `todo::subcommand`.)
pub fn subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])> {
    let top = options.first()?;
    match &top.value {
        CommandDataOptionValue::SubCommand(nested) => Some((top.name.as_str(), nested)),
        _ => None,
    }
}

pub async fn handle_status(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let reply_text = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => "⛔ This command is restricted to the tech lead.".to_string(),
            Ok(true) => match status::team_status(&conn, &date) {
                Ok(rows) if rows.is_empty() => {
                    "No team members configured yet - see members.toml.".to_string()
                }
                Ok(rows) => rows
                    .iter()
                    .map(status::format_status_line)
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => {
                    eprintln!("failed to fetch team status: {e}");
                    "⚠️ Something went wrong fetching team status.".to_string()
                }
            },
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                "⚠️ Something went wrong checking permissions.".to_string()
            }
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /team status: {e}");
    }
}

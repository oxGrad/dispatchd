use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context as SerenityContext, CreateCommand,
    CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage, Permissions,
};

use crate::{entries, followups, members, recap};

use super::get_option_string;

pub fn command() -> CreateCommand {
    CreateCommand::new("missed")
        .description(
            "Who missed a /todo or /progress submission in a date range, tech-lead/admin only",
        )
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "start",
                "Start date YYYY-MM-DD (default: 14 days before end)",
            )
            .required(false),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "end",
                "End date YYYY-MM-DD (default: today)",
            )
            .required(false),
        )
}

/// `/missed` reads `missed_submissions`, the ticker's once-a-day snapshot
/// (`followups::record_missed`, taken at `day_summary_time`) - not a live
/// query - so today's row only shows up once that snapshot has actually
/// run. Same date-range handling as `/recap` (`recap::resolve_range`), and
/// small enough (at most a handful of rows for a 6-person team) that it
/// never needs `/recap`'s multi-message chunking.
pub async fn handle(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let today = entries::today_in(timezone);
    let start = get_option_string(&command.data.options, "start");
    let end = get_option_string(&command.data.options, "end");

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => "⛔ This command is restricted to the tech lead.".to_string(),
            Ok(true) => match recap::resolve_range(start.as_deref(), end.as_deref(), &today) {
                Err(msg) => msg,
                Ok((start, end)) => match followups::missed_summary(&conn, &start, &end) {
                    Ok(rows) => followups::format_missed_summary(&rows, &start, &end),
                    Err(e) => {
                        eprintln!("failed to build /missed: {e}");
                        "⚠️ Something went wrong building the report.".to_string()
                    }
                },
            },
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                "⚠️ Something went wrong checking permissions.".to_string()
            }
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(body)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /missed: {e}");
    }
}

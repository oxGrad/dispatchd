use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context as SerenityContext, CreateAttachment,
    CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, Permissions,
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
/// run. Same date-range handling as `/recap` (`recap::resolve_range`). The
/// reply is a short header plus, when there's anything to show, the full
/// report - per-member detail then a ranked summary table
/// (`followups::format_missed_report_file`) - as a `.md` file attachment
/// rather than chunked messages, same as `/recap`.
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
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) => match recap::resolve_range(start.as_deref(), end.as_deref(), &today) {
                Err(msg) => Err(msg),
                Ok((start, end)) => match followups::missed_detail(&conn, &start, &end) {
                    Ok(details) => Ok((start, end, details)),
                    Err(e) => {
                        eprintln!("failed to build /missed: {e}");
                        Err("⚠️ Something went wrong building the report.".to_string())
                    }
                },
            },
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                Err("⚠️ Something went wrong checking permissions.".to_string())
            }
        }
    };

    let (content, file) = match body {
        Ok((start, end, details)) => {
            match followups::format_missed_report_file(&details, &start, &end) {
                Some(file) => (
                    format!("📉 **Missed submissions ({start} to {end})** - see attached."),
                    Some((file, format!("missed-{start}_{end}.md"))),
                ),
                None => (
                    format!("✅ No missed submissions between {start} and {end}."),
                    None,
                ),
            }
        }
        Err(message) => (message, None),
    };

    let mut reply = CreateInteractionResponseMessage::new()
        .content(content)
        .ephemeral(true);
    if let Some((file, filename)) = file {
        reply = reply.add_file(CreateAttachment::bytes(file.into_bytes(), filename));
    }
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /missed: {e}");
    }
}

use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context as SerenityContext, CreateAttachment,
    CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, Permissions,
};

use crate::{entries, members, recap};

use super::get_option_string;

pub fn command() -> CreateCommand {
    CreateCommand::new("recap")
        .description("Multi-day recap: one table per day, tech-lead/admin only")
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
                Ok((start, end)) => match recap::recap_range(&conn, &start, &end) {
                    Ok(days) => Ok((start, end, days)),
                    Err(e) => {
                        eprintln!("failed to build /recap: {e}");
                        Err("⚠️ Something went wrong building the recap.".to_string())
                    }
                },
            },
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                Err("⚠️ Something went wrong checking permissions.".to_string())
            }
        }
    };

    // A file, not chunked messages: an attachment isn't bound by Discord's
    // 2000-char cap, and lets the table's columns actually line up.
    let (content, file) = match body {
        Ok((start, end, days)) => match recap::format_recap_file(&days) {
            Some(file) => (
                format!("📅 **Recap ({start} to {end})** - see attached."),
                Some((file, format!("recap-{start}_{end}.md"))),
            ),
            None => (format!("No activity between {start} and {end}."), None),
        },
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
        eprintln!("failed to respond to /recap: {e}");
    }
}

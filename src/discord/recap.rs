use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context as SerenityContext, CreateCommand,
    CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseFollowup,
    CreateInteractionResponseMessage, Permissions,
};

use crate::{entries, members, recap, status};

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
                    Ok(days) if days.is_empty() => {
                        Err(format!("No activity between {start} and {end}."))
                    }
                    Ok(days) => {
                        let full = days
                            .iter()
                            .map(recap::format_day_table)
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        // 1900, not Discord's 2000 cap: headroom in case a non-BMP
                        // emoji in user text counts as 2 against the limit (same
                        // margin as /team report).
                        Ok(status::split_into_messages(&full, 1900))
                    }
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

    let mut chunks = match body {
        Ok(chunks) => chunks.into_iter(),
        Err(message) => vec![message].into_iter(),
    };
    let first = chunks.next().unwrap_or_else(|| "No activity.".to_string());

    let reply = CreateInteractionResponseMessage::new()
        .content(first)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /recap: {e}");
        return;
    }

    for chunk in chunks {
        if let Err(e) = command
            .create_followup(
                &ctx.http,
                CreateInteractionResponseFollowup::new()
                    .content(chunk)
                    .ephemeral(true),
            )
            .await
        {
            eprintln!("failed to send /recap follow-up: {e}");
        }
    }
}

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
        .description("Recap for a day or date range: one table per day, tech-lead/admin only")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "start",
                "Start date YYYY-MM-DD (alone: just that day; default: today)",
            )
            .required(false),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "end",
                "End date YYYY-MM-DD (alone: just that day; default: today)",
            )
            .required(false),
        )
}

pub fn biweekly_command() -> CreateCommand {
    CreateCommand::new("recap-biweekly")
        .description("Recap of the 14 days ending yesterday, tech-lead/admin only")
        .default_member_permissions(Permissions::MANAGE_GUILD)
}

/// `/recap`: `start`/`end` resolved via `recap::resolve_day_or_range` -
/// neither = today, one = that single day, both = the range.
pub async fn handle(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let today = entries::today_in(timezone);
    let start = get_option_string(&command.data.options, "start");
    let end = get_option_string(&command.data.options, "end");
    let range = recap::resolve_day_or_range(start.as_deref(), end.as_deref(), &today);
    respond(ctx, command, db, range, "/recap").await;
}

/// `/recap-biweekly`: no options, always `recap::biweekly_range` - the
/// 14 days ending yesterday (what a bare `/recap` used to mean).
pub async fn handle_biweekly(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let today = entries::today_in(timezone);
    let range = recap::biweekly_range(&today);
    respond(ctx, command, db, range, "/recap-biweekly").await;
}

/// Shared by both commands once the range is resolved: the `is_lead` gate,
/// building the recap, and the header-plus-attachment reply.
async fn respond(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    range: Result<(String, String), String>,
    command_name: &str,
) {
    let discord_user_id = command.user.id.to_string();

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) => match range {
                Err(msg) => Err(msg),
                Ok((start, end)) => match recap::recap_range(&conn, &start, &end) {
                    Ok(days) => Ok((start, end, days)),
                    Err(e) => {
                        eprintln!("failed to build {command_name}: {e}");
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
                format!(
                    "📅 **Recap ({})** - see attached.",
                    range_label(&start, &end)
                ),
                Some((file, format!("recap-{start}_{end}.md"))),
            ),
            None => (format!("No activity {}.", range_phrase(&start, &end)), None),
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
        eprintln!("failed to respond to {command_name}: {e}");
    }
}

/// "2026-10-01" for a single day, "2026-10-01 to 2026-10-14" for a range -
/// now that a bare `/recap` (or `/missed`) is a single day, "X to X" would
/// read oddly.
pub(super) fn range_label(start: &str, end: &str) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start} to {end}")
    }
}

/// "on 2026-10-01" / "between 2026-10-01 and 2026-10-14", for the
/// empty-result replies.
pub(super) fn range_phrase(start: &str, end: &str) -> String {
    if start == end {
        format!("on {start}")
    } else {
        format!("between {start} and {end}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_label_collapses_a_single_day() {
        assert_eq!(range_label("2026-10-01", "2026-10-01"), "2026-10-01");
        assert_eq!(
            range_label("2026-10-01", "2026-10-14"),
            "2026-10-01 to 2026-10-14"
        );
    }

    #[test]
    fn range_phrase_collapses_a_single_day() {
        assert_eq!(range_phrase("2026-10-01", "2026-10-01"), "on 2026-10-01");
        assert_eq!(
            range_phrase("2026-10-01", "2026-10-14"),
            "between 2026-10-01 and 2026-10-14"
        );
    }
}

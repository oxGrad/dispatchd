use std::sync::{Arc, Mutex};

use chrono::{Datelike, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    CommandInteraction, CommandOptionType, Context as SerenityContext, CreateAttachment,
    CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, Permissions,
};

use crate::{entries, followups, members, recap, reminders};

use super::get_option_string;
use super::recap::{attachment_name, range_label, range_phrase};

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

/// `/missed` reads `missed_submissions`, the ticker's once-a-day snapshot
/// (`followups::record_missed`, taken at `day_summary_time`) - not a live
/// query - so today's row only shows up once that snapshot has actually
/// run - when the range includes today and that hasn't happened yet, the
/// reply says so (`pending_today_note`) rather than implying a clean day.
/// Same date handling as `/recap` (`recap::resolve_day_or_range`). The
/// reply is a short header plus, when there's anything to show, the full
/// report - per-member detail then a ranked summary table
/// (`followups::format_missed_report_file`) - as a `.md` file attachment
/// rather than chunked messages, same as `/recap`.
pub async fn handle(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    day_summary_time: NaiveTime,
    run_on_weekends: bool,
) {
    let discord_user_id = command.user.id.to_string();
    let today = entries::today_in(timezone);
    let start = get_option_string(&command.data.options, "start");
    let end = get_option_string(&command.data.options, "end");

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) => match recap::resolve_day_or_range(start.as_deref(), end.as_deref(), &today)
            {
                Err(msg) => Err(msg),
                Ok((start, end)) => match followups::missed_detail(&conn, &start, &end) {
                    Ok(details) => {
                        let includes_today =
                            start.as_str() <= today.as_str() && today.as_str() <= end.as_str();
                        // A DB error here only costs the note, not the report.
                        let recorded = reminders::already_sent(&conn, &today, "missed_recorded")
                            .unwrap_or_else(|e| {
                                eprintln!("failed to check missed_recorded status: {e}");
                                true
                            });
                        let note = pending_today_note(
                            includes_today,
                            recorded,
                            is_ritual_day(&today, run_on_weekends),
                            day_summary_time,
                        );
                        Ok((start, end, details, note))
                    }
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
        Ok((start, end, details, note)) => {
            let (header, file) = match followups::format_missed_report_file(&details, &start, &end)
            {
                Some(file) => (
                    format!(
                        "📉 **Missed submissions ({})** - see attached.",
                        range_label(&start, &end)
                    ),
                    Some((file, attachment_name("missed", &start, &end))),
                ),
                None => (
                    format!("✅ No missed submissions {}.", range_phrase(&start, &end)),
                    None,
                ),
            };
            match note {
                Some(note) => (format!("{header}\n{note}"), file),
                None => (header, file),
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

/// Whether the ticker records misses for `today` at all - it skips
/// weekends unless `run_on_weekends`, so promising a snapshot "at 16:00"
/// on a Saturday would be wrong.
fn is_ritual_day(today: &str, run_on_weekends: bool) -> bool {
    run_on_weekends
        || NaiveDate::parse_from_str(today, "%Y-%m-%d")
            .map(|d| !super::ticker::is_weekend(d.weekday()))
            .unwrap_or(true)
}

/// Pure - the "today isn't in yet" caveat appended to a `/missed` reply
/// whose range includes today before the ticker has taken today's
/// snapshot. Without it, a range ending today reads as "today was clean"
/// when today simply hasn't been checked yet. `None` when today is out of
/// range, already recorded, or a day the ritual doesn't run.
fn pending_today_note(
    includes_today: bool,
    recorded: bool,
    ritual_day: bool,
    day_summary_time: NaiveTime,
) -> Option<String> {
    if !includes_today || recorded || !ritual_day {
        return None;
    }
    Some(format!(
        "ℹ️ Today's misses haven't been recorded yet - that happens at {}.",
        day_summary_time.format("%H:%M")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    #[test]
    fn pending_today_note_explains_an_unrecorded_today() {
        assert_eq!(
            pending_today_note(true, false, true, t("16:00")),
            Some(
                "ℹ️ Today's misses haven't been recorded yet - that happens at 16:00.".to_string()
            )
        );
    }

    #[test]
    fn pending_today_note_is_none_once_recorded_or_out_of_range_or_off_day() {
        assert_eq!(pending_today_note(true, true, true, t("16:00")), None);
        assert_eq!(pending_today_note(false, false, true, t("16:00")), None);
        assert_eq!(pending_today_note(true, false, false, t("16:00")), None);
    }

    #[test]
    fn is_ritual_day_skips_weekends_unless_configured() {
        // 2026-10-17 is a Saturday, 2026-10-15 a Thursday.
        assert!(!is_ritual_day("2026-10-17", false));
        assert!(is_ritual_day("2026-10-17", true));
        assert!(is_ritual_day("2026-10-15", false));
    }
}

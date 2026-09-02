use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    Context as SerenityContext, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage, Http,
    Permissions,
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
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "report",
            "Full detail: everyone's todos and progress for today",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // caller lands in Task 9 (/team remind handler)
pub enum RemindKind {
    Todo,
    Progress,
}

#[allow(dead_code)] // caller lands in Task 9 (/team remind handler)
impl RemindKind {
    pub fn parse(s: &str) -> Option<RemindKind> {
        match s {
            "todo" => Some(RemindKind::Todo),
            "progress" => Some(RemindKind::Progress),
            _ => None,
        }
    }

    pub fn reminder_text(&self, user_id: &str) -> String {
        match self {
            RemindKind::Todo => format!(
                "👋 <@{user_id}> — reminder from the tech lead: please submit your `/todo` for today."
            ),
            RemindKind::Progress => format!(
                "👋 <@{user_id}> — reminder from the tech lead: please post a `/progress` update for today's todo(s)."
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)] // caller lands in Task 9 (/team remind handler)
pub enum SendOutcome {
    Sent,
    NoThread,
    ThreadGone,
    Failed,
}

/// Posts a manual reminder for `member_id` into today's standup thread.
/// Independent of the ticker's automated follow-ups - never reads or
/// writes `followups_sent`.
#[allow(dead_code)] // caller lands in Task 9 (/team remind handler)
pub async fn send_reminder(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    member_id: &str,
    kind: RemindKind,
) -> SendOutcome {
    let date = entries::today_in(timezone);

    let thread_id = {
        let conn = db.lock().expect("db mutex poisoned");
        crate::reminders::thread_for(&conn, &date)
    };
    let thread_id = match thread_id {
        Ok(Some(id)) => id,
        Ok(None) => return SendOutcome::NoThread,
        Err(e) => {
            eprintln!("failed to look up standup thread for /team remind: {e}");
            return SendOutcome::Failed;
        }
    };
    let Ok(raw_id) = thread_id.parse::<u64>() else {
        eprintln!("invalid stored thread_id {thread_id:?} for {date}");
        return SendOutcome::Failed;
    };

    match ChannelId::new(raw_id)
        .send_message(
            http,
            CreateMessage::new().content(kind.reminder_text(member_id)),
        )
        .await
    {
        Ok(_) => SendOutcome::Sent,
        Err(e) if super::is_unknown_channel_error(&e) => SendOutcome::ThreadGone,
        Err(e) => {
            eprintln!("failed to post /team remind message: {e}");
            SendOutcome::Failed
        }
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

pub async fn handle_report(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) => match status::team_report(&conn, &date) {
                Ok(reports) if reports.is_empty() => {
                    Err("No team members configured yet - see members.toml.".to_string())
                }
                Ok(reports) => {
                    let full = reports
                        .iter()
                        .map(status::format_report)
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Ok(status::split_into_messages(&full, 2000))
                }
                Err(e) => {
                    eprintln!("failed to fetch team report: {e}");
                    Err("⚠️ Something went wrong fetching the team report.".to_string())
                }
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
    let first = chunks
        .next()
        .unwrap_or_else(|| "No activity today.".to_string());

    let reply = CreateInteractionResponseMessage::new()
        .content(first)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /team report: {e}");
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
            eprintln!("failed to send /team report follow-up: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remind_kind_parses_known_values_only() {
        assert!(matches!(RemindKind::parse("todo"), Some(RemindKind::Todo)));
        assert!(matches!(
            RemindKind::parse("progress"),
            Some(RemindKind::Progress)
        ));
        assert!(RemindKind::parse("").is_none());
        assert!(RemindKind::parse("TODO").is_none());
        assert!(RemindKind::parse("nope").is_none());
    }

    #[test]
    fn reminder_text_mentions_the_user_and_the_right_command() {
        let todo = RemindKind::Todo.reminder_text("123");
        assert!(todo.contains("<@123>"));
        assert!(todo.contains("/todo"));

        let progress = RemindKind::Progress.reminder_text("456");
        assert!(progress.contains("<@456>"));
        assert!(progress.contains("/progress"));
    }
}

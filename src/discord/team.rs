use std::sync::{Arc, Mutex};

use chrono_tz::Tz;
use rusqlite::Connection;
use serenity::all::{
    AutocompleteChoice, ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction,
    CommandInteraction as AutocompleteInteraction, CommandOptionType, Context as SerenityContext,
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage, Http,
    Permissions,
};

use crate::{entries, members, status};

use super::get_option_string;

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
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "remind",
                "Remind a team member to submit a todo or progress update",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "member", "Who to remind")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "kind",
                    "What to remind them about",
                )
                .required(true)
                .add_string_choice("Submit a todo", "todo")
                .add_string_choice("Post a progress update", "progress"),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "skip-meeting",
            "Cancel today's meeting and tell the team you're all caught up",
        ))
}

/// The fixed announcement `/team skip-meeting` posts into the standup
/// thread. Plain text, no mentions.
pub const SKIP_MEETING_MESSAGE: &str = "🗓️ **No meeting today.** I've reviewed everyone's progress and I'm all caught up: no follow-ups or clarifications needed. Thanks, team, and enjoy the rest of your day!";

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
pub enum RemindKind {
    Todo,
    Progress,
}

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
                "👋 <@{user_id}>, reminder from the tech lead: please submit your `/todo` for today."
            ),
            RemindKind::Progress => format!(
                "👋 <@{user_id}>, reminder from the tech lead: please post a `/progress` update for today's todo(s)."
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    NoThread,
    ThreadGone,
    Failed,
}

/// Posts `content` into today's standup thread. Shared by `/team remind`
/// and `/team skip-meeting`. Independent of the ticker's automated
/// follow-ups - never reads or writes `followups_sent`.
pub async fn post_to_standup_thread(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    content: &str,
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
            eprintln!("failed to look up standup thread for a /team post: {e}");
            return SendOutcome::Failed;
        }
    };
    let Ok(raw_id) = thread_id.parse::<u64>() else {
        eprintln!("invalid stored thread_id {thread_id:?} for {date}");
        return SendOutcome::Failed;
    };

    match ChannelId::new(raw_id)
        .send_message(http, CreateMessage::new().content(content))
        .await
    {
        Ok(_) => SendOutcome::Sent,
        Err(e) if super::is_unknown_channel_error(&e) => SendOutcome::ThreadGone,
        Err(e) => {
            eprintln!("failed to post a /team message to the standup thread: {e}");
            SendOutcome::Failed
        }
    }
}

/// Posts a manual reminder for `member_id` into today's standup thread.
pub async fn send_reminder(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
    member_id: &str,
    kind: RemindKind,
) -> SendOutcome {
    post_to_standup_thread(http, db, timezone, &kind.reminder_text(member_id)).await
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
                    // 1900, not Discord's 2000 cap: headroom in case a non-BMP
                    // emoji in user text counts as 2 against the limit.
                    Ok(status::split_into_messages(&full, 1900))
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

pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
) {
    let partial = subcommand(&autocomplete.data.options)
        .and_then(|(_, opts)| get_option_string(opts, "member"))
        .unwrap_or_default()
        .to_lowercase();

    let roster = {
        let conn = db.lock().expect("db mutex poisoned");
        members::roster(&conn)
    };

    let response = match roster {
        Ok(rows) => {
            let choices = rows
                .into_iter()
                .filter(|(_, name)| name.to_lowercase().contains(&partial))
                .take(25)
                .map(|(id, name)| AutocompleteChoice::new(name, id))
                .collect();
            CreateAutocompleteResponse::new().set_choices(choices)
        }
        Err(e) => {
            eprintln!("failed to load roster for /team remind autocomplete: {e}");
            CreateAutocompleteResponse::new()
        }
    };

    if let Err(e) = autocomplete
        .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("failed to respond to /team remind autocomplete: {e}");
    }
}

pub async fn handle_remind(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let member = get_option_string(options, "member");
    let kind = get_option_string(options, "kind");

    let reply_text = 'reply: {
        let (member, kind) = match (member, kind) {
            (Some(m), Some(k)) => (m, k),
            _ => break 'reply "⚠️ Provide both a member and a kind.".to_string(),
        };
        let Some(kind) = RemindKind::parse(&kind) else {
            break 'reply "⚠️ Unknown reminder kind.".to_string();
        };

        let name = {
            let conn = db.lock().expect("db mutex poisoned");
            match members::is_lead(&conn, &discord_user_id) {
                Ok(false) => {
                    break 'reply "⛔ This command is restricted to the tech lead.".to_string();
                }
                Ok(true) => {}
                Err(e) => {
                    eprintln!("failed to check is_lead: {e}");
                    break 'reply "⚠️ Something went wrong checking permissions.".to_string();
                }
            }
            match members::name_of(&conn, &member) {
                Ok(Some(name)) => name,
                Ok(None) => break 'reply "⚠️ That user isn't on the team roster.".to_string(),
                Err(e) => {
                    eprintln!("failed to look up member name: {e}");
                    break 'reply "⚠️ Something went wrong looking up that member.".to_string();
                }
            }
        };

        match send_reminder(&ctx.http, db, timezone, &member, kind).await {
            SendOutcome::Sent => format!("✅ Reminder sent to {name} in today's standup thread."),
            SendOutcome::NoThread => {
                "⚠️ Today's standup thread hasn't been created yet - try again once it's posted."
                    .to_string()
            }
            SendOutcome::ThreadGone => {
                "⚠️ Today's standup thread appears to have been deleted.".to_string()
            }
            SendOutcome::Failed => "⚠️ Something went wrong sending the reminder.".to_string(),
        }
    };

    let reply = CreateInteractionResponseMessage::new()
        .content(reply_text)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to /team remind: {e}");
    }
}

pub async fn handle_skip_meeting(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &Tz,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let reply_text = 'reply: {
        {
            let conn = db.lock().expect("db mutex poisoned");
            match members::is_lead(&conn, &discord_user_id) {
                Ok(false) => {
                    break 'reply "⛔ This command is restricted to the tech lead.".to_string();
                }
                Ok(true) => {}
                Err(e) => {
                    eprintln!("failed to check is_lead: {e}");
                    break 'reply "⚠️ Something went wrong checking permissions.".to_string();
                }
            }
            match crate::reminders::already_sent(&conn, &date, "meeting_skip") {
                Ok(true) => {
                    break 'reply "ℹ️ The meeting is already marked skipped for today.".to_string();
                }
                Ok(false) => {}
                Err(e) => {
                    eprintln!("failed to check meeting_skip status: {e}");
                    break 'reply "⚠️ Something went wrong.".to_string();
                }
            }
        }

        match post_to_standup_thread(&ctx.http, db, timezone, SKIP_MEETING_MESSAGE).await {
            SendOutcome::Sent => {
                let conn = db.lock().expect("db mutex poisoned");
                if let Err(e) = crate::reminders::mark_sent(&conn, &date, "meeting_skip") {
                    eprintln!("failed to record meeting_skip: {e}");
                }
                // Suppress the automated pre-meeting ping if it hasn't fired yet.
                match crate::reminders::already_sent(&conn, &date, "meeting_reminder") {
                    Ok(false) => {
                        if let Err(e) =
                            crate::reminders::mark_sent(&conn, &date, "meeting_reminder")
                        {
                            eprintln!("failed to suppress meeting_reminder: {e}");
                        }
                    }
                    Ok(true) => {}
                    Err(e) => eprintln!("failed to check meeting_reminder status: {e}"),
                }
                "✅ Meeting skipped for today - the team has been notified in the standup thread."
                    .to_string()
            }
            SendOutcome::NoThread => {
                "⚠️ Today's standup thread hasn't been created yet - try again once it's posted."
                    .to_string()
            }
            SendOutcome::ThreadGone => {
                "⚠️ Today's standup thread appears to have been deleted.".to_string()
            }
            SendOutcome::Failed => {
                "⚠️ Something went wrong posting to the standup thread.".to_string()
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
        eprintln!("failed to respond to /team skip-meeting: {e}");
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
    fn skip_meeting_message_is_plain_and_closes_warmly() {
        assert!(!SKIP_MEETING_MESSAGE.contains('—'));
        assert!(!SKIP_MEETING_MESSAGE.contains("<@"));
        assert!(SKIP_MEETING_MESSAGE.contains("No meeting today"));
        assert!(SKIP_MEETING_MESSAGE.contains("enjoy the rest of your day"));
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

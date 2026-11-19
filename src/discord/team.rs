use std::sync::{Arc, Mutex};

use jiff::tz::TimeZone;
use rusqlite::Connection;
use serenity::all::{
    AutocompleteChoice, ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction,
    CommandInteraction as AutocompleteInteraction, CommandOptionType, Context as SerenityContext,
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage, Http,
    Permissions,
};

use crate::cutoffs::{self, Rules};
use crate::leave::{self, LeaveDay, Reason};
use crate::{entries, followups, members, status};

use super::get_option_string;

pub fn command() -> CreateCommand {
    CreateCommand::new("team")
        .description("Tech-lead tools: status, full report, reminders, GitHub activity")
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
        .add_option(leave_group())
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "github",
                "Per-person GitHub activity: PRs, closed sub-issues, reviews",
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "start",
                    "Start date YYYY-MM-DD (alone: just that day; default: today)",
                )
                .required(false),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "end",
                    "End date YYYY-MM-DD (alone: just that day; default: today)",
                )
                .required(false),
            ),
        )
}

fn date_option(name: &str, desc: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::String, name, desc).required(false)
}

fn member_option(desc: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::String, "member", desc)
        .required(true)
        .set_autocomplete(true)
}

fn leave_group() -> CreateCommandOption {
    let start = "Start date YYYY-MM-DD (alone: just that day; default: today)";
    let end = "End date YYYY-MM-DD (alone: just that day; default: today)";
    CreateCommandOption::new(
        CommandOptionType::SubCommandGroup,
        "leave",
        "Mark members on leave or sick",
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::SubCommand, "add", "Mark a member away")
            .add_sub_option(member_option("Who is away"))
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "reason", "Why")
                    .required(true)
                    .add_string_choice("Leave", "leave")
                    .add_string_choice("Sick", "sick"),
            )
            .add_sub_option(date_option("start", start))
            .add_sub_option(date_option("end", end)),
    )
    .add_sub_option(
        CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "remove",
            "Clear a member's leave days",
        )
        .add_sub_option(member_option("Whose leave to clear"))
        .add_sub_option(date_option("start", start))
        .add_sub_option(date_option("end", end)),
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::SubCommand, "list", "List leave days")
            .add_sub_option(date_option(
                "start",
                "Start date YYYY-MM-DD (default: this month)",
            ))
            .add_sub_option(date_option(
                "end",
                "End date YYYY-MM-DD (default: this month)",
            )),
    )
}

/// `Some((sub, options))` when the top option is the `leave` group.
pub fn leave_subcommand(options: &[CommandDataOption]) -> Option<(&str, &[CommandDataOption])> {
    let top = options.first()?;
    match &top.value {
        CommandDataOptionValue::SubCommandGroup(nested) if top.name == "leave" => {
            subcommand(nested)
        }
        _ => None,
    }
}

fn leave_added_reply(name: &str, reason: Reason, days: &[String]) -> String {
    if days.is_empty() {
        return "⚠️ No working days in that range - nothing recorded.".to_string();
    }
    let dates: Vec<String> = days.iter().map(|d| entries::short_date(d)).collect();
    format!(
        "✅ {name} marked {} for {}.",
        reason.label(),
        dates.join(", ")
    )
}

fn leave_removed_reply(name: &str, removed: usize) -> String {
    format!("✅ Removed {removed} leave day(s) for {name}.")
}

fn format_leave_list(rows: &[LeaveDay], start: &str, end: &str) -> String {
    if rows.is_empty() {
        return format!("No leave between {start} and {end}.");
    }
    let mut out = format!("🏖️ **Leave {start} to {end}**");
    for r in rows {
        out.push_str(&format!(
            "\n• {} · {} · {}",
            entries::short_date(&r.date),
            r.name,
            r.reason.label()
        ));
    }
    out
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
    timezone: &TimeZone,
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
    timezone: &TimeZone,
    member_id: &str,
    kind: RemindKind,
) -> SendOutcome {
    post_to_standup_thread(http, db, timezone, &kind.reminder_text(member_id)).await
}

pub async fn handle_status(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    carryover_lookback_days: u32,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let reply_text = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => "⛔ This command is restricted to the tech lead.".to_string(),
            Ok(true) => match status::team_status(&conn, &date, carryover_lookback_days as i64) {
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
    timezone: &TimeZone,
    carryover_lookback_days: u32,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) => match status::team_report(&conn, &date, carryover_lookback_days as i64) {
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

    reply_in_chunks(ctx, command, body, "No activity today.", "/team report").await;
}

/// The ephemeral reply shared by `/team report` and `/team github`: the
/// first chunk (or `empty` when there are none) as the response, the rest
/// as follow-ups; an `Err` is a single-message reply.
async fn reply_in_chunks(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    body: Result<Vec<String>, String>,
    empty: &str,
    command_name: &str,
) {
    let mut chunks = match body {
        Ok(chunks) => chunks.into_iter(),
        Err(message) => vec![message].into_iter(),
    };
    let first = chunks.next().unwrap_or_else(|| empty.to_string());

    let reply = CreateInteractionResponseMessage::new()
        .content(first)
        .ephemeral(true);
    if let Err(e) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
        .await
    {
        eprintln!("failed to respond to {command_name}: {e}");
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
            eprintln!("failed to send {command_name} follow-up: {e}");
        }
    }
}

/// `/team github [start] [end]`: per-person GitHub activity
/// (`github::report::team_github`) for a day or range, same date rule as
/// `/recap`. Same lead/viewer gate as `/team status`/`report`; ephemeral.
pub async fn handle_github(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    github_configured: bool,
) {
    let discord_user_id = command.user.id.to_string();
    let today = entries::today_in(timezone);
    let start = get_option_string(options, "start");
    let end = get_option_string(options, "end");

    let body = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_lead(&conn, &discord_user_id) {
            Ok(false) => Err("⛔ This command is restricted to the tech lead.".to_string()),
            Ok(true) if !github_configured => Err(
                "GitHub isn't configured - see docs/github-setup.md to set up the GitHub App."
                    .to_string(),
            ),
            Ok(true) => {
                match crate::recap::resolve_day_or_range(start.as_deref(), end.as_deref(), &today) {
                    Err(msg) => Err(msg),
                    Ok((start, end)) => {
                        match crate::github::report::team_github(&conn, &start, &end, timezone) {
                            Ok(text) => Ok(status::split_into_messages(&text, 1900)),
                            Err(e) => {
                                eprintln!("failed to build /team github: {e:#}");
                                Err("⚠️ Something went wrong building the GitHub summary."
                                    .to_string())
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("failed to check is_lead: {e}");
                Err("⚠️ Something went wrong checking permissions.".to_string())
            }
        }
    };

    reply_in_chunks(ctx, command, body, "No GitHub activity.", "/team github").await;
}

pub async fn handle_autocomplete(
    ctx: &SerenityContext,
    autocomplete: &AutocompleteInteraction,
    db: &Arc<Mutex<Connection>>,
) {
    let opts = &autocomplete.data.options;
    let partial = leave_subcommand(opts)
        .or_else(|| subcommand(opts))
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
    timezone: &TimeZone,
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
            match members::is_active_lead(&conn, &discord_user_id) {
                Ok(false) => {
                    break 'reply "⛔ This command is restricted to the tech lead.".to_string();
                }
                Ok(true) => {}
                Err(e) => {
                    eprintln!("failed to check is_active_lead: {e}");
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

/// `/team leave add|remove|list`. Lead-only, ephemeral.
#[allow(clippy::too_many_arguments)]
pub async fn handle_leave(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    sub: &str,
    options: &[CommandDataOption],
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
    rules: &Rules,
    run_on_weekends: bool,
) {
    let user_id = command.user.id.to_string();
    let today = entries::today_in(timezone);
    let start = get_option_string(options, "start");
    let end = get_option_string(options, "end");

    let reply_text = 'reply: {
        let conn = db.lock().expect("db mutex poisoned");
        match members::is_active_lead(&conn, &user_id) {
            Ok(true) => {}
            Ok(false) => {
                break 'reply "⛔ This command is restricted to the tech lead.".to_string();
            }
            Err(e) => {
                eprintln!("failed to check is_active_lead: {e}");
                break 'reply "⚠️ Something went wrong checking permissions.".to_string();
            }
        }
        let failed = |e: anyhow::Error| {
            eprintln!("/team leave {sub} failed: {e:#}");
            "⚠️ Something went wrong - please try again.".to_string()
        };

        if sub == "list" {
            let (s, e) = match crate::recap::resolve_month_or_range(
                start.as_deref(),
                end.as_deref(),
                &today,
                366,
            ) {
                Ok(r) => r,
                Err(msg) => break 'reply msg,
            };
            break 'reply match leave::list(&conn, &s, &e) {
                Ok(rows) => format_leave_list(&rows, &s, &e),
                Err(e) => failed(e),
            };
        }

        let Some(member) = get_option_string(options, "member") else {
            break 'reply "⚠️ Provide a member.".to_string();
        };
        let reason = match sub {
            "add" => match get_option_string(options, "reason").and_then(|r| Reason::parse(&r)) {
                Some(r) => Some(r),
                None => break 'reply "⚠️ Provide a reason (leave or sick).".to_string(),
            },
            _ => None,
        };
        match members::role_of(&conn, &member) {
            Ok(Some(role)) if role != "viewer" => {}
            Ok(_) => {
                break 'reply "⚠️ That member isn't tracked (not on the roster, or a viewer)."
                    .to_string();
            }
            Err(e) => break 'reply failed(e),
        }
        let name = match members::name_of(&conn, &member) {
            Ok(Some(n)) => n,
            Ok(None) => member.clone(),
            Err(e) => break 'reply failed(e),
        };
        let (s, e) =
            match crate::recap::resolve_day_or_range(start.as_deref(), end.as_deref(), &today) {
                Ok(r) => r,
                Err(msg) => break 'reply msg,
            };
        let days = match leave::ritual_days(&s, &e, run_on_weekends) {
            Ok(d) => d,
            Err(e) => break 'reply failed(e),
        };

        let result = (|| -> anyhow::Result<String> {
            let text = match reason {
                Some(reason) => {
                    leave::add(&conn, &member, &days, reason)?;
                    leave_added_reply(&name, reason, &days)
                }
                None => leave_removed_reply(&name, leave::remove(&conn, &member, &days)?),
            };
            let since = cutoffs::rules_since(&conn)?;
            followups::rederive_member(&conn, &member, &days, rules, since.as_deref())?;
            Ok(text)
        })();
        result.unwrap_or_else(failed)
    };

    let chunks = status::split_into_messages(&reply_text, 1900);
    reply_in_chunks(ctx, command, Ok(chunks), "No leave.", "/team leave").await;
}

pub async fn handle_skip_meeting(
    ctx: &SerenityContext,
    command: &CommandInteraction,
    db: &Arc<Mutex<Connection>>,
    timezone: &TimeZone,
) {
    let discord_user_id = command.user.id.to_string();
    let date = entries::today_in(timezone);

    let reply_text = 'reply: {
        {
            let conn = db.lock().expect("db mutex poisoned");
            match members::is_active_lead(&conn, &discord_user_id) {
                Ok(false) => {
                    break 'reply "⛔ This command is restricted to the tech lead.".to_string();
                }
                Ok(true) => {}
                Err(e) => {
                    eprintln!("failed to check is_active_lead: {e}");
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
    fn leave_replies() {
        assert_eq!(
            leave_added_reply(
                "Bima",
                Reason::Sick,
                &["2026-11-20".into(), "2026-11-23".into()]
            ),
            "✅ Bima marked Sick for Nov 20, Nov 23."
        );
        assert_eq!(
            leave_added_reply("Bima", Reason::Leave, &[]),
            "⚠️ No working days in that range - nothing recorded."
        );
        assert_eq!(
            leave_removed_reply("Bima", 2),
            "✅ Removed 2 leave day(s) for Bima."
        );
    }

    #[test]
    fn long_leave_list_splits_within_discord_limit() {
        let rows: Vec<LeaveDay> = (1..=28)
            .flat_map(|d| {
                (0..4).map(move |m| LeaveDay {
                    date: format!("2026-11-{d:02}"),
                    discord_user_id: m.to_string(),
                    name: format!("Member{m}"),
                    reason: Reason::Leave,
                })
            })
            .collect();
        let text = format_leave_list(&rows, "2026-11-01", "2026-11-30");
        assert!(text.len() > 2000);
        let chunks = status::split_into_messages(&text, 2000);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.chars().count() <= 2000));
        let joined = chunks.join("\n");
        for line in text.lines() {
            assert!(joined.contains(line), "missing {line}");
        }
    }

    #[test]
    fn leave_list_formats_one_line_per_day() {
        let rows = vec![LeaveDay {
            date: "2026-11-20".into(),
            discord_user_id: "1".into(),
            name: "Bima".into(),
            reason: Reason::Sick,
        }];
        assert_eq!(
            format_leave_list(&rows, "2026-11-01", "2026-11-30"),
            "🏖️ **Leave 2026-11-01 to 2026-11-30**\n• Nov 20 · Bima · Sick"
        );
        assert_eq!(
            format_leave_list(&[], "2026-11-01", "2026-11-30"),
            "No leave between 2026-11-01 and 2026-11-30."
        );
    }

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

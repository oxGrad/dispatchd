use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Datelike, NaiveTime, Weekday};
use rusqlite::Connection;
use serenity::all::{
    ChannelId, ChannelType, CreateMessage, CreateThread, Error as SerenityError, Http, HttpError,
};

use crate::config::Config;
use crate::{followups, members, reminders};

/// Pure time comparison - testable without a live clock or Discord types.
fn is_due(now: NaiveTime, trigger: NaiveTime) -> bool {
    now >= trigger
}

/// Pure weekday check - testable without a live clock or Discord types.
fn is_weekend(day: Weekday) -> bool {
    matches!(day, Weekday::Sat | Weekday::Sun)
}

/// Discord's JSON error code for "Unknown Channel" - returned when a
/// request targets a channel or thread that no longer exists (e.g.
/// someone deleted today's standup thread mid-day).
const UNKNOWN_CHANNEL_ERROR_CODE: isize = 10003;

fn is_unknown_channel_code(code: isize) -> bool {
    code == UNKNOWN_CHANNEL_ERROR_CODE
}

/// True when `err` is Discord reporting that the channel/thread a send
/// targeted no longer exists, as opposed to a transient failure (rate
/// limit, network blip, a permissions problem) that's worth retrying on
/// the next tick. Not unit-tested like the pure helper above - building a
/// real `serenity::Error` needs a `reqwest::Method`, and reqwest isn't (and
/// shouldn't become) a direct dependency of this crate just for a test -
/// same "can't exercise real Discord types without a live connection"
/// limitation as the rest of this codebase's Discord-facing code.
fn is_unknown_channel_error(err: &SerenityError) -> bool {
    matches!(
        err,
        SerenityError::Http(HttpError::UnsuccessfulRequest(response))
            if is_unknown_channel_code(response.error.code)
    )
}

/// Runs forever, checking every `config.ticker_interval_seconds` whether
/// any of the three daily reminders are due. `tokio::time::interval` fires
/// immediately on its first tick, which doubles as the doc's "plus a check
/// on startup" - no special-casing needed.
pub async fn run(
    http: Arc<Http>,
    db: Arc<Mutex<Connection>>,
    channel_id: ChannelId,
    config: Config,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.ticker_interval_seconds));
    loop {
        interval.tick().await;
        tick(&http, &db, channel_id, &config).await;
    }
}

async fn tick(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    channel_id: ChannelId,
    config: &Config,
) {
    let now = chrono::Utc::now().with_timezone(&config.timezone);
    if !config.run_on_weekends && is_weekend(now.weekday()) {
        return;
    }
    let date = now.format("%Y-%m-%d").to_string();
    let now_time = now.time();

    if is_due(now_time, config.todo_time) {
        maybe_fire_todo_reminder(http, db, channel_id, &date).await;
    }
    if is_due(now_time, config.update_time) {
        maybe_fire_simple_reminder(
            http,
            db,
            &date,
            "update_reminder",
            "⏰ Time for your afternoon update! Submit via `/update`.",
        )
        .await;
    }
    if is_due(now_time, config.meeting_reminder_time) {
        maybe_fire_simple_reminder(
            http,
            db,
            &date,
            "meeting_reminder",
            "🗓️ Optional meeting time - whether it happens today is the tech lead's call.",
        )
        .await;
    }

    let todo_followup_trigger =
        config.todo_time + chrono::Duration::minutes(config.todo_followup_delay_minutes.into());
    let update_followup_trigger =
        config.update_time + chrono::Duration::minutes(config.update_followup_delay_minutes.into());

    if is_due(now_time, todo_followup_trigger) {
        maybe_fire_followups(
            http,
            db,
            &date,
            "todo_followup",
            followups::members_missing_todo,
            "📋 don't forget to submit your `/todo` for today!",
        )
        .await;
    }
    if is_due(now_time, update_followup_trigger) {
        maybe_fire_followups(
            http,
            db,
            &date,
            "update_followup",
            followups::members_missing_update,
            "⏰ don't forget to submit an `/update` for today's todo(s)!",
        )
        .await;
    }
}

async fn maybe_fire_todo_reminder(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    channel_id: ChannelId,
    date: &str,
) {
    match already_sent(db, date, "todo_reminder") {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            eprintln!("failed to check todo_reminder status: {e}");
            return;
        }
    }

    let mentions = {
        let conn = db.lock().expect("db mutex poisoned");
        match members::all_member_ids(&conn) {
            Ok(ids) => ids
                .iter()
                .map(|id| format!("<@{id}>"))
                .collect::<Vec<_>>()
                .join(" "),
            Err(e) => {
                eprintln!("failed to list members for standup ping: {e}");
                String::new()
            }
        }
    };

    let thread = match channel_id
        .create_thread(
            http,
            CreateThread::new(format!("Standup — {date}")).kind(ChannelType::PublicThread),
        )
        .await
    {
        Ok(thread) => thread,
        Err(e) => {
            eprintln!("failed to create standup thread: {e}");
            return;
        }
    };

    let content =
        format!("{mentions}\n📋 Time for today's standup! Submit your todo with `/todo`.");
    if let Err(e) = thread
        .id
        .send_message(http, CreateMessage::new().content(content))
        .await
    {
        eprintln!("failed to post todo prompt: {e}");
    }

    let conn = db.lock().expect("db mutex poisoned");
    if let Err(e) = reminders::save_thread(&conn, date, &thread.id.to_string()) {
        eprintln!("failed to save standup thread id: {e}");
    }
    if let Err(e) = reminders::mark_sent(&conn, date, "todo_reminder") {
        eprintln!("failed to mark todo_reminder sent: {e}");
    }
}

async fn maybe_fire_simple_reminder(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    date: &str,
    kind: &str,
    message: &str,
) {
    match already_sent(db, date, kind) {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            eprintln!("failed to check {kind} status: {e}");
            return;
        }
    }

    let thread_id = {
        let conn = db.lock().expect("db mutex poisoned");
        reminders::thread_for(&conn, date)
    };
    let thread_id = match thread_id {
        Ok(Some(id)) => id,
        Ok(None) => {
            // The todo reminder never fired today (bot was down, or the
            // standup channel was only just configured) - skip rather
            // than inventing a late thread.
            eprintln!("no standup thread yet for {date}, skipping {kind}");
            return;
        }
        Err(e) => {
            eprintln!("failed to look up standup thread for {kind}: {e}");
            return;
        }
    };

    let Ok(raw_id) = thread_id.parse::<u64>() else {
        eprintln!("invalid stored thread_id {thread_id:?} for {date}");
        return;
    };

    if let Err(e) = ChannelId::new(raw_id)
        .send_message(http, CreateMessage::new().content(message))
        .await
    {
        if is_unknown_channel_error(&e) {
            // The thread was deleted - marking sent anyway stops this from
            // retrying (and failing identically) every tick for the rest
            // of the day.
            eprintln!(
                "standup thread for {date} no longer exists (deleted?) - giving up on {kind} for today: {e}"
            );
            let conn = db.lock().expect("db mutex poisoned");
            if let Err(e) = reminders::mark_sent(&conn, date, kind) {
                eprintln!("failed to mark {kind} sent: {e}");
            }
        } else {
            eprintln!("failed to post {kind}: {e}");
        }
        return;
    }

    let conn = db.lock().expect("db mutex poisoned");
    if let Err(e) = reminders::mark_sent(&conn, date, kind) {
        eprintln!("failed to mark {kind} sent: {e}");
    }
}

fn already_sent(db: &Arc<Mutex<Connection>>, date: &str, kind: &str) -> anyhow::Result<bool> {
    let conn = db.lock().expect("db mutex poisoned");
    reminders::already_sent(&conn, date, kind)
}

/// Nags each member `query` returns as missing something for `date`, one
/// in-thread `@mention` per person, skipping anyone already nagged today
/// (per the doc's "at most once" framing - a restart mid-way through
/// nagging several people won't re-nag the ones already done).
async fn maybe_fire_followups(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    date: &str,
    kind: &str,
    query: fn(&Connection, &str) -> anyhow::Result<Vec<String>>,
    message: &str,
) {
    let missing = {
        let conn = db.lock().expect("db mutex poisoned");
        query(&conn, date)
    };
    let missing = match missing {
        Ok(ids) if ids.is_empty() => return,
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("failed to list members missing {kind}: {e}");
            return;
        }
    };

    let thread_id = {
        let conn = db.lock().expect("db mutex poisoned");
        reminders::thread_for(&conn, date)
    };
    let thread_id = match thread_id {
        Ok(Some(id)) => id,
        Ok(None) => {
            eprintln!("no standup thread yet for {date}, skipping {kind}");
            return;
        }
        Err(e) => {
            eprintln!("failed to look up standup thread for {kind}: {e}");
            return;
        }
    };
    let Ok(raw_id) = thread_id.parse::<u64>() else {
        eprintln!("invalid stored thread_id {thread_id:?} for {date}");
        return;
    };
    let channel_id = ChannelId::new(raw_id);

    // Set once the thread is found gone - every remaining member this tick
    // is then marked sent without another doomed send, instead of each one
    // failing identically against the same deleted thread.
    let mut thread_gone = false;

    for discord_user_id in missing {
        let already = {
            let conn = db.lock().expect("db mutex poisoned");
            followups::already_sent(&conn, date, &discord_user_id, kind)
        };
        match already {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                eprintln!("failed to check {kind} status for {discord_user_id}: {e}");
                continue;
            }
        }

        if !thread_gone {
            let content = format!("<@{discord_user_id}> {message}");
            if let Err(e) = channel_id
                .send_message(http, CreateMessage::new().content(content))
                .await
            {
                if is_unknown_channel_error(&e) {
                    eprintln!(
                        "standup thread for {date} no longer exists (deleted?) - giving up on {kind} for the rest of today: {e}"
                    );
                    thread_gone = true;
                } else {
                    eprintln!("failed to post {kind} to {discord_user_id}: {e}");
                    continue;
                }
            }
        }

        // Reached with a successful send, or after just detecting the
        // thread is gone (marking here, not retrying, stops the
        // per-person per-tick retry loop for the rest of today).
        let conn = db.lock().expect("db mutex poisoned");
        if let Err(e) = followups::mark_sent(&conn, date, &discord_user_id, kind) {
            eprintln!("failed to mark {kind} sent for {discord_user_id}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_due_before_trigger_time_is_false() {
        let trigger = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let now = NaiveTime::from_hms_opt(8, 59, 59).unwrap();
        assert!(!is_due(now, trigger));
    }

    #[test]
    fn is_due_at_or_after_trigger_time_is_true() {
        let trigger = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert!(is_due(trigger, trigger));
        assert!(is_due(NaiveTime::from_hms_opt(9, 0, 1).unwrap(), trigger));
        assert!(is_due(NaiveTime::from_hms_opt(15, 0, 0).unwrap(), trigger));
    }

    #[test]
    fn is_weekend_is_true_only_for_saturday_and_sunday() {
        assert!(is_weekend(Weekday::Sat));
        assert!(is_weekend(Weekday::Sun));
        assert!(!is_weekend(Weekday::Mon));
        assert!(!is_weekend(Weekday::Tue));
        assert!(!is_weekend(Weekday::Wed));
        assert!(!is_weekend(Weekday::Thu));
        assert!(!is_weekend(Weekday::Fri));
    }
}

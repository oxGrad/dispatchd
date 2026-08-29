use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveTime;
use rusqlite::Connection;
use serenity::all::{ChannelId, ChannelType, CreateMessage, CreateThread, Http};

use crate::config::Config;
use crate::{members, reminders};

/// Pure time comparison - testable without a live clock or Discord types.
fn is_due(now: NaiveTime, trigger: NaiveTime) -> bool {
    now >= trigger
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
        eprintln!("failed to post {kind}: {e}");
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
}

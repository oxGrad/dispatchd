use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Datelike, NaiveTime, Weekday};
use rusqlite::Connection;
use serenity::all::{ChannelId, ChannelType, CreateMessage, CreateThread, Http};

use crate::config::Config;
use crate::{entries, followups, members, reminders};

/// Pure time comparison - testable without a live clock or Discord types.
fn is_due(now: NaiveTime, trigger: NaiveTime) -> bool {
    now >= trigger
}

/// Pure weekday check - testable without a live clock or Discord types.
fn is_weekend(day: Weekday) -> bool {
    matches!(day, Weekday::Sat | Weekday::Sun)
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

    if is_due(now_time, config.thread_creation_time) {
        maybe_create_thread(http, db, channel_id, &date).await;
    }
    if is_due(now_time, config.todo_time) {
        maybe_fire_todo_reminder(http, db, &date).await;
    }
    if is_due(now_time, config.update_time) {
        maybe_fire_simple_reminder(
            http,
            db,
            &date,
            "update_reminder",
            "⏰ Time for your afternoon progress report! Submit via `/progress`.",
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
            "⏰ don't forget to submit a `/progress` report for today's todo(s)!",
        )
        .await;
    }

    maybe_sync_thread(http, db, &date).await;
}

/// Creates today's standup thread ahead of the actual todo prompt (see
/// `maybe_fire_todo_reminder` below), so early submissions have somewhere
/// to sync into even before the 9am ping fires. No message is posted here -
/// Discord's own "X started a thread: Standup: <date>" system line is
/// enough; the todo prompt still does the @mention/ping once it fires.
async fn maybe_create_thread(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    channel_id: ChannelId,
    date: &str,
) {
    match already_sent(db, date, "thread_creation") {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            eprintln!("failed to check thread_creation status: {e}");
            return;
        }
    }

    let thread = match channel_id
        .create_thread(
            http,
            CreateThread::new(format!("Standup: {date}")).kind(ChannelType::PublicThread),
        )
        .await
    {
        Ok(thread) => thread,
        Err(e) => {
            eprintln!("failed to create standup thread: {e}");
            return;
        }
    };

    let conn = db.lock().expect("db mutex poisoned");
    if let Err(e) = reminders::save_thread(&conn, date, &thread.id.to_string()) {
        eprintln!("failed to save standup thread id: {e}");
    }
    if let Err(e) = reminders::mark_sent(&conn, date, "thread_creation") {
        eprintln!("failed to mark thread_creation sent: {e}");
    }
}

/// Pings the team in today's thread with the `/todo create` prompt. The
/// thread itself is created separately by `maybe_create_thread` above (at
/// the earlier, independently configurable `thread_creation_time`) - this
/// only looks one up, skipping with a log line if none exists yet (bot was
/// down at both trigger times, or the channel was only just configured),
/// same as the 3pm/4pm reminders below.
async fn maybe_fire_todo_reminder(http: &Arc<Http>, db: &Arc<Mutex<Connection>>, date: &str) {
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

    let message =
        format!("{mentions}\n📋 Time for today's standup! Submit your todo with `/todo create`.");
    maybe_fire_simple_reminder(http, db, date, "todo_reminder", &message).await;
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
            // Today's thread hasn't been created yet (bot was down, or the
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
        if super::is_unknown_channel_error(&e) {
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

/// Posts every `/todo`/`/progress` submission since the last tick into
/// today's thread, so the team actually sees each other's activity (the
/// bot's replies to those commands are otherwise ephemeral - private to the
/// submitter). Runs every tick unconditionally; a no-op when there's no
/// thread yet or nothing new to post.
async fn maybe_sync_thread(http: &Arc<Http>, db: &Arc<Mutex<Connection>>, date: &str) {
    let thread_id = {
        let conn = db.lock().expect("db mutex poisoned");
        reminders::thread_for(&conn, date)
    };
    let thread_id = match thread_id {
        Ok(Some(id)) => id,
        Ok(None) => return,
        Err(e) => {
            eprintln!("failed to look up standup thread for sync: {e}");
            return;
        }
    };
    let Ok(raw_id) = thread_id.parse::<u64>() else {
        eprintln!("invalid stored thread_id {thread_id:?} for {date}");
        return;
    };
    let channel_id = ChannelId::new(raw_id);

    let cursor = {
        let conn = db.lock().expect("db mutex poisoned");
        reminders::sync_cursor(&conn, date)
    };
    let cursor = match cursor {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read sync cursor for {date}: {e}");
            return;
        }
    };

    let new_entries = {
        let conn = db.lock().expect("db mutex poisoned");
        entries::entries_since(&conn, date, cursor)
    };
    let new_entries = match new_entries {
        Ok(rows) if rows.is_empty() => return,
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("failed to list new entries to sync for {date}: {e}");
            return;
        }
    };

    // Best-effort, at-most-once, same stance as the reminders above: every
    // entry gets exactly one send attempt, then the cursor moves past it
    // regardless of outcome - a transient failure (or a deleted thread)
    // just drops that one entry from the thread rather than retrying it
    // forever.
    let mut synced_through = cursor;
    for entry in &new_entries {
        let content = format_sync_message(entry);
        if let Err(e) = channel_id
            .send_message(http, CreateMessage::new().content(content))
            .await
        {
            if super::is_unknown_channel_error(&e) {
                eprintln!(
                    "standup thread for {date} no longer exists (deleted?) - skipping sync for entry {}: {e}",
                    entry.id
                );
            } else {
                eprintln!("failed to sync entry {} to thread: {e}", entry.id);
            }
        }
        synced_through = entry.id;
    }

    if synced_through > cursor {
        let conn = db.lock().expect("db mutex poisoned");
        if let Err(e) = reminders::advance_sync_cursor(&conn, date, synced_through) {
            eprintln!("failed to advance sync cursor for {date}: {e}");
        }
    }
}

/// Pure, unit-testable without any serenity types - same style as
/// `is_due`/`is_weekend` above.
fn format_sync_message(entry: &entries::SyncEntry) -> String {
    match entry.entry_type.as_str() {
        "todo" => {
            let mut message = format!(
                "📋 <@{}> added a todo: **{}**",
                entry.discord_user_id, entry.task
            );
            if let Some(sow_ref) = &entry.sow_ref {
                message.push_str(&format!(" [{sow_ref}]"));
            }
            if let Some(notes) = non_empty(&entry.notes) {
                message.push_str(&quote_block(&format!("_{notes}_")));
            }
            message
        }
        _ => {
            let (emoji, label) = match entry.status.as_deref() {
                Some("done") => ("✅", "Done"),
                Some("blocked") => ("⚠️", "Blocked"),
                _ => ("🔧", "In Progress"),
            };
            let mut message = format!(
                "{emoji} <@{}> progress on **{}**: {label}",
                entry.discord_user_id, entry.task
            );
            if let Some(progress) = non_empty(&entry.progress) {
                message.push_str(&quote_block(progress));
            }
            if let Some(blocker) = non_empty(&entry.blocker) {
                message.push_str(&quote_block(&format!("blocked on: {blocker}")));
            }
            message
        }
    }
}

/// The trimmed inner text of an optional field, or `None` when it is
/// absent or whitespace-only.
fn non_empty(field: &Option<String>) -> Option<&str> {
    field.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Renders `text` as a Discord blockquote appended below the current line:
/// a leading newline, then every line of `text` prefixed with `> `.
fn quote_block(text: &str) -> String {
    format!("\n> {}", text.replace('\n', "\n> "))
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
                if super::is_unknown_channel_error(&e) {
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

    fn sync_entry(
        entry_type: &str,
        task: &str,
        notes: Option<&str>,
        status: Option<&str>,
        blocker: Option<&str>,
    ) -> entries::SyncEntry {
        sync_entry_with_sow_ref(entry_type, task, notes, status, blocker, None)
    }

    fn sync_entry_with_sow_ref(
        entry_type: &str,
        task: &str,
        notes: Option<&str>,
        status: Option<&str>,
        blocker: Option<&str>,
        sow_ref: Option<&str>,
    ) -> entries::SyncEntry {
        entries::SyncEntry {
            id: 1,
            discord_user_id: "42".to_string(),
            entry_type: entry_type.to_string(),
            task: task.to_string(),
            notes: notes.map(str::to_string),
            status: status.map(str::to_string),
            progress: None,
            blocker: blocker.map(str::to_string),
            sow_ref: sow_ref.map(str::to_string),
        }
    }

    /// An `'update'` `SyncEntry` carrying a `/progress` writeup - the case
    /// the sync-message progress line depends on.
    fn sync_update(
        task: &str,
        status: &str,
        progress: &str,
        blocker: Option<&str>,
    ) -> entries::SyncEntry {
        entries::SyncEntry {
            progress: Some(progress.to_string()),
            ..sync_entry_with_sow_ref("update", task, None, Some(status), blocker, None)
        }
    }

    #[test]
    fn format_sync_message_for_todo_without_notes() {
        let entry = sync_entry("todo", "Write tests", None, None, None);
        assert_eq!(
            format_sync_message(&entry),
            "📋 <@42> added a todo: **Write tests**"
        );
    }

    #[test]
    fn format_sync_message_for_todo_with_notes() {
        let entry = sync_entry("todo", "Write tests", Some("keep it simple"), None, None);
        assert_eq!(
            format_sync_message(&entry),
            "📋 <@42> added a todo: **Write tests**\n> _keep it simple_"
        );
    }

    #[test]
    fn format_sync_message_for_todo_with_sow_ref() {
        let entry = sync_entry_with_sow_ref("todo", "Write tests", None, None, None, Some("M1D2"));
        assert_eq!(
            format_sync_message(&entry),
            "📋 <@42> added a todo: **Write tests** [M1D2]"
        );
    }

    #[test]
    fn format_sync_message_for_todo_with_notes_and_sow_ref() {
        let entry = sync_entry_with_sow_ref(
            "todo",
            "Write tests",
            Some("keep it simple"),
            None,
            None,
            Some("M1D2"),
        );
        assert_eq!(
            format_sync_message(&entry),
            "📋 <@42> added a todo: **Write tests** [M1D2]\n> _keep it simple_"
        );
    }

    #[test]
    fn format_sync_message_for_progress_ignores_sow_ref() {
        let entry = sync_entry_with_sow_ref(
            "update",
            "Write tests",
            None,
            Some("done"),
            None,
            Some("M1"),
        );
        assert_eq!(
            format_sync_message(&entry),
            "✅ <@42> progress on **Write tests**: Done"
        );
    }

    #[test]
    fn format_sync_message_for_progress_done() {
        let entry = sync_entry("update", "Write tests", None, Some("done"), None);
        assert_eq!(
            format_sync_message(&entry),
            "✅ <@42> progress on **Write tests**: Done"
        );
    }

    #[test]
    fn format_sync_message_for_progress_in_progress() {
        let entry = sync_entry("update", "Ship release", None, Some("in_progress"), None);
        assert_eq!(
            format_sync_message(&entry),
            "🔧 <@42> progress on **Ship release**: In Progress"
        );
    }

    #[test]
    fn format_sync_message_progress_includes_the_writeup() {
        let entry = sync_update(
            "Ship release",
            "in_progress",
            "cut the RC, smoke tests green",
            None,
        );
        assert_eq!(
            format_sync_message(&entry),
            "🔧 <@42> progress on **Ship release**: In Progress\n> cut the RC, smoke tests green"
        );
    }

    #[test]
    fn format_sync_message_blocked_shows_writeup_then_blocker() {
        let entry = sync_update(
            "Fix bug",
            "blocked",
            "traced it to the cache",
            Some("waiting on ops"),
        );
        assert_eq!(
            format_sync_message(&entry),
            "⚠️ <@42> progress on **Fix bug**: Blocked\n> traced it to the cache\n> blocked on: waiting on ops"
        );
    }

    #[test]
    fn format_sync_message_multiline_writeup_is_fully_quoted() {
        let entry = sync_update("Refactor", "done", "line one\nline two", None);
        assert_eq!(
            format_sync_message(&entry),
            "✅ <@42> progress on **Refactor**: Done\n> line one\n> line two"
        );
    }

    #[test]
    fn format_sync_message_blank_writeup_adds_no_quote_line() {
        let entry = sync_update("Refactor", "done", "   ", None);
        assert_eq!(
            format_sync_message(&entry),
            "✅ <@42> progress on **Refactor**: Done"
        );
    }

    #[test]
    fn format_sync_message_for_progress_blocked_with_blocker() {
        let entry = sync_entry(
            "update",
            "Fix bug",
            None,
            Some("blocked"),
            Some("waiting on ops"),
        );
        assert_eq!(
            format_sync_message(&entry),
            "⚠️ <@42> progress on **Fix bug**: Blocked\n> blocked on: waiting on ops"
        );
    }

    #[test]
    fn format_sync_message_for_progress_blocked_without_blocker() {
        let entry = sync_entry("update", "Fix bug", None, Some("blocked"), None);
        assert_eq!(
            format_sync_message(&entry),
            "⚠️ <@42> progress on **Fix bug**: Blocked"
        );
    }
}

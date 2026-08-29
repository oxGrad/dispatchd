use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// Whether a reminder of `kind` ("todo_reminder" | "update_reminder" |
/// "meeting_reminder") already fired for `date`.
pub fn already_sent(conn: &Connection, date: &str, kind: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM reminders_sent WHERE date = ?1 AND type = ?2",
            params![date, kind],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Records that a reminder of `kind` fired for `date`.
pub fn mark_sent(conn: &Connection, date: &str, kind: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO reminders_sent (date, type) VALUES (?1, ?2)",
        params![date, kind],
    )?;
    Ok(())
}

/// Records the thread created for `date`'s standup.
pub fn save_thread(conn: &Connection, date: &str, thread_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO daily_threads (date, thread_id) VALUES (?1, ?2)",
        params![date, thread_id],
    )?;
    Ok(())
}

/// The thread created for `date`'s standup, if any (`None` if the todo
/// reminder hasn't fired yet today - e.g. the bot was down at 9am).
pub fn thread_for(conn: &Connection, date: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT thread_id FROM daily_threads WHERE date = ?1",
            params![date],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    #[test]
    fn already_sent_is_false_until_marked() {
        let conn = open_test_db();
        assert!(!already_sent(&conn, "2026-08-29", "todo_reminder").unwrap());
        mark_sent(&conn, "2026-08-29", "todo_reminder").unwrap();
        assert!(already_sent(&conn, "2026-08-29", "todo_reminder").unwrap());
    }

    #[test]
    fn kinds_are_tracked_independently() {
        let conn = open_test_db();
        mark_sent(&conn, "2026-08-29", "todo_reminder").unwrap();
        assert!(already_sent(&conn, "2026-08-29", "todo_reminder").unwrap());
        assert!(!already_sent(&conn, "2026-08-29", "update_reminder").unwrap());
        assert!(!already_sent(&conn, "2026-08-29", "meeting_reminder").unwrap());
    }

    #[test]
    fn dates_are_tracked_independently() {
        let conn = open_test_db();
        mark_sent(&conn, "2026-08-29", "todo_reminder").unwrap();
        assert!(!already_sent(&conn, "2026-08-30", "todo_reminder").unwrap());
    }

    #[test]
    fn thread_round_trips() {
        let conn = open_test_db();
        assert_eq!(thread_for(&conn, "2026-08-29").unwrap(), None);
        save_thread(&conn, "2026-08-29", "111222333").unwrap();
        assert_eq!(
            thread_for(&conn, "2026-08-29").unwrap(),
            Some("111222333".to_string())
        );
    }
}

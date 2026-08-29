use anyhow::Result;
use chrono::Utc;
use chrono_tz::Tz;
use rusqlite::{Connection, params};

/// Today's date, in the given timezone, as `YYYY-MM-DD`.
pub fn today_in(tz: &Tz) -> String {
    Utc::now().with_timezone(tz).format("%Y-%m-%d").to_string()
}

/// Inserts a new `type = 'todo'` row. `todo_id`/`status`/`progress`/`blocker`
/// stay `NULL` - those are update-only columns. Returns the new row's id.
pub fn insert_todo(
    conn: &Connection,
    discord_user_id: &str,
    date: &str,
    task: &str,
    notes: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO entries (discord_user_id, date, type, task, notes, created_at)
         VALUES (?1, ?2, 'todo', ?3, ?4, ?5)",
        params![discord_user_id, date, task, notes, Utc::now().to_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir so it outlives the connection within the test -
        // fine for a short-lived test process.
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    type EntryRow = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    fn row(conn: &Connection, id: i64) -> EntryRow {
        conn.query_row(
            "SELECT discord_user_id, date, type, task, notes, todo_id, status, progress, blocker
             FROM entries WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .unwrap()
    }

    #[test]
    fn insert_todo_with_notes_sets_expected_columns() {
        let conn = open_test_db();
        let id = insert_todo(
            &conn,
            "42",
            "2026-08-29",
            "Write tests",
            Some("keep it simple"),
        )
        .unwrap();

        let (discord_user_id, date, entry_type, task, notes, todo_id, status, progress, blocker) =
            row(&conn, id);

        assert_eq!(discord_user_id, "42");
        assert_eq!(date, "2026-08-29");
        assert_eq!(entry_type, "todo");
        assert_eq!(task, "Write tests");
        assert_eq!(notes.as_deref(), Some("keep it simple"));
        assert_eq!(todo_id, None);
        assert_eq!(status, None);
        assert_eq!(progress, None);
        assert_eq!(blocker, None);

        let created_at: String = conn
            .query_row("SELECT created_at FROM entries WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(&created_at).is_ok());
    }

    #[test]
    fn insert_todo_without_notes_leaves_notes_null() {
        let conn = open_test_db();
        let id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();
        let (_, _, _, _, notes, ..) = row(&conn, id);
        assert_eq!(notes, None);
    }

    #[test]
    fn multiple_todos_same_user_and_date_are_distinct_rows() {
        let conn = open_test_db();
        let id1 = insert_todo(&conn, "42", "2026-08-29", "First task", None).unwrap();
        let id2 = insert_todo(&conn, "42", "2026-08-29", "Second task", None).unwrap();
        assert_ne!(id1, id2);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE discord_user_id = '42' AND date = '2026-08-29'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn today_in_returns_a_parseable_date() {
        let result = today_in(&Tz::UTC);
        assert!(NaiveDate::parse_from_str(&result, "%Y-%m-%d").is_ok());
    }
}

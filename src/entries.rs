use anyhow::Result;
use chrono::Utc;
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, params};

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

/// Inserts a new `type = 'update'` row. `notes` stays `NULL` - that's a
/// todo-only column. Returns the new row's id.
#[allow(clippy::too_many_arguments)]
pub fn insert_update(
    conn: &Connection,
    discord_user_id: &str,
    date: &str,
    task: &str,
    todo_id: Option<i64>,
    status: &str,
    progress: &str,
    blocker: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO entries (discord_user_id, date, type, task, todo_id, status, progress, blocker, created_at)
         VALUES (?1, ?2, 'update', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            discord_user_id,
            date,
            task,
            todo_id,
            status,
            progress,
            blocker,
            Utc::now().to_rfc3339()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetches a todo's task text, scoped to the owning user (defensive check -
/// a submitted modal's custom_id could in principle reference a todo id
/// that doesn't belong to whoever is submitting it).
pub fn todo_task(conn: &Connection, todo_id: i64, discord_user_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT task FROM entries WHERE id = ?1 AND type = 'todo' AND discord_user_id = ?2",
            params![todo_id, discord_user_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// This user's open todos for `date` - `type = 'todo'` rows with no
/// matching `type = 'update'` row yet (matched via `todo_id`) - optionally
/// filtered by a substring of `task` (the autocomplete partial input),
/// capped at 25 (Discord's autocomplete choice limit).
pub fn list_open_todos(
    conn: &Connection,
    discord_user_id: &str,
    date: &str,
    partial: &str,
) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, task FROM entries
         WHERE type = 'todo' AND discord_user_id = ?1 AND date = ?2
           AND task LIKE '%' || ?3 || '%'
           AND id NOT IN (SELECT todo_id FROM entries WHERE type = 'update' AND todo_id IS NOT NULL)
         ORDER BY id
         LIMIT 25",
    )?;
    let rows = stmt
        .query_map(params![discord_user_id, date, partial], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Like `list_open_todos`, but not filtered to "open" - includes todos
/// that already have an update against them. Used by `/todo edit`/`/todo
/// delete`'s autocomplete and `/todo list`, where the point is to target
/// *any* of today's todos, not just ones still awaiting an update.
pub fn list_todos(
    conn: &Connection,
    discord_user_id: &str,
    date: &str,
    partial: &str,
) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, task FROM entries
         WHERE type = 'todo' AND discord_user_id = ?1 AND date = ?2
           AND task LIKE '%' || ?3 || '%'
         ORDER BY id
         LIMIT 25",
    )?;
    let rows = stmt
        .query_map(params![discord_user_id, date, partial], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Current (task, notes) for a todo, scoped to owner+date+type='todo' -
/// used to pre-fill `/todo edit`'s modal.
pub fn todo_for_edit(
    conn: &Connection,
    todo_id: i64,
    discord_user_id: &str,
    date: &str,
) -> Result<Option<(String, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT task, notes FROM entries
             WHERE id = ?1 AND type = 'todo' AND discord_user_id = ?2 AND date = ?3",
            params![todo_id, discord_user_id, date],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// Updates a todo's task/notes in place, scoped to owner+date+type='todo'
/// (same defensive scoping as `todo_task`). Returns `false` if no matching
/// row was found (already deleted, wrong owner, or not from today).
pub fn update_todo(
    conn: &Connection,
    todo_id: i64,
    discord_user_id: &str,
    date: &str,
    task: &str,
    notes: Option<&str>,
) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE entries SET task = ?1, notes = ?2
         WHERE id = ?3 AND type = 'todo' AND discord_user_id = ?4 AND date = ?5",
        params![task, notes, todo_id, discord_user_id, date],
    )?;
    Ok(rows > 0)
}

/// The outcome of a `delete_todo` call.
#[derive(Debug, PartialEq, Eq)]
pub enum DeleteTodoOutcome {
    /// Deleted; carries the deleted todo's task text (for the
    /// confirmation reply).
    Deleted(String),
    /// No such todo today, or not owned by this user.
    NotFound,
    /// An `/update` already references this todo via `todo_id`.
    StillReferenced,
}

/// Deletes a todo, scoped to owner+date+type='todo'. `entries.todo_id`'s
/// `FOREIGN KEY` (with `PRAGMA foreign_keys = ON`, set in `db::open`)
/// means deleting a todo an update already references fails at the DB
/// level - caught here and turned into `StillReferenced` rather than a
/// raw error, so callers never have to inspect `rusqlite::Error`
/// internals.
pub fn delete_todo(
    conn: &Connection,
    todo_id: i64,
    discord_user_id: &str,
    date: &str,
) -> Result<DeleteTodoOutcome> {
    let task: Option<String> = conn
        .query_row(
            "SELECT task FROM entries
             WHERE id = ?1 AND type = 'todo' AND discord_user_id = ?2 AND date = ?3",
            params![todo_id, discord_user_id, date],
            |row| row.get(0),
        )
        .optional()?;
    let Some(task) = task else {
        return Ok(DeleteTodoOutcome::NotFound);
    };

    match conn.execute("DELETE FROM entries WHERE id = ?1", params![todo_id]) {
        Ok(_) => Ok(DeleteTodoOutcome::Deleted(task)),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Ok(DeleteTodoOutcome::StillReferenced)
        }
        Err(e) => Err(e.into()),
    }
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

    #[test]
    fn insert_update_with_todo_id_sets_expected_columns() {
        let conn = open_test_db();
        let todo_id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();
        let update_id = insert_update(
            &conn,
            "42",
            "2026-08-29",
            "Write tests",
            Some(todo_id),
            "done",
            "Finished all the tests",
            None,
        )
        .unwrap();

        let (
            discord_user_id,
            date,
            entry_type,
            task,
            notes,
            linked_todo_id,
            status,
            progress,
            blocker,
        ) = row(&conn, update_id);

        assert_eq!(discord_user_id, "42");
        assert_eq!(date, "2026-08-29");
        assert_eq!(entry_type, "update");
        assert_eq!(task, "Write tests");
        assert_eq!(notes, None);
        assert_eq!(linked_todo_id, Some(todo_id));
        assert_eq!(status.as_deref(), Some("done"));
        assert_eq!(progress.as_deref(), Some("Finished all the tests"));
        assert_eq!(blocker, None);
    }

    #[test]
    fn insert_update_without_todo_id_is_ad_hoc() {
        let conn = open_test_db();
        let update_id = insert_update(
            &conn,
            "42",
            "2026-08-29",
            "Unplanned firefighting",
            None,
            "blocked",
            "Prod was down",
            Some("waiting on ops"),
        )
        .unwrap();

        let (_, _, _, task, notes, todo_id, status, _, blocker) = row(&conn, update_id);
        assert_eq!(task, "Unplanned firefighting");
        assert_eq!(notes, None);
        assert_eq!(todo_id, None);
        assert_eq!(status.as_deref(), Some("blocked"));
        assert_eq!(blocker.as_deref(), Some("waiting on ops"));
    }

    #[test]
    fn todo_task_returns_text_for_owning_user_only() {
        let conn = open_test_db();
        let todo_id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();

        assert_eq!(
            todo_task(&conn, todo_id, "42").unwrap(),
            Some("Write tests".to_string())
        );
        assert_eq!(todo_task(&conn, todo_id, "99").unwrap(), None);
        assert_eq!(todo_task(&conn, 999_999, "42").unwrap(), None);
    }

    #[test]
    fn list_open_todos_excludes_already_updated_and_filters_by_substring() {
        let conn = open_test_db();
        let open_id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();
        let updated_id = insert_todo(&conn, "42", "2026-08-29", "Ship release", None).unwrap();
        insert_update(
            &conn,
            "42",
            "2026-08-29",
            "Ship release",
            Some(updated_id),
            "done",
            "Shipped",
            None,
        )
        .unwrap();

        let all_open = list_open_todos(&conn, "42", "2026-08-29", "").unwrap();
        assert_eq!(all_open, vec![(open_id, "Write tests".to_string())]);

        let filtered = list_open_todos(&conn, "42", "2026-08-29", "test").unwrap();
        assert_eq!(filtered, vec![(open_id, "Write tests".to_string())]);

        let no_match = list_open_todos(&conn, "42", "2026-08-29", "nonexistent").unwrap();
        assert!(no_match.is_empty());
    }

    #[test]
    fn list_todos_includes_already_updated_todos_unlike_list_open_todos() {
        let conn = open_test_db();
        let open_id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();
        let updated_id = insert_todo(&conn, "42", "2026-08-29", "Ship release", None).unwrap();
        insert_update(
            &conn,
            "42",
            "2026-08-29",
            "Ship release",
            Some(updated_id),
            "done",
            "Shipped",
            None,
        )
        .unwrap();

        let open_only = list_open_todos(&conn, "42", "2026-08-29", "").unwrap();
        assert_eq!(open_only, vec![(open_id, "Write tests".to_string())]);

        let mut all = list_todos(&conn, "42", "2026-08-29", "").unwrap();
        all.sort();
        let mut expected = vec![
            (open_id, "Write tests".to_string()),
            (updated_id, "Ship release".to_string()),
        ];
        expected.sort();
        assert_eq!(all, expected);
    }

    #[test]
    fn todo_for_edit_returns_current_task_and_notes_scoped_to_owner_and_date() {
        let conn = open_test_db();
        let id = insert_todo(
            &conn,
            "42",
            "2026-08-29",
            "Write tests",
            Some("keep it simple"),
        )
        .unwrap();

        assert_eq!(
            todo_for_edit(&conn, id, "42", "2026-08-29").unwrap(),
            Some((
                "Write tests".to_string(),
                Some("keep it simple".to_string())
            ))
        );
        assert_eq!(todo_for_edit(&conn, id, "99", "2026-08-29").unwrap(), None);
        assert_eq!(todo_for_edit(&conn, id, "42", "2026-08-30").unwrap(), None);
        assert_eq!(
            todo_for_edit(&conn, 999_999, "42", "2026-08-29").unwrap(),
            None
        );
    }

    #[test]
    fn update_todo_changes_task_and_notes() {
        let conn = open_test_db();
        let id = insert_todo(&conn, "42", "2026-08-29", "Write tests", Some("old notes")).unwrap();

        let changed = update_todo(
            &conn,
            id,
            "42",
            "2026-08-29",
            "Write better tests",
            Some("new notes"),
        )
        .unwrap();
        assert!(changed);

        let (_, _, _, task, notes, ..) = row(&conn, id);
        assert_eq!(task, "Write better tests");
        assert_eq!(notes.as_deref(), Some("new notes"));

        let cleared =
            update_todo(&conn, id, "42", "2026-08-29", "Write better tests", None).unwrap();
        assert!(cleared);
        let (_, _, _, _, notes, ..) = row(&conn, id);
        assert_eq!(notes, None);
    }

    #[test]
    fn update_todo_returns_false_when_no_matching_row() {
        let conn = open_test_db();
        let id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();

        assert!(!update_todo(&conn, id, "99", "2026-08-29", "x", None).unwrap());
        assert!(!update_todo(&conn, id, "42", "2026-08-30", "x", None).unwrap());
        assert!(!update_todo(&conn, 999_999, "42", "2026-08-29", "x", None).unwrap());
    }

    #[test]
    fn delete_todo_removes_the_row_and_returns_its_task_text() {
        let conn = open_test_db();
        let id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();

        let outcome = delete_todo(&conn, id, "42", "2026-08-29").unwrap();
        assert_eq!(
            outcome,
            DeleteTodoOutcome::Deleted("Write tests".to_string())
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_todo_returns_not_found_for_wrong_owner_wrong_date_or_unknown_id() {
        let conn = open_test_db();
        let id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();

        assert_eq!(
            delete_todo(&conn, id, "99", "2026-08-29").unwrap(),
            DeleteTodoOutcome::NotFound
        );
        assert_eq!(
            delete_todo(&conn, id, "42", "2026-08-30").unwrap(),
            DeleteTodoOutcome::NotFound
        );
        assert_eq!(
            delete_todo(&conn, 999_999, "42", "2026-08-29").unwrap(),
            DeleteTodoOutcome::NotFound
        );
    }

    #[test]
    fn delete_todo_blocks_when_an_update_references_it() {
        let conn = open_test_db();
        let id = insert_todo(&conn, "42", "2026-08-29", "Write tests", None).unwrap();
        insert_update(
            &conn,
            "42",
            "2026-08-29",
            "Write tests",
            Some(id),
            "done",
            "Finished",
            None,
        )
        .unwrap();

        let outcome = delete_todo(&conn, id, "42", "2026-08-29").unwrap();
        assert_eq!(outcome, DeleteTodoOutcome::StillReferenced);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}

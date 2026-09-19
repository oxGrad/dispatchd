use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// Whether a follow-up of `kind` ("todo_followup" | "update_followup")
/// already fired for `discord_user_id` on `date`.
pub fn already_sent(
    conn: &Connection,
    date: &str,
    discord_user_id: &str,
    kind: &str,
) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM followups_sent WHERE date = ?1 AND discord_user_id = ?2 AND type = ?3",
            params![date, discord_user_id, kind],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Records that a follow-up of `kind` fired for `discord_user_id` on `date`.
pub fn mark_sent(conn: &Connection, date: &str, discord_user_id: &str, kind: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO followups_sent (date, discord_user_id, type) VALUES (?1, ?2, ?3)",
        params![date, discord_user_id, kind],
    )?;
    Ok(())
}

/// Members with no `type = 'todo'` row for `date` at all. Excludes role
/// `viewer` - view-only members aren't expected to submit one, so they're
/// never nagged for it.
pub fn members_missing_todo(conn: &Connection, date: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT discord_user_id FROM members
         WHERE role != 'viewer'
           AND discord_user_id NOT IN (
             SELECT discord_user_id FROM entries WHERE type = 'todo' AND date = ?1
         )",
    )?;
    let ids = stmt
        .query_map(params![date], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Members who posted at least one todo for `date` but have at least one
/// that still has no matching update. Someone with zero todos isn't
/// included here - there's nothing to update against, so they're only
/// nagged by the todo follow-up.
pub fn members_missing_update(conn: &Connection, date: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT discord_user_id FROM entries
         WHERE type = 'todo' AND date = ?1
           AND id NOT IN (SELECT todo_id FROM entries WHERE type = 'update' AND todo_id IS NOT NULL)",
    )?;
    let ids = stmt
        .query_map(params![date], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Members with no `entries` row of any type (`todo` or `update`) for
/// `date` at all - i.e. submitted nothing whatsoever today. Distinct from
/// `members_missing_todo`/`members_missing_update` above, which each
/// track one submission kind independently; this is the "no activity at
/// all" set the day summary's missing-submissions message nags. Excludes
/// role `viewer`, same as `members_missing_todo`.
pub fn members_with_no_activity(conn: &Connection, date: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT discord_user_id FROM members
         WHERE role != 'viewer'
           AND discord_user_id NOT IN (
             SELECT discord_user_id FROM entries WHERE date = ?1
         )",
    )?;
    let ids = stmt
        .query_map(params![date], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries;

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    fn seed_member(conn: &Connection, id: &str, name: &str) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES (?1, ?2, 'senior', 0)",
            params![id, name],
        )
        .unwrap();
    }

    fn seed_viewer(conn: &Connection, id: &str, name: &str) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES (?1, ?2, 'viewer', 1)",
            params![id, name],
        )
        .unwrap();
    }

    const DATE: &str = "2026-08-29";

    #[test]
    fn already_sent_is_false_until_marked() {
        let conn = open_test_db();
        assert!(!already_sent(&conn, DATE, "1", "todo_followup").unwrap());
        mark_sent(&conn, DATE, "1", "todo_followup").unwrap();
        assert!(already_sent(&conn, DATE, "1", "todo_followup").unwrap());
    }

    #[test]
    fn kinds_and_users_are_tracked_independently() {
        let conn = open_test_db();
        mark_sent(&conn, DATE, "1", "todo_followup").unwrap();
        assert!(!already_sent(&conn, DATE, "1", "update_followup").unwrap());
        assert!(!already_sent(&conn, DATE, "2", "todo_followup").unwrap());
    }

    #[test]
    fn members_missing_todo_excludes_those_who_posted() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        seed_member(&conn, "2", "Budi");
        entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();

        let missing = members_missing_todo(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["2".to_string()]);
    }

    #[test]
    fn members_missing_todo_excludes_viewers() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        seed_viewer(&conn, "2", "Watcher");

        let missing = members_missing_todo(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["1".to_string()]);
    }

    #[test]
    fn members_missing_update_only_includes_partial_matches() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // no todo at all
        seed_member(&conn, "2", "Budi"); // todo, no update
        seed_member(&conn, "3", "Citra"); // todo, fully matched

        entries::insert_todo(&conn, "2", DATE, "a", None, None).unwrap();

        let todo3 = entries::insert_todo(&conn, "3", DATE, "a", None, None).unwrap();
        entries::insert_update(&conn, "3", DATE, "a", Some(todo3), "done", "done", None).unwrap();

        let missing = members_missing_update(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["2".to_string()]);
    }

    #[test]
    fn members_with_no_activity_excludes_anyone_with_a_todo_or_an_update() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // todo only
        seed_member(&conn, "2", "Budi"); // ad-hoc update only, no todo
        seed_member(&conn, "3", "Citra"); // nothing at all

        entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();
        entries::insert_update(&conn, "2", DATE, "hotfix", None, "done", "shipped", None).unwrap();

        let missing = members_with_no_activity(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["3".to_string()]);
    }

    #[test]
    fn members_with_no_activity_excludes_viewers() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        seed_viewer(&conn, "2", "Watcher");

        let missing = members_with_no_activity(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["1".to_string()]);
    }

    #[test]
    fn members_with_no_activity_is_empty_when_everyone_submitted_something() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();

        let missing = members_with_no_activity(&conn, DATE).unwrap();
        assert!(missing.is_empty());
    }
}

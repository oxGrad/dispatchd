use anyhow::Result;
use rusqlite::{Connection, params};

pub struct MemberStatus {
    pub name: String,
    pub todo_count: i64,
    pub matched_update_count: i64,
}

/// One row per `members` entry for `date`. Simple per-member COUNT queries
/// rather than one complex JOIN - fine for a 6-person team, easier to
/// read and verify.
pub fn team_status(conn: &Connection, date: &str) -> Result<Vec<MemberStatus>> {
    let mut stmt = conn.prepare("SELECT discord_user_id, name FROM members ORDER BY name")?;
    let members = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut result = Vec::with_capacity(members.len());
    for (discord_user_id, name) in members {
        let todo_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM entries WHERE type = 'todo' AND date = ?1 AND discord_user_id = ?2",
            params![date, discord_user_id],
            |row| row.get(0),
        )?;
        let matched_update_count: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT todo_id) FROM entries
             WHERE type = 'update' AND date = ?1 AND discord_user_id = ?2 AND todo_id IS NOT NULL",
            params![date, discord_user_id],
            |row| row.get(0),
        )?;
        result.push(MemberStatus {
            name,
            todo_count,
            matched_update_count,
        });
    }
    Ok(result)
}

/// Formats one `/team-status` line, e.g. `✅ Alice — 3/3 updated`.
/// A member with no todos posted shows no fraction (`0/0` reads as noise);
/// one who posted todos but matched none of them is treated the same as
/// "no todo posted" - both are the "needs attention" case.
pub fn format_status_line(status: &MemberStatus) -> String {
    if status.todo_count == 0 {
        return format!("❌ {} — no todo posted", status.name);
    }
    let emoji = if status.matched_update_count == status.todo_count {
        "✅"
    } else if status.matched_update_count == 0 {
        "❌"
    } else {
        "⚠️"
    };
    format!(
        "{emoji} {} — {}/{} updated",
        status.name, status.matched_update_count, status.todo_count
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries;
    use crate::members;

    fn seed_member(conn: &Connection, id: &str, name: &str, role: &str) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, role, role == "lead"],
        )
        .unwrap();
    }

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    const DATE: &str = "2026-08-29";

    #[test]
    fn fully_matched_member_shows_green_check() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        for task in ["a", "b", "c"] {
            let todo_id = entries::insert_todo(&conn, "1", DATE, task, None).unwrap();
            entries::insert_update(&conn, "1", DATE, task, Some(todo_id), "done", "done", None)
                .unwrap();
        }

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(format_status_line(&statuses[0]), "✅ Alice — 3/3 updated");
    }

    #[test]
    fn partially_matched_member_shows_warning() {
        let conn = open_test_db();
        seed_member(&conn, "2", "Budi", "designer");
        let todo1 = entries::insert_todo(&conn, "2", DATE, "a", None).unwrap();
        entries::insert_todo(&conn, "2", DATE, "b", None).unwrap();
        entries::insert_update(&conn, "2", DATE, "a", Some(todo1), "done", "done", None).unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(format_status_line(&statuses[0]), "⚠️ Budi — 1/2 updated");
    }

    #[test]
    fn member_with_no_todos_shows_no_fraction() {
        let conn = open_test_db();
        seed_member(&conn, "3", "Citra", "senior");

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(
            format_status_line(&statuses[0]),
            "❌ Citra — no todo posted"
        );
    }

    #[test]
    fn member_with_todos_but_zero_matches_shows_red() {
        let conn = open_test_db();
        seed_member(&conn, "4", "Dedi", "medior");
        entries::insert_todo(&conn, "4", DATE, "a", None).unwrap();
        entries::insert_todo(&conn, "4", DATE, "b", None).unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(format_status_line(&statuses[0]), "❌ Dedi — 0/2 updated");
    }

    #[test]
    fn ad_hoc_update_does_not_count_toward_matched() {
        let conn = open_test_db();
        seed_member(&conn, "5", "Eka", "junior");
        entries::insert_todo(&conn, "5", DATE, "a", None).unwrap();
        entries::insert_update(
            &conn,
            "5",
            DATE,
            "unplanned work",
            None,
            "done",
            "done",
            None,
        )
        .unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(statuses[0].todo_count, 1);
        assert_eq!(statuses[0].matched_update_count, 0);
    }

    #[test]
    fn two_updates_against_the_same_todo_still_count_as_one_match() {
        let conn = open_test_db();
        seed_member(&conn, "6", "Fajar", "senior");
        let todo_id = entries::insert_todo(&conn, "6", DATE, "a", None).unwrap();
        entries::insert_update(
            &conn,
            "6",
            DATE,
            "a",
            Some(todo_id),
            "in_progress",
            "started",
            None,
        )
        .unwrap();
        entries::insert_update(
            &conn,
            "6",
            DATE,
            "a",
            Some(todo_id),
            "done",
            "finished",
            None,
        )
        .unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(statuses[0].matched_update_count, 1);
    }

    #[test]
    fn is_lead_check_still_works_alongside_status() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        seed_member(&conn, "2", "Budi", "designer");
        assert!(members::is_lead(&conn, "1").unwrap());
        assert!(!members::is_lead(&conn, "2").unwrap());
        assert!(!members::is_lead(&conn, "999").unwrap());
    }
}

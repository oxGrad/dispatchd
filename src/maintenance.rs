use anyhow::Result;
use rusqlite::Connection;

/// Suggested default from the handover doc ("not a firm requirement -
/// adjust freely").
const RETENTION_DAYS: u32 = 90;

/// Prunes `reminders_sent`/`followups_sent` rows older than
/// `RETENTION_DAYS` and reclaims the freed space with `VACUUM`.
/// `entries` is never touched - it's the retained history the biweekly
/// recap depends on. Returns (reminders_sent rows deleted, followups_sent
/// rows deleted).
pub fn run(conn: &Connection) -> Result<(usize, usize)> {
    let cutoff = format!("-{RETENTION_DAYS} days");

    let reminders_deleted = conn.execute(
        "DELETE FROM reminders_sent WHERE date < date('now', ?1)",
        [&cutoff],
    )?;
    let followups_deleted = conn.execute(
        "DELETE FROM followups_sent WHERE date < date('now', ?1)",
        [&cutoff],
    )?;

    // VACUUM can't run inside a transaction; each `execute` above commits
    // in autocommit mode by default, so this is safe to run right after.
    conn.execute("VACUUM", [])?;

    Ok((reminders_deleted, followups_deleted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    fn seed_reminder(conn: &Connection, date: &str) {
        conn.execute(
            "INSERT INTO reminders_sent (date, type) VALUES (?1, 'todo_reminder')",
            [date],
        )
        .unwrap();
    }

    fn seed_followup(conn: &Connection, date: &str, discord_user_id: &str) {
        conn.execute(
            "INSERT INTO followups_sent (date, discord_user_id, type) VALUES (?1, ?2, 'todo_followup')",
            params![date, discord_user_id],
        )
        .unwrap();
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn prunes_rows_older_than_retention_window() {
        let conn = open_test_db();
        seed_reminder(&conn, "2000-01-01"); // ancient - pruned
        seed_reminder(&conn, "2000-01-02"); // ancient - pruned
        seed_followup(&conn, "2000-01-01", "1"); // ancient - pruned

        let (reminders_deleted, followups_deleted) = run(&conn).unwrap();

        assert_eq!(reminders_deleted, 2);
        assert_eq!(followups_deleted, 1);
        assert_eq!(count(&conn, "reminders_sent"), 0);
        assert_eq!(count(&conn, "followups_sent"), 0);
    }

    #[test]
    fn keeps_rows_within_retention_window() {
        let conn = open_test_db();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        seed_reminder(&conn, &today);
        seed_followup(&conn, &today, "1");

        let (reminders_deleted, followups_deleted) = run(&conn).unwrap();

        assert_eq!(reminders_deleted, 0);
        assert_eq!(followups_deleted, 0);
        assert_eq!(count(&conn, "reminders_sent"), 1);
        assert_eq!(count(&conn, "followups_sent"), 1);
    }

    #[test]
    fn never_touches_entries() {
        let conn = open_test_db();
        crate::entries::insert_todo(&conn, "1", "2000-01-01", "ancient task", None, None).unwrap();
        seed_reminder(&conn, "2000-01-01");

        run(&conn).unwrap();

        assert_eq!(count(&conn, "entries"), 1);
    }
}

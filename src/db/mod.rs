use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!("migrations/0001_initial.sql"))])
}

/// Opens (creating if needed) the SQLite DB at `path`, sets the pragmas the
/// handover doc calls for, and migrates the schema to the latest version.
/// Safe to call on every startup - migrations are idempotent, and the
/// pragmas below must be re-applied per-connection anyway.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut conn = Connection::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;

    // PRAGMA foreign_keys is per-connection, not persisted by SQLite.
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
        .context("failed to set database pragmas")?;

    migrations()
        .to_latest(&mut conn)
        .context("failed to run database migrations")?;

    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        // `entries.id ... AUTOINCREMENT` makes SQLite create its own
        // internal `sqlite_sequence` bookkeeping table - not part of our
        // schema, so it's excluded here rather than asserted on.
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
                 ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn fresh_open_creates_all_tables() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("dispatchd.sqlite3")).unwrap();
        assert_eq!(
            table_names(&conn),
            vec!["entries", "followups_sent", "members", "reminders_sent"]
        );
    }

    #[test]
    fn pragmas_are_set() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("dispatchd.sqlite3")).unwrap();

        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
    }

    #[test]
    fn reopening_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatchd.sqlite3");

        let conn1 = open(&path).unwrap();
        drop(conn1);
        let conn2 = open(&path).unwrap();

        assert_eq!(
            table_names(&conn2),
            vec!["entries", "followups_sent", "members", "reminders_sent"]
        );
    }
}

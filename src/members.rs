use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;

use crate::config::xdg_dirs;

pub(crate) const MEMBERS_PATH_OVERRIDE_ENV: &str = "DISPATCHD_MEMBERS_PATH";
const VALID_ROLES: &[&str] = &["admin", "lead", "designer", "senior", "medior", "junior"];

#[derive(Debug, Default, Deserialize)]
struct MembersFile {
    #[serde(default)]
    members: Vec<MemberSeed>,
}

#[derive(Debug, Deserialize)]
struct MemberSeed {
    discord_user_id: String,
    name: String,
    role: String,
}

/// Resolves the roster file to read, without reading it. `None` means
/// there's nothing to seed from - not an error, same "missing is valid"
/// convention as `config.toml`.
pub fn resolve_path() -> Option<PathBuf> {
    if let Ok(path) = env::var(MEMBERS_PATH_OVERRIDE_ENV) {
        return Some(PathBuf::from(path));
    }
    xdg_dirs().find_config_file("members.toml")
}

/// Resolves the roster file's *target* path for `dispatchd init` - where a
/// new file should be created, as opposed to `resolve_path` which only
/// locates one that already exists.
pub fn target_path() -> Result<PathBuf> {
    if let Ok(path) = env::var(MEMBERS_PATH_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        return Ok(path);
    }
    xdg_dirs()
        .place_config_file("members.toml")
        .context("failed to resolve members.toml target path (is $HOME set?)")
}

/// Reads `members.toml` (if present) and upserts each row into `members`.
/// Returns the number of rows upserted (0 if no file was found).
pub fn seed(conn: &Connection) -> Result<usize> {
    let Some(path) = resolve_path() else {
        return Ok(0);
    };

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read members file {}", path.display()))?;
    let file: MembersFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse members file {}", path.display()))?;

    for member in &file.members {
        if !VALID_ROLES.contains(&member.role.as_str()) {
            bail!(
                "invalid role {:?} for member {:?} ({}): must be one of {:?}",
                member.role,
                member.name,
                member.discord_user_id,
                VALID_ROLES
            );
        }
    }

    for member in &file.members {
        let is_lead = matches!(member.role.as_str(), "lead" | "admin");
        let is_admin = member.role == "admin";
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead, is_admin)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(discord_user_id) DO UPDATE SET
                 name = excluded.name,
                 role = excluded.role,
                 is_lead = excluded.is_lead,
                 is_admin = excluded.is_admin",
            rusqlite::params![
                member.discord_user_id,
                member.name,
                member.role,
                is_lead,
                is_admin
            ],
        )
        .with_context(|| format!("failed to upsert member {:?}", member.discord_user_id))?;
    }

    Ok(file.members.len())
}

/// `true` when the member has tech-lead privileges - role `lead` or
/// `admin`. `false` for an unknown `discord_user_id`, not an error - the
/// bot-side source-of-truth check for `/team`.
pub fn is_lead(conn: &Connection, discord_user_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT is_lead FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// `true` when the member has bot-operator privileges (role `admin`).
/// `false` for an unknown `discord_user_id`, not an error - the bot-side
/// source-of-truth check for `/admin`.
// The `/admin` command group that consumes this lands in a later change;
// until then only the unit tests exercise it.
#[allow(dead_code)]
pub fn is_admin(conn: &Connection, discord_user_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT is_admin FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// Every team member's Discord user id - used to build the `<@id> ...`
/// mention text for the 9am standup ping. Order doesn't matter.
pub fn all_member_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT discord_user_id FROM members")?;
    let ids = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Every member as `(discord_user_id, name)`, ordered by name. Backs the
/// `/team remind` autocomplete.
pub fn roster(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT discord_user_id, name FROM members ORDER BY name")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A member's display name, or `None` if `discord_user_id` isn't on the roster.
pub fn name_of(conn: &Connection, discord_user_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT name FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use std::io::Write;

    fn write_members(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("members.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        (dir, path)
    }

    fn seed_from(conn: &Connection, contents: &str) -> Result<usize> {
        let _guard = ENV_LOCK.lock().unwrap();
        let (_dir, path) = write_members(contents);
        // SAFETY: held under ENV_LOCK; removed before returning.
        unsafe {
            env::set_var(MEMBERS_PATH_OVERRIDE_ENV, &path);
        }
        let result = seed(conn);
        unsafe {
            env::remove_var(MEMBERS_PATH_OVERRIDE_ENV);
        }
        result
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM members", [], |row| row.get(0))
            .unwrap()
    }

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    fn seed_member(conn: &Connection, id: &str, name: &str) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES (?1, ?2, 'senior', 0)",
            rusqlite::params![id, name],
        )
        .unwrap();
    }

    #[test]
    fn no_file_present_seeds_nothing() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: held under ENV_LOCK; both vars removed before returning.
        // No MEMBERS_PATH_OVERRIDE_ENV set (an override pointing at a
        // missing path is a real error, same as config.toml - this test is
        // for the "nothing configured at all" case), and XDG_CONFIG_HOME
        // pinned to an empty temp dir so `find_config_file` can't pick up
        // a real members.toml from the host running the tests.
        let empty_xdg = tempfile::tempdir().unwrap();
        unsafe {
            env::remove_var(MEMBERS_PATH_OVERRIDE_ENV);
            env::set_var("XDG_CONFIG_HOME", empty_xdg.path());
        }
        let conn = crate::db::open(&tempfile::tempdir().unwrap().path().join("d.sqlite3")).unwrap();
        let count = seed(&conn).unwrap();
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
        assert_eq!(count, 0);
        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn valid_file_seeds_all_members_with_correct_is_lead() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();

        let count = seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "lead"

            [[members]]
            discord_user_id = "2"
            name = "Budi"
            role = "designer"
            "#,
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(row_count(&conn), 2);

        let alice_is_lead: bool = conn
            .query_row(
                "SELECT is_lead FROM members WHERE discord_user_id = '1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(alice_is_lead);

        let budi_is_lead: bool = conn
            .query_row(
                "SELECT is_lead FROM members WHERE discord_user_id = '2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!budi_is_lead);
    }

    #[test]
    fn reseeding_updates_existing_rows_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();

        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "senior"
            "#,
        )
        .unwrap();

        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice Renamed"
            role = "lead"
            "#,
        )
        .unwrap();

        assert_eq!(row_count(&conn), 1);
        let (name, role): (String, String) = conn
            .query_row(
                "SELECT name, role FROM members WHERE discord_user_id = '1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Alice Renamed");
        assert_eq!(role, "lead");
    }

    #[test]
    fn invalid_role_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();

        let err = seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "intern"
            "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("intern"), "{err}");
        assert!(err.to_string().contains("Alice"), "{err}");
        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn all_member_ids_returns_every_seeded_member() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();

        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "lead"

            [[members]]
            discord_user_id = "2"
            name = "Budi"
            role = "designer"
            "#,
        )
        .unwrap();

        let mut ids = all_member_ids(&conn).unwrap();
        ids.sort();
        assert_eq!(ids, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn roster_returns_every_member_ordered_by_name() {
        let conn = open_test_db();
        seed_member(&conn, "2", "Budi");
        seed_member(&conn, "1", "Alice");
        seed_member(&conn, "3", "Citra");

        let roster = roster(&conn).unwrap();
        assert_eq!(
            roster,
            vec![
                ("1".to_string(), "Alice".to_string()),
                ("2".to_string(), "Budi".to_string()),
                ("3".to_string(), "Citra".to_string()),
            ]
        );
    }

    #[test]
    fn admin_role_seeds_both_is_admin_and_is_lead() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Ops"
            role = "admin"
            "#,
        )
        .unwrap();

        let (is_lead, is_admin): (bool, bool) = conn
            .query_row(
                "SELECT is_lead, is_admin FROM members WHERE discord_user_id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(is_lead, "admin must inherit lead privileges");
        assert!(is_admin);
    }

    #[test]
    fn is_admin_is_true_only_for_admins() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Ops"
            role = "admin"

            [[members]]
            discord_user_id = "2"
            name = "Lead"
            role = "lead"
            "#,
        )
        .unwrap();

        assert!(is_admin(&conn, "1").unwrap());
        assert!(!is_admin(&conn, "2").unwrap());
        assert!(!is_admin(&conn, "999").unwrap());
    }

    #[test]
    fn reseeding_admin_to_senior_clears_both_flags() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            "[[members]]\ndiscord_user_id = \"1\"\nname = \"X\"\nrole = \"admin\"\n",
        )
        .unwrap();
        seed_from(
            &conn,
            "[[members]]\ndiscord_user_id = \"1\"\nname = \"X\"\nrole = \"senior\"\n",
        )
        .unwrap();
        let (is_lead, is_admin): (bool, bool) = conn
            .query_row(
                "SELECT is_lead, is_admin FROM members WHERE discord_user_id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(!is_lead);
        assert!(!is_admin);
    }

    #[test]
    fn name_of_finds_a_seeded_member_and_misses_an_unknown_id() {
        let conn = open_test_db();
        seed_member(&conn, "7", "Gita");

        assert_eq!(name_of(&conn, "7").unwrap().as_deref(), Some("Gita"));
        assert_eq!(name_of(&conn, "999").unwrap(), None);
    }
}

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;

use crate::config::xdg_dirs;

pub(crate) const MEMBERS_PATH_OVERRIDE_ENV: &str = "DISPATCHD_MEMBERS_PATH";
const VALID_ROLES: &[&str] = &["lead", "designer", "senior", "medior", "junior", "viewer"];

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
    /// Bot-operator privileges (`/admin`), independent of `role` - not a
    /// superset/subset relationship with `is_lead`. A member needs
    /// `role = "lead"` (or `"viewer"`) separately if they should also pass
    /// `/team`'s `is_lead` gate.
    #[serde(default)]
    is_admin: bool,
    /// The member's GitHub username, for the GitHub integration. Optional -
    /// a member without one (e.g. a designer) simply has no GitHub
    /// activity attributed to them. Validated (`is_valid_github_user`) and
    /// checked for duplicates here; seeded into `members.github_user`, which
    /// the GitHub sync resolves to a numeric `github_id` (kept across
    /// reseeds while `github_user` is unchanged).
    #[serde(default)]
    github_user: Option<String>,
}

/// What `dispatchd validate` reports about the roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RosterSummary {
    pub members: usize,
    /// How many members have a `github_user` set.
    pub with_github: usize,
}

/// GitHub's username rules: 1-39 characters, ASCII letters, digits and
/// hyphens, not starting with a hyphen. (GitHub also forbids trailing and
/// doubled hyphens for new accounts, but some older accounts have them, so
/// those aren't rejected.)
fn is_valid_github_user(name: &str) -> bool {
    (1..=39).contains(&name.len())
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
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

/// Reads and parses `members.toml` (if present) and checks every `role`
/// against `VALID_ROLES`. No DB access - shared by `seed` (which then
/// writes the parsed rows) and `validate` (which just wants the same
/// checks without a DB connection).
fn load_and_validate() -> Result<Option<MembersFile>> {
    let Some(path) = resolve_path() else {
        return Ok(None);
    };

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read members file {}", path.display()))?;
    let file: MembersFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse members file {}", path.display()))?;

    let mut github_users = std::collections::HashMap::new();
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
        if let Some(user) = &member.github_user {
            if !is_valid_github_user(user) {
                let hint = if user.starts_with('@') {
                    " - drop the leading @"
                } else {
                    ""
                };
                bail!(
                    "invalid github_user {:?} for member {:?} ({}): expected a GitHub \
                     username{hint}",
                    user,
                    member.name,
                    member.discord_user_id
                );
            }
            // GitHub usernames are case-insensitive, so `Budi` and `budi`
            // are the same account.
            if let Some(other) = github_users.insert(user.to_ascii_lowercase(), &member.name) {
                bail!(
                    "github_user {:?} is set on both {:?} and {:?} - each GitHub account \
                     can belong to only one member",
                    user,
                    other,
                    member.name
                );
            }
        }
    }

    Ok(Some(file))
}

/// Parses and validates `members.toml` without touching the DB - the
/// `dispatchd validate` check. Returns how many members would be seeded
/// and how many of them have a `github_user` (all 0 if no file was found).
pub fn validate() -> Result<RosterSummary> {
    Ok(
        load_and_validate()?.map_or_else(RosterSummary::default, |file| RosterSummary {
            members: file.members.len(),
            with_github: file
                .members
                .iter()
                .filter(|m| m.github_user.is_some())
                .count(),
        }),
    )
}

/// Replaces the `members` table with `members.toml`'s roster, atomically.
/// The file is the source of truth: a member removed from it (or the whole
/// file removed) loses their row - and with it `is_lead`/`is_admin` - on
/// the next start, rather than keeping stale privileges. Nothing references
/// `members` by foreign key, and history queries `LEFT JOIN` it, so past
/// entries survive (shown by raw ID). Returns the number of rows written.
pub fn seed(conn: &Connection) -> Result<usize> {
    let members = load_and_validate()?.map_or_else(Vec::new, |file| file.members);

    let tx = conn.unchecked_transaction()?;
    // A resolved GitHub id survives the reseed as long as the member's
    // github_user is unchanged (GitHub usernames are case-insensitive):
    // re-resolving by username on every start would lose a member who has
    // since renamed their account, or credit them with whoever claimed the
    // old name.
    let previous_ids: std::collections::HashMap<String, (String, i64)> = tx
        .prepare(
            "SELECT discord_user_id, github_user, github_id FROM members
             WHERE github_user IS NOT NULL AND github_id IS NOT NULL",
        )?
        .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute("DELETE FROM members", [])?;
    for member in &members {
        let is_lead = matches!(member.role.as_str(), "lead" | "viewer");
        let github_id = member.github_user.as_ref().and_then(|user| {
            previous_ids
                .get(&member.discord_user_id)
                .filter(|(previous, _)| previous.eq_ignore_ascii_case(user))
                .map(|(_, id)| *id)
        });
        tx.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead, is_admin, github_user,
                 github_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                member.discord_user_id,
                member.name,
                member.role,
                is_lead,
                member.is_admin,
                member.github_user,
                github_id
            ],
        )
        .with_context(|| format!("failed to insert member {:?}", member.discord_user_id))?;
    }
    tx.commit()?;

    Ok(members.len())
}

/// `true` when `discord_user_id` is on the roster at all - the gate for
/// `/todo` and `/progress`, so only team members can write entries.
pub fn is_member(conn: &Connection, discord_user_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// `true` when the member has tech-lead privileges - role `lead` or
/// `viewer` (view-only: sees `/team status`/`report` but isn't nagged to
/// submit `/todo`/`/progress`, see `all_member_ids`). `false` for an
/// unknown `discord_user_id`, not an error - the bot-side source-of-truth
/// check for `/team`.
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

/// `true` only for role `lead` - unlike `is_lead`, excludes `viewer`.
/// Gates `/team remind` and `/team skip-meeting`: a view-only member can
/// see the team's progress but shouldn't be nudging members or cancelling
/// the meeting on the tech lead's behalf.
pub fn is_active_lead(conn: &Connection, discord_user_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT role = 'lead' FROM members WHERE discord_user_id = ?1",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// `true` when the member has bot-operator privileges (`is_admin = true`
/// in `members.toml`, set independently of `role`). `false` for an
/// unknown `discord_user_id`, not an error - the bot-side source-of-truth
/// check for `/admin`.
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

/// Every *participating* team member's Discord user id - used to build the
/// `<@id> ...` mention text for the 9am standup / pre-meeting pings.
/// Excludes role `viewer` (view-only members aren't expected to submit
/// anything, so they're not pinged to). Order doesn't matter.
pub fn all_member_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT discord_user_id FROM members WHERE role != 'viewer'")?;
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

    fn validate_from(contents: &str) -> Result<RosterSummary> {
        let _guard = ENV_LOCK.lock().unwrap();
        let (_dir, path) = write_members(contents);
        // SAFETY: held under ENV_LOCK; removed before returning.
        unsafe {
            env::set_var(MEMBERS_PATH_OVERRIDE_ENV, &path);
        }
        let result = validate();
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
    fn reseed_removes_members_dropped_from_the_file() {
        let conn = open_test_db();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "lead"
            is_admin = true

            [[members]]
            discord_user_id = "2"
            name = "Budi"
            role = "senior"
            "#,
        )
        .unwrap();
        assert!(is_admin(&conn, "1").unwrap());

        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "2"
            name = "Budi"
            role = "senior"
            "#,
        )
        .unwrap();

        assert_eq!(row_count(&conn), 1);
        assert!(!is_admin(&conn, "1").unwrap());
        assert!(!is_lead(&conn, "1").unwrap());
        assert!(!is_member(&conn, "1").unwrap());
        assert!(is_member(&conn, "2").unwrap());
    }

    #[test]
    fn seed_stores_github_user_and_leaves_the_id_for_the_sync() {
        let conn = open_test_db();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Budi"
            role = "senior"
            github_user = "budi-dev"

            [[members]]
            discord_user_id = "2"
            name = "Andi"
            role = "designer"
            "#,
        )
        .unwrap();
        let rows: Vec<(String, Option<String>, Option<i64>)> = conn
            .prepare("SELECT discord_user_id, github_user, github_id FROM members ORDER BY 1")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows,
            vec![
                ("1".into(), Some("budi-dev".into()), None),
                ("2".into(), None, None),
            ]
        );
    }

    #[test]
    fn reseed_keeps_a_resolved_github_id_only_while_github_user_is_unchanged() {
        let conn = open_test_db();
        let roster = |budi: &str| {
            format!(
                r#"
                [[members]]
                discord_user_id = "1"
                name = "Budi"
                role = "senior"
                github_user = "{budi}"

                [[members]]
                discord_user_id = "2"
                name = "Sari"
                role = "senior"
                github_user = "sari"
                "#
            )
        };
        seed_from(&conn, &roster("budi-dev")).unwrap();
        conn.execute_batch("UPDATE members SET github_id = 100 + discord_user_id")
            .unwrap();
        let id_of = |id: &str| -> Option<i64> {
            conn.query_row(
                "SELECT github_id FROM members WHERE discord_user_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };

        // Same usernames (different case): ids kept.
        seed_from(&conn, &roster("Budi-Dev")).unwrap();
        assert_eq!(id_of("1"), Some(101));
        assert_eq!(id_of("2"), Some(102));

        // Budi's username changed: his id is cleared for the sync to
        // re-resolve; Sari's is untouched.
        seed_from(&conn, &roster("budi-new")).unwrap();
        assert_eq!(id_of("1"), None);
        assert_eq!(id_of("2"), Some(102));
    }

    #[test]
    fn no_file_present_clears_the_roster() {
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
        seed_member(&conn, "1", "Stale");
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
    fn validate_counts_members_without_a_db_connection() {
        let summary = validate_from(
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "lead"

            [[members]]
            discord_user_id = "2"
            name = "Budi"
            role = "viewer"
            github_user = "budi-dev"
            "#,
        )
        .unwrap();

        assert_eq!(
            summary,
            RosterSummary {
                members: 2,
                with_github: 1,
            }
        );
    }

    #[test]
    fn validate_rejects_an_invalid_github_user() {
        let err = validate_from(
            "[[members]]\ndiscord_user_id = \"1\"\nname = \"Budi\"\nrole = \"senior\"\ngithub_user = \"@budi\"\n",
        )
        .unwrap_err();

        assert!(err.to_string().contains("drop the leading @"), "{err}");
    }

    #[test]
    fn validate_rejects_a_github_user_shared_by_two_members() {
        let err = validate_from(
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Alice"
            role = "lead"
            github_user = "Budi-Dev"

            [[members]]
            discord_user_id = "2"
            name = "Budi"
            role = "senior"
            github_user = "budi-dev"
            "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("only one member"), "{err}");
    }

    #[test]
    fn is_valid_github_user_follows_githubs_rules() {
        assert!(is_valid_github_user("budi"));
        assert!(is_valid_github_user("budi-dev-2"));
        assert!(is_valid_github_user(&"a".repeat(39)));
        assert!(!is_valid_github_user(""));
        assert!(!is_valid_github_user(&"a".repeat(40)));
        assert!(!is_valid_github_user("-budi"));
        assert!(!is_valid_github_user("budi_dev"));
        assert!(!is_valid_github_user("budi dev"));
        assert!(!is_valid_github_user("@budi"));
    }

    #[test]
    fn validate_rejects_an_invalid_role() {
        let err = validate_from(
            "[[members]]\ndiscord_user_id = \"1\"\nname = \"Ops\"\nrole = \"admin\"\n",
        )
        .unwrap_err();

        assert!(err.to_string().contains("admin"), "{err}");
    }

    #[test]
    fn validate_returns_zero_with_no_file_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        let empty_xdg = tempfile::tempdir().unwrap();
        unsafe {
            env::remove_var(MEMBERS_PATH_OVERRIDE_ENV);
            env::set_var("XDG_CONFIG_HOME", empty_xdg.path());
        }
        let summary = validate().unwrap();
        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
        assert_eq!(summary, RosterSummary::default());
    }

    #[test]
    fn all_member_ids_returns_every_seeded_member_except_viewers() {
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

            [[members]]
            discord_user_id = "3"
            name = "Citra"
            role = "viewer"
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
    fn is_admin_is_independent_of_role() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Ops"
            role = "senior"
            is_admin = true
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
        assert!(
            !is_lead,
            "is_admin must not grant is_lead - they're set independently"
        );
        assert!(is_admin);
    }

    #[test]
    fn is_admin_is_true_only_for_flagged_members() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Ops"
            role = "lead"
            is_admin = true

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
    fn reseeding_without_is_admin_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            "[[members]]\ndiscord_user_id = \"1\"\nname = \"X\"\nrole = \"lead\"\nis_admin = true\n",
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
    fn viewer_role_grants_is_lead_but_not_is_admin() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            "[[members]]\ndiscord_user_id = \"1\"\nname = \"Watcher\"\nrole = \"viewer\"\n",
        )
        .unwrap();

        assert!(is_lead(&conn, "1").unwrap());
        assert!(!is_admin(&conn, "1").unwrap());
    }

    #[test]
    fn is_active_lead_admits_lead_but_not_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("d.sqlite3")).unwrap();
        seed_from(
            &conn,
            r#"
            [[members]]
            discord_user_id = "1"
            name = "Lead"
            role = "lead"

            [[members]]
            discord_user_id = "2"
            name = "Watcher"
            role = "viewer"
            "#,
        )
        .unwrap();

        assert!(is_active_lead(&conn, "1").unwrap());
        assert!(!is_active_lead(&conn, "2").unwrap());
        assert!(!is_active_lead(&conn, "999").unwrap());
    }

    #[test]
    fn name_of_finds_a_seeded_member_and_misses_an_unknown_id() {
        let conn = open_test_db();
        seed_member(&conn, "7", "Gita");

        assert_eq!(name_of(&conn, "7").unwrap().as_deref(), Some("Gita"));
        assert_eq!(name_of(&conn, "999").unwrap(), None);
    }
}

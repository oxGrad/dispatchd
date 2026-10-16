//! DB side of the GitHub sync: upserting what `graphql.rs` parsed, the
//! sync's own bookkeeping, and resolving members' GitHub usernames to
//! numeric ids. No network and no serenity types, per the repo's "DB logic
//! in its own module" split; the report queries arrive with design phase 3.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use std::collections::BTreeMap;

use super::graphql::{Issue, PullRequest, SubIssueList};

pub const PR_HIGH_WATER: &str = "pr_high_water";
pub const ISSUE_HIGH_WATER: &str = "issue_high_water";
pub const LAST_SYNC_AT: &str = "last_sync_at";
pub const LAST_SYNC_ERROR: &str = "last_sync_error";

pub fn get_state(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM github_sync_state WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn set_state(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO github_sync_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// How much of a sync error `record_sync_error` keeps - it ends up inside
/// `/admin status`'s single Discord message (2000 characters in all).
pub const MAX_SYNC_ERROR_CHARS: usize = 300;

/// Stores a failed run's error for `/admin status`, truncated to
/// `MAX_SYNC_ERROR_CHARS`.
pub fn record_sync_error(conn: &Connection, error: &str) -> Result<()> {
    set_state(
        conn,
        LAST_SYNC_ERROR,
        &truncate(error, MAX_SYNC_ERROR_CHARS),
    )
}

/// `s` cut to at most `max` characters (not bytes), with a trailing `...`
/// when anything was dropped.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

pub fn clear_state(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM github_sync_state WHERE key = ?1", [key])?;
    Ok(())
}

/// Raises the stored high-water mark to `value` if it's later (RFC3339 UTC
/// strings from GitHub compare correctly as text). Never lowers it, so an
/// out-of-order page can't make the next sync re-fetch further back.
pub fn raise_high_water(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO github_sync_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value
         WHERE excluded.value > github_sync_state.value",
        params![key, value],
    )?;
    Ok(())
}

/// Everything one sync run writes, applied by `write_batch` in a single
/// transaction.
#[derive(Default)]
pub struct Batch<'a> {
    pub issues: &'a [Issue],
    pub prs: &'a [PullRequest],
    /// Full sub-issue lists fetched this run, for unlinking children a
    /// deliverable no longer lists.
    pub sub_issue_lists: &'a [SubIssueList],
    pub pr_high_water: Option<&'a str>,
    pub issue_high_water: Option<&'a str>,
}

/// Applies one sync run's writes in a single transaction: one commit (one
/// fsync) for the whole run rather than one per row, so a big backfill
/// doesn't hold the bot's shared DB lock for long, and the high-water marks
/// only move if every row landed.
pub fn write_batch(conn: &Connection, batch: &Batch) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    // The same issue can arrive several times in one run (a parent riding
    // along with each of its updated children, a sub-issue that was also a
    // search hit); every copy carries the same data, so write each once.
    let issues: BTreeMap<&str, &Issue> = batch
        .issues
        .iter()
        .map(|i| (i.node_id.as_str(), i))
        .collect();
    for issue in issues.values() {
        upsert_issue(&tx, issue)?;
    }
    for list in batch.sub_issue_lists.iter().filter(|l| l.complete) {
        unlink_detached_children(&tx, list)?;
    }
    for pr in batch.prs {
        upsert_pr(&tx, pr)?;
    }
    if let Some(hw) = batch.pr_high_water {
        raise_high_water(&tx, PR_HIGH_WATER, hw)?;
    }
    if let Some(hw) = batch.issue_high_water {
        raise_high_water(&tx, ISSUE_HIGH_WATER, hw)?;
    }
    tx.commit()?;
    Ok(())
}

/// A sub-issue removed from a deliverable doesn't necessarily change its
/// own `updatedAt`, so it would never be refetched; when the parent's full
/// list comes back without it, clear its stored link here instead. Only
/// called for a complete list - a truncated one can't prove absence.
fn unlink_detached_children(conn: &Connection, list: &SubIssueList) -> Result<()> {
    let mut stmt = conn.prepare("SELECT node_id FROM github_issues WHERE parent_node_id = ?1")?;
    let stored: Vec<String> = stmt
        .query_map([&list.parent_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for node_id in stored {
        if !list.children.iter().any(|c| c.node_id == node_id) {
            conn.execute(
                "UPDATE github_issues SET parent_node_id = NULL WHERE node_id = ?1",
                [&node_id],
            )?;
        }
    }
    Ok(())
}

/// Inserts or updates one issue and replaces its assignee set. A sub-issue
/// never stores a SOW ref of its own (it takes its parent's), so `sow_ref`
/// is only written for a top-level issue. Runs inside `write_batch`'s
/// transaction.
fn upsert_issue(conn: &Connection, issue: &Issue) -> Result<()> {
    let sow_ref = match issue.parent_node_id {
        None => issue.sow_ref.map(|s| s.to_string()),
        Some(_) => None,
    };
    conn.execute(
        "INSERT INTO github_issues (node_id, repo, number, title, state, state_reason,
             author_id, author_login, created_at, closed_at, parent_node_id, sow_ref, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(node_id) DO UPDATE SET
             repo = excluded.repo, number = excluded.number, title = excluded.title,
             state = excluded.state, state_reason = excluded.state_reason,
             author_id = excluded.author_id, author_login = excluded.author_login,
             created_at = excluded.created_at, closed_at = excluded.closed_at,
             parent_node_id = excluded.parent_node_id, sow_ref = excluded.sow_ref,
             updated_at = excluded.updated_at",
        params![
            issue.node_id,
            issue.repo,
            issue.number,
            issue.title,
            issue.state,
            issue.state_reason,
            issue.author.id,
            issue.author.login,
            issue.created_at,
            issue.closed_at,
            issue.parent_node_id,
            sow_ref,
            issue.updated_at,
        ],
    )?;
    conn.execute(
        "DELETE FROM github_issue_assignees WHERE issue_node_id = ?1",
        [&issue.node_id],
    )?;
    for github_id in &issue.assignee_ids {
        conn.execute(
            "INSERT OR IGNORE INTO github_issue_assignees (issue_node_id, github_id)
             VALUES (?1, ?2)",
            params![issue.node_id, github_id],
        )?;
    }
    Ok(())
}

/// Inserts or updates one PR, replaces its linked-issue set, and upserts
/// its reviews (reviews are never deleted: a dismissed review comes back
/// with state `dismissed`). Runs inside `write_batch`'s transaction.
fn upsert_pr(conn: &Connection, pr: &PullRequest) -> Result<()> {
    conn.execute(
        "INSERT INTO github_prs (node_id, repo, number, title, state, is_draft, author_id,
             author_login, created_at, merged_at, closed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(node_id) DO UPDATE SET
             repo = excluded.repo, number = excluded.number, title = excluded.title,
             state = excluded.state, is_draft = excluded.is_draft,
             author_id = excluded.author_id, author_login = excluded.author_login,
             created_at = excluded.created_at, merged_at = excluded.merged_at,
             closed_at = excluded.closed_at, updated_at = excluded.updated_at",
        params![
            pr.node_id,
            pr.repo,
            pr.number,
            pr.title,
            pr.state,
            pr.is_draft,
            pr.author.id,
            pr.author.login,
            pr.created_at,
            pr.merged_at,
            pr.closed_at,
            pr.updated_at,
        ],
    )?;
    conn.execute(
        "DELETE FROM github_pr_issues WHERE pr_node_id = ?1",
        [&pr.node_id],
    )?;
    for issue_id in &pr.closing_issue_ids {
        conn.execute(
            "INSERT OR IGNORE INTO github_pr_issues (pr_node_id, issue_node_id) VALUES (?1, ?2)",
            params![pr.node_id, issue_id],
        )?;
    }
    for review in &pr.reviews {
        conn.execute(
            "INSERT INTO github_reviews (node_id, pr_node_id, author_id, author_login, state,
                 submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(node_id) DO UPDATE SET
                 author_id = excluded.author_id, author_login = excluded.author_login,
                 state = excluded.state, submitted_at = excluded.submitted_at",
            params![
                review.node_id,
                pr.node_id,
                review.author.id,
                review.author.login,
                review.state,
                review.submitted_at,
            ],
        )?;
    }
    Ok(())
}

/// Of `ids`, the issues not stored yet - ones a PR links to that the
/// issue search didn't return, which the sync then fetches by id.
pub fn missing_issue_ids(conn: &Connection, ids: &[String]) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT 1 FROM github_issues WHERE node_id = ?1")?;
    let mut missing = Vec::new();
    for id in ids {
        if !stmt.exists([id])? && !missing.contains(id) {
            missing.push(id.clone());
        }
    }
    Ok(missing)
}

/// Members with a `github_user` whose numeric id isn't resolved yet (every
/// reseed clears it), as `(discord_user_id, github_user)`.
pub fn members_needing_github_id(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT discord_user_id, github_user FROM members
         WHERE github_user IS NOT NULL AND github_id IS NULL
         ORDER BY discord_user_id",
    )?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn set_member_github_id(
    conn: &Connection,
    discord_user_id: &str,
    github_id: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE members SET github_id = ?2 WHERE discord_user_id = ?1",
        params![discord_user_id, github_id],
    )?;
    Ok(())
}

/// What `/admin status` shows about the sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSummary {
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub prs: u64,
    pub issues: u64,
    pub reviews: u64,
}

pub fn sync_summary(conn: &Connection) -> Result<SyncSummary> {
    let count = |table: &str| -> Result<u64> {
        Ok(
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })? as u64,
        )
    };
    Ok(SyncSummary {
        last_sync_at: get_state(conn, LAST_SYNC_AT)?,
        last_error: get_state(conn, LAST_SYNC_ERROR)?,
        prs: count("github_prs")?,
        issues: count("github_issues")?,
        reviews: count("github_reviews")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::graphql::{Actor, Review};
    use crate::sow::SowRef;

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    fn actor(login: &str, id: i64) -> Actor {
        Actor {
            id: Some(id),
            login: Some(login.into()),
        }
    }

    fn issue(node_id: &str, parent: Option<&str>) -> Issue {
        Issue {
            node_id: node_id.into(),
            repo: "api".into(),
            number: 201,
            title: "Stripe webhook handler".into(),
            state: "open".into(),
            state_reason: None,
            author: actor("sari", 2),
            created_at: "2026-10-01T03:00:00Z".into(),
            updated_at: "2026-10-15T09:00:00Z".into(),
            closed_at: None,
            assignee_ids: vec![2, 3],
            sow_ref: Some(SowRef {
                milestone: 1,
                deliverable: Some(2),
            }),
            parent_node_id: parent.map(Into::into),
            origin: crate::github::graphql::Origin::SearchHit,
            warnings: vec![],
        }
    }

    fn pr() -> PullRequest {
        PullRequest {
            node_id: "PR_1".into(),
            repo: "api".into(),
            number: 210,
            title: "Stripe webhook handler".into(),
            state: "open".into(),
            is_draft: true,
            author: actor("budi-dev", 1),
            created_at: "2026-10-14T02:00:00Z".into(),
            updated_at: "2026-10-14T02:00:00Z".into(),
            merged_at: None,
            closed_at: None,
            closing_issue_ids: vec!["I_201".into()],
            reviews: vec![Review {
                node_id: "R_1".into(),
                author: actor("rina", 3),
                state: "commented".into(),
                submitted_at: "2026-10-14T05:00:00Z".into(),
            }],
        }
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn state_round_trips_and_clears() {
        let conn = open_test_db();
        assert_eq!(get_state(&conn, LAST_SYNC_ERROR).unwrap(), None);
        set_state(&conn, LAST_SYNC_ERROR, "boom").unwrap();
        set_state(&conn, LAST_SYNC_ERROR, "boom again").unwrap();
        assert_eq!(
            get_state(&conn, LAST_SYNC_ERROR).unwrap().as_deref(),
            Some("boom again")
        );
        clear_state(&conn, LAST_SYNC_ERROR).unwrap();
        assert_eq!(get_state(&conn, LAST_SYNC_ERROR).unwrap(), None);
    }

    #[test]
    fn raise_high_water_never_moves_backwards() {
        let conn = open_test_db();
        raise_high_water(&conn, PR_HIGH_WATER, "2026-10-15T09:00:00Z").unwrap();
        raise_high_water(&conn, PR_HIGH_WATER, "2026-10-14T00:00:00Z").unwrap();
        assert_eq!(
            get_state(&conn, PR_HIGH_WATER).unwrap().as_deref(),
            Some("2026-10-15T09:00:00Z")
        );
        raise_high_water(&conn, PR_HIGH_WATER, "2026-10-16T00:00:00Z").unwrap();
        assert_eq!(
            get_state(&conn, PR_HIGH_WATER).unwrap().as_deref(),
            Some("2026-10-16T00:00:00Z")
        );
    }

    #[test]
    fn upsert_issue_updates_in_place_and_replaces_assignees() {
        let conn = open_test_db();
        upsert_issue(&conn, &issue("I_100", None)).unwrap();

        let mut closed = issue("I_100", None);
        closed.state = "closed".into();
        closed.state_reason = Some("completed".into());
        closed.closed_at = Some("2026-10-15T10:00:00Z".into());
        closed.assignee_ids = vec![3];
        upsert_issue(&conn, &closed).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM github_issues"), 1);
        let (state, reason, sow): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT state, state_reason, sow_ref FROM github_issues WHERE node_id = 'I_100'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "closed");
        assert_eq!(reason.as_deref(), Some("completed"));
        assert_eq!(sow.as_deref(), Some("D.P.1.2"));
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM github_issue_assignees"),
            1
        );
    }

    #[test]
    fn a_sub_issue_never_stores_its_own_sow_ref() {
        let conn = open_test_db();
        upsert_issue(&conn, &issue("I_201", Some("I_100"))).unwrap();
        let (parent, sow): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT parent_node_id, sow_ref FROM github_issues WHERE node_id = 'I_201'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("I_100"));
        assert_eq!(sow, None);
    }

    #[test]
    fn upsert_pr_tracks_merge_links_and_reviews() {
        let conn = open_test_db();
        upsert_pr(&conn, &pr()).unwrap();

        let mut merged = pr();
        merged.state = "merged".into();
        merged.is_draft = false;
        merged.merged_at = Some("2026-10-15T08:00:00Z".into());
        merged.closing_issue_ids = vec!["I_201".into(), "I_202".into()];
        merged.reviews.push(Review {
            node_id: "R_2".into(),
            author: actor("sari", 2),
            state: "approved".into(),
            submitted_at: "2026-10-15T07:00:00Z".into(),
        });
        upsert_pr(&conn, &merged).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM github_prs"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM github_prs WHERE state = 'merged' AND is_draft = 0"
            ),
            1
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM github_pr_issues"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM github_reviews"), 2);
    }

    #[test]
    fn write_batch_writes_everything_once_and_raises_both_marks() {
        let conn = open_test_db();
        let parent = issue("I_100", None);
        let child = issue("I_201", Some("I_100"));
        // The parent arrives twice, as it would riding along with two children.
        let issues = vec![parent.clone(), child, parent];
        let prs = vec![pr()];
        write_batch(
            &conn,
            &Batch {
                issues: &issues,
                prs: &prs,
                pr_high_water: Some("2026-10-14T02:00:00Z"),
                issue_high_water: Some("2026-10-15T09:00:00Z"),
                ..Batch::default()
            },
        )
        .unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM github_issues"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM github_prs"), 1);
        assert_eq!(
            get_state(&conn, PR_HIGH_WATER).unwrap().as_deref(),
            Some("2026-10-14T02:00:00Z")
        );
        assert_eq!(
            get_state(&conn, ISSUE_HIGH_WATER).unwrap().as_deref(),
            Some("2026-10-15T09:00:00Z")
        );
    }

    fn parent_of(conn: &Connection, node_id: &str) -> Option<String> {
        conn.query_row(
            "SELECT parent_node_id FROM github_issues WHERE node_id = ?1",
            [node_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_complete_sub_issue_list_unlinks_children_no_longer_in_it() {
        let conn = open_test_db();
        let stored = vec![issue("I_201", Some("I_100")), issue("I_202", Some("I_100"))];
        write_batch(
            &conn,
            &Batch {
                issues: &stored,
                ..Batch::default()
            },
        )
        .unwrap();

        // I_202 was removed from the deliverable; its own row didn't change.
        let lists = vec![SubIssueList {
            parent_id: "I_100".into(),
            children: vec![issue("I_201", Some("I_100"))],
            complete: true,
        }];
        write_batch(
            &conn,
            &Batch {
                sub_issue_lists: &lists,
                ..Batch::default()
            },
        )
        .unwrap();

        assert_eq!(parent_of(&conn, "I_201").as_deref(), Some("I_100"));
        assert_eq!(parent_of(&conn, "I_202"), None);
    }

    #[test]
    fn an_incomplete_sub_issue_list_unlinks_nothing() {
        let conn = open_test_db();
        let stored = vec![issue("I_202", Some("I_100"))];
        write_batch(
            &conn,
            &Batch {
                issues: &stored,
                ..Batch::default()
            },
        )
        .unwrap();
        let lists = vec![SubIssueList {
            parent_id: "I_100".into(),
            children: vec![],
            complete: false,
        }];
        write_batch(
            &conn,
            &Batch {
                sub_issue_lists: &lists,
                ..Batch::default()
            },
        )
        .unwrap();
        assert_eq!(parent_of(&conn, "I_202").as_deref(), Some("I_100"));
    }

    #[test]
    fn record_sync_error_caps_the_stored_length() {
        let conn = open_test_db();
        record_sync_error(&conn, &"x".repeat(5_000)).unwrap();
        let stored = get_state(&conn, LAST_SYNC_ERROR).unwrap().unwrap();
        assert_eq!(stored.chars().count(), MAX_SYNC_ERROR_CHARS);
        assert!(stored.ends_with("..."));
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn missing_issue_ids_returns_unknown_ids_once() {
        let conn = open_test_db();
        upsert_issue(&conn, &issue("I_100", None)).unwrap();
        let ids = vec!["I_100".into(), "I_201".into(), "I_201".into()];
        assert_eq!(missing_issue_ids(&conn, &ids).unwrap(), vec!["I_201"]);
    }

    #[test]
    fn members_needing_github_id_until_resolved() {
        let conn = open_test_db();
        conn.execute_batch(
            "INSERT INTO members (discord_user_id, name, role, is_lead, github_user)
                 VALUES ('1', 'Budi', 'senior', 0, 'budi-dev');
             INSERT INTO members (discord_user_id, name, role, is_lead)
                 VALUES ('2', 'Andi', 'designer', 0);",
        )
        .unwrap();
        assert_eq!(
            members_needing_github_id(&conn).unwrap(),
            vec![("1".to_string(), "budi-dev".to_string())]
        );
        set_member_github_id(&conn, "1", 583231).unwrap();
        assert!(members_needing_github_id(&conn).unwrap().is_empty());
    }

    #[test]
    fn sync_summary_counts_rows_and_reads_state() {
        let conn = open_test_db();
        upsert_pr(&conn, &pr()).unwrap();
        upsert_issue(&conn, &issue("I_201", None)).unwrap();
        set_state(&conn, LAST_SYNC_AT, "2026-10-16T01:00:00Z").unwrap();
        assert_eq!(
            sync_summary(&conn).unwrap(),
            SyncSummary {
                last_sync_at: Some("2026-10-16T01:00:00Z".into()),
                last_error: None,
                prs: 1,
                issues: 1,
                reviews: 1,
            }
        );
    }
}

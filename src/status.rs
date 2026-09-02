use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};

pub struct MemberStatus {
    pub name: String,
    pub todo_count: i64,
    pub matched_update_count: i64,
    /// Today's non-null SOW refs from this member's todos, unique and in
    /// first-seen (id) order.
    pub sow_refs: Vec<String>,
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

        let mut sow_ref_stmt = conn.prepare(
            "SELECT sow_ref FROM entries
             WHERE type = 'todo' AND date = ?1 AND discord_user_id = ?2 AND sow_ref IS NOT NULL
             ORDER BY id",
        )?;
        let mut sow_refs = Vec::new();
        let mut seen = HashSet::new();
        for sow_ref in sow_ref_stmt.query_map(params![date, discord_user_id], |row| {
            row.get::<_, String>(0)
        })? {
            let sow_ref = sow_ref?;
            if seen.insert(sow_ref.clone()) {
                sow_refs.push(sow_ref);
            }
        }

        result.push(MemberStatus {
            name,
            todo_count,
            matched_update_count,
            sow_refs,
        });
    }
    Ok(result)
}

/// Full per-member detail for `/team report`: every todo with its notes,
/// SOW ref and matching progress report(s), plus any unplanned progress.
#[derive(Debug, PartialEq, Eq)]
pub struct UpdateDetail {
    pub task: String,
    pub status: String,
    pub progress: String,
    pub blocker: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct TodoDetail {
    pub task: String,
    pub notes: Option<String>,
    pub sow_ref: Option<String>,
    pub updates: Vec<UpdateDetail>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MemberReport {
    pub name: String,
    pub todos: Vec<TodoDetail>,
    pub ad_hoc: Vec<UpdateDetail>,
}

/// One `MemberReport` per roster row, ordered by name. Per-member queries
/// rather than one big JOIN - same rationale as `team_status`: a 6-person
/// team's worth of rows, easier to read and verify.
pub fn team_report(conn: &Connection, date: &str) -> Result<Vec<MemberReport>> {
    let mut members_stmt =
        conn.prepare("SELECT discord_user_id, name FROM members ORDER BY name")?;
    let members = members_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut reports = Vec::with_capacity(members.len());
    for (discord_user_id, name) in members {
        let mut todo_stmt = conn.prepare(
            "SELECT id, task, notes, sow_ref FROM entries
             WHERE type = 'todo' AND date = ?1 AND discord_user_id = ?2
             ORDER BY id",
        )?;
        let todo_rows = todo_stmt
            .query_map(params![date, discord_user_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut todos = Vec::with_capacity(todo_rows.len());
        for (todo_id, task, notes, sow_ref) in todo_rows {
            // date-scoped to match team_status's matched_update_count filter, so
            // /team status and /team report agree for a given day. A cross-midnight
            // update (todo dated D, its update dated D+1) is shown by neither.
            let mut upd_stmt = conn.prepare(
                "SELECT task, status, progress, blocker FROM entries
                 WHERE type = 'update' AND todo_id = ?1 AND date = ?2
                 ORDER BY id",
            )?;
            let updates = upd_stmt
                .query_map(params![todo_id, date], row_to_update_detail)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            todos.push(TodoDetail {
                task,
                notes,
                sow_ref,
                updates,
            });
        }

        let mut adhoc_stmt = conn.prepare(
            "SELECT task, status, progress, blocker FROM entries
             WHERE type = 'update' AND date = ?1 AND discord_user_id = ?2 AND todo_id IS NULL
             ORDER BY id",
        )?;
        let ad_hoc = adhoc_stmt
            .query_map(params![date, discord_user_id], row_to_update_detail)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        reports.push(MemberReport {
            name,
            todos,
            ad_hoc,
        });
    }
    Ok(reports)
}

fn row_to_update_detail(row: &rusqlite::Row<'_>) -> rusqlite::Result<UpdateDetail> {
    Ok(UpdateDetail {
        task: row.get(0)?,
        status: row.get(1)?,
        progress: row.get(2)?,
        blocker: row.get(3)?,
    })
}

/// Formats one `/team status` line, e.g. `✅ Alice — 3/3 updated`, with
/// any SOW refs tagged on today's todos appended in parens, e.g.
/// `✅ Alice — 3/3 updated (M1D1, M1D2, M2)`. A member with no todos posted
/// shows no fraction (`0/0` reads as noise); one who posted todos but
/// matched none of them is treated the same as "no todo posted" - both
/// are the "needs attention" case. A member with no SOW refs set gets no
/// trailing parens (a `todo_count == 0` member structurally can't have
/// any either, since the column only exists on todo rows).
pub fn format_status_line(status: &MemberStatus) -> String {
    let base = if status.todo_count == 0 {
        format!("❌ {} — no todo posted", status.name)
    } else {
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
    };
    if status.sow_refs.is_empty() {
        base
    } else {
        format!("{base} ({})", status.sow_refs.join(", "))
    }
}

/// `(glyph, label)` for a progress-report status value. Unknown values
/// (shouldn't happen - the command only writes done/in_progress/blocked)
/// fall back to a neutral bullet and the raw string.
fn status_glyph_label(status: &str) -> (&'static str, String) {
    match status {
        "done" => ("✅", "done".to_string()),
        "in_progress" => ("⏳", "in progress".to_string()),
        "blocked" => ("⛔", "blocked".to_string()),
        other => ("•", other.to_string()),
    }
}

fn push_update_line(out: &mut String, update: &UpdateDetail) {
    let (glyph, label) = status_glyph_label(&update.status);
    out.push_str(&format!("  {glyph} {label} — {}", update.progress));
    if let Some(blocker) = &update.blocker {
        out.push_str(&format!(" (blocker: {blocker})"));
    }
}

/// One member's block for `/team report`, in Discord markdown, with no
/// trailing newline. Callers join member blocks with "\n\n".
pub fn format_report(report: &MemberReport) -> String {
    if report.todos.is_empty() && report.ad_hoc.is_empty() {
        return format!("**{}** — nothing posted today", report.name);
    }

    let mut out = format!("**{}**", report.name);
    for todo in &report.todos {
        out.push('\n');
        match &todo.sow_ref {
            Some(sow_ref) => out.push_str(&format!("• {} [{sow_ref}]", todo.task)),
            None => out.push_str(&format!("• {}", todo.task)),
        }
        if let Some(notes) = &todo.notes {
            out.push_str(&format!("\n  notes: {notes}"));
        }
        if todo.updates.is_empty() {
            out.push_str("\n  ❌ no progress report yet");
        } else {
            for update in &todo.updates {
                out.push('\n');
                push_update_line(&mut out, update);
            }
        }
    }
    for update in &report.ad_hoc {
        out.push_str(&format!("\n• unplanned: {}\n", update.task));
        push_update_line(&mut out, update);
    }
    out
}

/// Packs member blocks (separated by "\n\n" in `full`) into chunks of at
/// most `limit` characters, so `/team report` fits Discord's 2000-char
/// message cap. A single block over `limit` is hard-wrapped at char
/// boundaries. Empty input yields no chunks.
pub fn split_into_messages(full: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for block in full.split("\n\n") {
        if block.is_empty() {
            continue;
        }
        let sep_len = if current.is_empty() { 0 } else { 2 };
        let fits = current.chars().count() + sep_len + block.chars().count() <= limit;
        if fits {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(block);
            continue;
        }

        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if block.chars().count() > limit {
            let mut chars = block.chars();
            loop {
                let chunk: String = chars.by_ref().take(limit).collect();
                if chunk.is_empty() {
                    break;
                }
                out.push(chunk);
            }
        } else {
            current = block.to_string();
        }
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
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
            let todo_id = entries::insert_todo(&conn, "1", DATE, task, None, None).unwrap();
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
        let todo1 = entries::insert_todo(&conn, "2", DATE, "a", None, None).unwrap();
        entries::insert_todo(&conn, "2", DATE, "b", None, None).unwrap();
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
        entries::insert_todo(&conn, "4", DATE, "a", None, None).unwrap();
        entries::insert_todo(&conn, "4", DATE, "b", None, None).unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(format_status_line(&statuses[0]), "❌ Dedi — 0/2 updated");
    }

    #[test]
    fn ad_hoc_update_does_not_count_toward_matched() {
        let conn = open_test_db();
        seed_member(&conn, "5", "Eka", "junior");
        entries::insert_todo(&conn, "5", DATE, "a", None, None).unwrap();
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
        let todo_id = entries::insert_todo(&conn, "6", DATE, "a", None, None).unwrap();
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
    fn sow_refs_are_appended_in_first_seen_order() {
        let conn = open_test_db();
        seed_member(&conn, "7", "Gita", "senior");
        let todo1 = entries::insert_todo(&conn, "7", DATE, "a", None, Some("M1D1")).unwrap();
        entries::insert_todo(&conn, "7", DATE, "b", None, Some("M1D2")).unwrap();
        entries::insert_update(&conn, "7", DATE, "a", Some(todo1), "done", "done", None).unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(statuses[0].sow_refs, vec!["M1D1", "M1D2"]);
        assert_eq!(
            format_status_line(&statuses[0]),
            "⚠️ Gita — 1/2 updated (M1D1, M1D2)"
        );
    }

    #[test]
    fn repeated_sow_ref_across_todos_appears_once() {
        let conn = open_test_db();
        seed_member(&conn, "8", "Hadi", "medior");
        entries::insert_todo(&conn, "8", DATE, "a", None, Some("M1")).unwrap();
        entries::insert_todo(&conn, "8", DATE, "b", None, Some("M1")).unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert_eq!(statuses[0].sow_refs, vec!["M1"]);
    }

    #[test]
    fn no_sow_refs_leaves_the_line_unchanged() {
        let conn = open_test_db();
        seed_member(&conn, "9", "Ida", "junior");
        entries::insert_todo(&conn, "9", DATE, "a", None, None).unwrap();

        let statuses = team_status(&conn, DATE).unwrap();
        assert!(statuses[0].sow_refs.is_empty());
        assert_eq!(format_status_line(&statuses[0]), "❌ Ida — 0/1 updated");
    }

    #[test]
    fn team_report_nests_updates_under_their_todo_in_id_order() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let todo = entries::insert_todo(
            &conn,
            "1",
            DATE,
            "Refactor auth",
            Some("split it"),
            Some("M1D2"),
        )
        .unwrap();
        entries::insert_update(
            &conn,
            "1",
            DATE,
            "Refactor auth",
            Some(todo),
            "in_progress",
            "started",
            None,
        )
        .unwrap();
        entries::insert_update(
            &conn,
            "1",
            DATE,
            "Refactor auth",
            Some(todo),
            "done",
            "finished",
            None,
        )
        .unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].name, "Alice");
        assert_eq!(reports[0].todos.len(), 1);
        let t = &reports[0].todos[0];
        assert_eq!(t.task, "Refactor auth");
        assert_eq!(t.notes.as_deref(), Some("split it"));
        assert_eq!(t.sow_ref.as_deref(), Some("M1D2"));
        assert_eq!(
            t.updates,
            vec![
                UpdateDetail {
                    task: "Refactor auth".into(),
                    status: "in_progress".into(),
                    progress: "started".into(),
                    blocker: None
                },
                UpdateDetail {
                    task: "Refactor auth".into(),
                    status: "done".into(),
                    progress: "finished".into(),
                    blocker: None
                },
            ]
        );
        assert!(reports[0].ad_hoc.is_empty());
    }

    #[test]
    fn team_report_puts_unmatched_updates_in_ad_hoc() {
        let conn = open_test_db();
        seed_member(&conn, "2", "Budi", "designer");
        entries::insert_update(
            &conn,
            "2",
            DATE,
            "Hotfix prod",
            None,
            "done",
            "added an index",
            Some("was waiting on DBA"),
        )
        .unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert!(reports[0].todos.is_empty());
        assert_eq!(
            reports[0].ad_hoc,
            vec![UpdateDetail {
                task: "Hotfix prod".into(),
                status: "done".into(),
                progress: "added an index".into(),
                blocker: Some("was waiting on DBA".into()),
            }]
        );
    }

    #[test]
    fn team_report_includes_members_with_nothing_ordered_by_name() {
        let conn = open_test_db();
        seed_member(&conn, "9", "Zoya", "junior");
        seed_member(&conn, "1", "Alice", "lead");

        let reports = team_report(&conn, DATE).unwrap();
        let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Alice", "Zoya"]);
        assert!(reports[0].todos.is_empty() && reports[0].ad_hoc.is_empty());
    }

    #[test]
    fn team_report_todo_with_no_update_has_empty_updates() {
        let conn = open_test_db();
        seed_member(&conn, "3", "Citra", "senior");
        entries::insert_todo(&conn, "3", DATE, "Design audit", None, None).unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert_eq!(reports[0].todos.len(), 1);
        assert!(reports[0].todos[0].updates.is_empty());
        assert!(reports[0].todos[0].notes.is_none());
        assert!(reports[0].todos[0].sow_ref.is_none());
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

    fn sample_report() -> MemberReport {
        MemberReport {
            name: "Alice".into(),
            todos: vec![
                TodoDetail {
                    task: "Refactor auth".into(),
                    notes: Some("split into service + handler".into()),
                    sow_ref: Some("M1D2".into()),
                    updates: vec![UpdateDetail {
                        task: "Refactor auth".into(),
                        status: "done".into(),
                        progress: "extracted AuthService".into(),
                        blocker: None,
                    }],
                },
                TodoDetail {
                    task: "Write migration".into(),
                    notes: None,
                    sow_ref: None,
                    updates: vec![UpdateDetail {
                        task: "Write migration".into(),
                        status: "blocked".into(),
                        progress: "schema drafted".into(),
                        blocker: Some("waiting on DBA".into()),
                    }],
                },
                TodoDetail {
                    task: "Docs pass".into(),
                    notes: None,
                    sow_ref: None,
                    updates: vec![],
                },
            ],
            ad_hoc: vec![UpdateDetail {
                task: "Hotfix prod 500".into(),
                status: "done".into(),
                progress: "bad index, added it".into(),
                blocker: None,
            }],
        }
    }

    #[test]
    fn format_report_renders_todos_notes_updates_and_ad_hoc() {
        let out = format_report(&sample_report());
        assert_eq!(
            out,
            "**Alice**\n\
             • Refactor auth [M1D2]\n\
             \u{20}\u{20}notes: split into service + handler\n\
             \u{20}\u{20}✅ done — extracted AuthService\n\
             • Write migration\n\
             \u{20}\u{20}⛔ blocked — schema drafted (blocker: waiting on DBA)\n\
             • Docs pass\n\
             \u{20}\u{20}❌ no progress report yet\n\
             • unplanned: Hotfix prod 500\n\
             \u{20}\u{20}✅ done — bad index, added it"
        );
    }

    #[test]
    fn format_report_empty_member_is_a_single_line() {
        let report = MemberReport {
            name: "Budi".into(),
            todos: vec![],
            ad_hoc: vec![],
        };
        assert_eq!(format_report(&report), "**Budi** — nothing posted today");
    }

    #[test]
    fn format_report_unknown_status_falls_back_to_verbatim() {
        let report = MemberReport {
            name: "Citra".into(),
            todos: vec![TodoDetail {
                task: "Thing".into(),
                notes: None,
                sow_ref: None,
                updates: vec![UpdateDetail {
                    task: "Thing".into(),
                    status: "weird".into(),
                    progress: "hmm".into(),
                    blocker: None,
                }],
            }],
            ad_hoc: vec![],
        };
        assert_eq!(
            format_report(&report),
            "**Citra**\n• Thing\n\u{20}\u{20}• weird — hmm"
        );
    }

    #[test]
    fn split_into_messages_empty_input_is_empty() {
        assert_eq!(split_into_messages("", 2000), Vec::<String>::new());
    }

    #[test]
    fn split_into_messages_keeps_a_small_report_as_one_chunk() {
        let full = "**Alice**\n• a\n  ✅ done — x\n\n**Budi** — nothing posted today";
        assert_eq!(split_into_messages(full, 2000), vec![full.to_string()]);
    }

    #[test]
    fn split_into_messages_breaks_on_member_boundaries() {
        let a = format!("**Alice**\n{}", "x".repeat(1200));
        let b = format!("**Budi**\n{}", "y".repeat(1200));
        let c = format!("**Citra**\n{}", "z".repeat(1200));
        let full = format!("{a}\n\n{b}\n\n{c}");

        let chunks = split_into_messages(&full, 2000);
        assert_eq!(chunks, vec![a, b, c]);
        for chunk in &split_into_messages(&full, 2000) {
            assert!(chunk.chars().count() <= 2000);
        }
    }

    #[test]
    fn split_into_messages_hard_wraps_an_oversized_block() {
        let full = format!("**Alice**\n{}", "x".repeat(4100));
        let chunks = split_into_messages(&full, 2000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 2000);
        assert_eq!(chunks[1].chars().count(), 2000);
        assert!(chunks[2].chars().count() <= 2000);
        assert_eq!(chunks.concat().chars().count(), full.chars().count());
    }

    #[test]
    fn team_report_pipeline_chunks_without_losing_content() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        seed_member(&conn, "2", "Budi", "designer");
        let a1 = entries::insert_todo(
            &conn,
            "1",
            DATE,
            "Refactor auth",
            Some("notes here"),
            Some("M1D2"),
        )
        .unwrap();
        entries::insert_update(
            &conn,
            "1",
            DATE,
            "Refactor auth",
            Some(a1),
            "done",
            "extracted service",
            None,
        )
        .unwrap();
        entries::insert_todo(&conn, "2", DATE, "Design audit", None, None).unwrap();
        entries::insert_update(&conn, "2", DATE, "Hotfix", None, "done", "shipped", None).unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        let full = reports
            .iter()
            .map(format_report)
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = split_into_messages(&full, 1900);

        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 1900);
        }
        let joined = chunks.concat();
        for needle in [
            "Alice",
            "Budi",
            "Refactor auth",
            "Design audit",
            "M1D2",
            "Hotfix",
            "extracted service",
        ] {
            assert!(joined.contains(needle), "chunked output lost {needle:?}");
        }
    }

    #[test]
    fn team_report_separates_matched_and_ad_hoc_for_one_member() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let t = entries::insert_todo(&conn, "1", DATE, "Planned work", None, None).unwrap();
        entries::insert_update(
            &conn,
            "1",
            DATE,
            "Planned work",
            Some(t),
            "in_progress",
            "halfway",
            None,
        )
        .unwrap();
        entries::insert_update(
            &conn,
            "1",
            DATE,
            "Surprise incident",
            None,
            "done",
            "resolved",
            None,
        )
        .unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        assert_eq!(reports[0].todos.len(), 1);
        assert_eq!(reports[0].todos[0].updates.len(), 1);
        assert_eq!(reports[0].todos[0].updates[0].progress, "halfway");
        assert_eq!(reports[0].ad_hoc.len(), 1);
        assert_eq!(reports[0].ad_hoc[0].task, "Surprise incident");
    }

    #[test]
    fn team_report_does_not_leak_one_members_updates_into_another() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        seed_member(&conn, "2", "Budi", "designer");
        let a = entries::insert_todo(&conn, "1", DATE, "Alice task", None, None).unwrap();
        entries::insert_update(
            &conn,
            "1",
            DATE,
            "Alice task",
            Some(a),
            "done",
            "alice progress",
            None,
        )
        .unwrap();
        entries::insert_todo(&conn, "2", DATE, "Budi task", None, None).unwrap();

        let reports = team_report(&conn, DATE).unwrap();
        let budi = reports.iter().find(|r| r.name == "Budi").unwrap();
        assert_eq!(budi.todos.len(), 1);
        assert!(budi.todos[0].updates.is_empty());
        assert!(budi.ad_hoc.is_empty());
    }

    #[test]
    fn split_into_messages_packs_two_blocks_that_exactly_hit_the_limit() {
        let a = "a".repeat(100);
        let b = "b".repeat(98);
        // 100 + 2 (for "\n\n") + 98 == 200
        let full = format!("{a}\n\n{b}");
        let chunks = split_into_messages(&full, 200);
        assert_eq!(chunks, vec![full]);
    }
}

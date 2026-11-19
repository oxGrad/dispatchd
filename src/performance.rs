//! Per-member review report (`/team performance`): standup discipline plus
//! output, as data and `.md` rendering. Serenity-free.
#![allow(dead_code)] // the `/team performance` command uses this; removed with it

use crate::entries::short_date;
use crate::followups::missed_detail;
use crate::github::report::{MemberActivity, team_activity, window};
use anyhow::Result;
use jiff::tz::TimeZone;
use rusqlite::{Connection, params};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberPerformance {
    pub name: String,
    pub expected: usize,
    pub on_time: usize,
    pub leave_dates: Vec<String>,
    pub sick_dates: Vec<String>,
    pub missed_todo: Vec<String>,
    pub missed_progress: Vec<String>,
    pub tasks: usize,
    pub done: usize,
    pub blocked_days: usize,
    pub github: Option<MemberActivity>,
}

fn strings(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<String>> {
    Ok(conn
        .prepare(sql)?
        .query_map(p, |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// One row per non-viewer member, alphabetical.
pub fn performance(
    conn: &Connection,
    start: &str,
    end: &str,
    github: Option<&TimeZone>,
) -> Result<Vec<MemberPerformance>> {
    let recorded = strings(
        conn,
        "SELECT date FROM reminders_sent WHERE type = 'missed_recorded'
         AND date >= ?1 AND date <= ?2 ORDER BY date",
        params![start, end],
    )?;
    let missed = missed_detail(conn, start, end)?;
    let activity = match github {
        Some(tz) => Some(team_activity(conn, &window(start, end, tz)?)?),
        None => None,
    };
    let members: Vec<(String, String)> = conn
        .prepare("SELECT discord_user_id, name FROM members WHERE role != 'viewer' ORDER BY name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut out = Vec::new();
    for (id, name) in members {
        let first_seen: Option<String> = conn.query_row(
            "SELECT MIN(d) FROM (
                 SELECT date AS d FROM entries WHERE discord_user_id = ?1
                 UNION ALL SELECT date FROM missed_submissions WHERE discord_user_id = ?1
                 UNION ALL SELECT date FROM member_leave WHERE discord_user_id = ?1)",
            [&id],
            |r| r.get(0),
        )?;
        let leave_in = |reason: &str| {
            strings(
                conn,
                "SELECT date FROM member_leave WHERE discord_user_id = ?1 AND reason = ?2
                 AND date >= ?3 AND date <= ?4 ORDER BY date",
                params![id, reason, start, end],
            )
        };
        let leave_dates = leave_in("leave")?;
        let sick_dates = leave_in("sick")?;
        let off: HashSet<&String> = leave_dates.iter().chain(&sick_dates).collect();
        let missed_any: HashSet<String> = strings(
            conn,
            "SELECT date FROM missed_submissions WHERE discord_user_id = ?1
             AND date >= ?2 AND date <= ?3",
            params![id, start, end],
        )?
        .into_iter()
        .collect();
        let expected_days: Vec<&String> = recorded
            .iter()
            .filter(|d| first_seen.as_deref().is_some_and(|f| d.as_str() >= f) && !off.contains(d))
            .collect();
        let on_time = expected_days
            .iter()
            .filter(|d| !missed_any.contains(**d))
            .count();

        let detail = missed.iter().find(|d| d.member == name);
        let (tasks, done): (usize, usize) = {
            let rows: Vec<bool> = conn
                .prepare(
                    "WITH RECURSIVE chain(root, id) AS (
                         SELECT id, id FROM entries WHERE type = 'todo' AND carried_from IS NULL
                         UNION ALL
                         SELECT chain.root, e.id FROM entries e JOIN chain ON e.carried_from = chain.id)
                     SELECT DISTINCT c.root, EXISTS(
                         SELECT 1 FROM chain c2 JOIN entries u
                           ON u.todo_id = c2.id AND u.type = 'update'
                          AND u.status = 'done' AND u.date <= ?4
                         WHERE c2.root = c.root)
                     FROM chain c JOIN entries t ON t.id = c.id
                     WHERE t.discord_user_id = ?1 AND t.date >= ?2 AND t.date <= ?3",
                )?
                .query_map(params![id, start, end, end], |r| r.get(1))?
                .collect::<rusqlite::Result<_>>()?;
            (rows.len(), rows.iter().filter(|d| **d).count())
        };
        let blocked_days: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT date) FROM entries WHERE discord_user_id = ?1
             AND type = 'update' AND status = 'blocked' AND date >= ?2 AND date <= ?3",
            params![id, start, end],
            |r| r.get(0),
        )?;

        out.push(MemberPerformance {
            github: activity
                .as_ref()
                .and_then(|a| a.members.iter().find(|m| m.name == name).cloned()),
            name,
            expected: expected_days.len(),
            on_time,
            leave_dates,
            sick_dates,
            missed_todo: detail
                .map(|d| d.missed_todo_dates.clone())
                .unwrap_or_default(),
            missed_progress: detail
                .map(|d| d.missed_update_dates.clone())
                .unwrap_or_default(),
            tasks,
            done,
            blocked_days: blocked_days as usize,
        });
    }
    Ok(out)
}

fn dates(ds: &[String]) -> String {
    ds.iter()
        .map(|d| short_date(d))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn format_performance_file(
    rows: &[MemberPerformance],
    start: &str,
    end: &str,
    with_github: bool,
    rules_since: Option<&str>,
) -> String {
    let mut out = format!(
        "# Team performance: {} – {}\n",
        short_date(start),
        short_date(end)
    );
    if let Some(s) = rules_since.filter(|s| start < *s) {
        out.push_str(&format!(
            "\n> Misses before {} were recorded under the old end-of-day rule (no cut-offs).\n",
            short_date(s)
        ));
    }
    let name = |r: &MemberPerformance| r.name.replace('|', "/");

    let discipline: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let on_time = if r.expected == 0 {
                "—".to_string()
            } else {
                format!(
                    "{}%",
                    (100.0 * r.on_time as f64 / r.expected as f64).round()
                )
            };
            vec![
                name(r),
                r.expected.to_string(),
                r.leave_dates.len().to_string(),
                r.sick_dates.len().to_string(),
                r.missed_todo.len().to_string(),
                r.missed_progress.len().to_string(),
                on_time,
            ]
        })
        .collect();
    out.push_str("\n## Standup discipline\n\n");
    out.push_str(&crate::status::render_aligned_table(
        &[
            "Member",
            "Expected",
            "Leave",
            "Sick",
            "Missed /todo",
            "Missed /progress",
            "On time",
        ],
        &discipline,
    ));

    let mut headers = vec!["Member", "Tasks", "Done", "Blocked days"];
    if with_github {
        headers.extend(["PRs merged", "PRs open", "Reviews", "Sub-issues closed"]);
    }
    let output: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut cells = vec![
                name(r),
                r.tasks.to_string(),
                r.done.to_string(),
                r.blocked_days.to_string(),
            ];
            if with_github {
                match r.github.as_ref().filter(|g| g.linked) {
                    Some(g) => cells.extend([
                        g.prs_merged.len().to_string(),
                        g.prs_opened.len().to_string(),
                        g.reviewed.len().to_string(),
                        g.sub_issues_closed.len().to_string(),
                    ]),
                    None => cells.extend(std::iter::repeat_n("—".to_string(), 4)),
                }
            }
            cells
        })
        .collect();
    out.push_str("\n## Output\n\n");
    out.push_str(&crate::status::render_aligned_table(&headers, &output));

    for r in rows {
        let refs = |v: &[crate::github::report::ItemRef]| {
            v.iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut lines = Vec::new();
        for (label, ds) in [
            ("Leave", &r.leave_dates),
            ("Sick", &r.sick_dates),
            ("Missed /todo", &r.missed_todo),
            ("Missed /progress", &r.missed_progress),
        ] {
            if !ds.is_empty() {
                lines.push(format!("- {label}: {}\n", dates(ds)));
            }
        }
        if with_github && let Some(g) = &r.github {
            if !g.prs_merged.is_empty() {
                lines.push(format!("- PRs merged: {}\n", refs(&g.prs_merged)));
            }
            if !g.reviewed.is_empty() {
                lines.push(format!("- Reviewed: {}\n", refs(&g.reviewed)));
            }
        }
        if !lines.is_empty() {
            out.push_str(&format!("\n## {}\n{}", name(r), lines.concat()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries;

    fn member(conn: &Connection, id: &str, name: &str, role: &str, gh: Option<(&str, i64)>) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead, github_user, github_id)
             VALUES (?1, ?2, ?3, 0, ?4, ?5)",
            params![id, name, role, gh.map(|g| g.0), gh.map(|g| g.1)],
        )
        .unwrap();
    }

    fn carry(conn: &Connection, from: i64, date: &str) -> i64 {
        let id = entries::insert_todo(conn, "1", date, "A", None, None).unwrap();
        conn.execute(
            "UPDATE entries SET carried_from = ?1 WHERE id = ?2",
            params![from, id],
        )
        .unwrap();
        id
    }

    fn fixture() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.keep().join("d.sqlite3")).unwrap();
        member(&conn, "1", "Bima", "senior", None);
        member(&conn, "2", "Alya", "senior", None);
        member(&conn, "9", "Boss", "viewer", None);
        for d in 3..=7 {
            crate::reminders::mark_sent(&conn, &format!("2026-11-0{d}"), "missed_recorded")
                .unwrap();
        }
        let a = entries::insert_todo(&conn, "1", "2026-11-03", "A", None, None).unwrap();
        let a2 = carry(&conn, a, "2026-11-04");
        let a3 = carry(&conn, a2, "2026-11-05");
        entries::insert_update(&conn, "1", "2026-11-05", "A", Some(a3), "done", "x", None).unwrap();
        let b = entries::insert_todo(&conn, "1", "2026-11-06", "B", None, None).unwrap();
        for d in ["2026-11-06", "2026-11-07"] {
            entries::insert_update(&conn, "1", d, "B", Some(b), "blocked", "x", Some("y")).unwrap();
        }
        conn.execute(
            "INSERT INTO member_leave (date, discord_user_id, reason) VALUES ('2026-11-04', '1', 'sick')",
            [],
        )
        .unwrap();
        for (d, k) in [
            ("2026-11-05", "todo"),
            ("2026-11-05", "update"),
            ("2026-11-06", "update"),
        ] {
            conn.execute(
                "INSERT INTO missed_submissions (date, discord_user_id, kind) VALUES (?1, '1', ?2)",
                params![d, k],
            )
            .unwrap();
        }
        entries::insert_todo(&conn, "2", "2026-11-06", "C", None, None).unwrap();
        conn
    }

    fn row_cells(file: &str, name: &str) -> Vec<String> {
        file.lines()
            .find(|l| l.starts_with(&format!("| {name}")))
            .unwrap()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect()
    }

    #[test]
    fn discipline_counts_follow_the_spec() {
        // Recorded days Nov 3-7 (Mon-Fri); Bima first seen Nov 3; sick Nov 4;
        // missed /todo Nov 5; missed /progress Nov 5 and Nov 6.
        let conn = fixture();
        let bima = &performance(&conn, "2026-11-01", "2026-11-30", None).unwrap()[1];
        assert_eq!(bima.name, "Bima");
        assert_eq!((bima.expected, bima.on_time), (4, 2));
        assert_eq!(bima.sick_dates, vec!["2026-11-04"]);
        assert_eq!(bima.missed_todo, vec!["2026-11-05"]);
        assert_eq!(bima.missed_progress, vec!["2026-11-05", "2026-11-06"]);
    }

    #[test]
    fn expected_starts_at_first_seen_and_viewers_are_omitted() {
        // Alya's first record is Nov 6 -> expected 2 (Nov 6, 7), not 5.
        let conn = fixture();
        let rows = performance(&conn, "2026-11-01", "2026-11-30", None).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["Alya", "Bima"]
        );
        assert_eq!(rows[0].expected, 2);
    }

    #[test]
    fn a_carried_chain_counts_as_one_task() {
        // Bima: todo A (Nov 3) carried to Nov 4 and Nov 5, done Nov 5; todo B
        // (Nov 6) blocked Nov 6 and Nov 7, never done.
        let conn = fixture();
        let bima = &performance(&conn, "2026-11-01", "2026-11-30", None).unwrap()[1];
        assert_eq!((bima.tasks, bima.done, bima.blocked_days), (2, 1, 2));
    }

    #[test]
    fn file_renders_tables_then_people_and_the_rules_note() {
        let conn = fixture();
        let rows = performance(&conn, "2026-11-01", "2026-11-30", None).unwrap();
        let file =
            format_performance_file(&rows, "2026-11-01", "2026-11-30", false, Some("2026-11-03"));
        assert!(file.starts_with("# Team performance: Nov 1 – Nov 30\n\n> Misses before Nov 3 were recorded under the old end-of-day rule (no cut-offs).\n"), "{file}");
        assert!(file.contains("## Standup discipline\n\n| Member | Expected | Leave | Sick | Missed /todo | Missed /progress | On time |"));
        assert_eq!(
            row_cells(&file, "Bima")[..7],
            ["Bima", "4", "0", "1", "1", "2", "50%"]
        );
        assert!(file.contains("## Output\n\n| Member | Tasks | Done | Blocked days |"));
        assert!(!file.contains("PRs merged"));
        assert!(file.contains(
            "## Bima\n- Sick: Nov 4\n- Missed /todo: Nov 5\n- Missed /progress: Nov 5, Nov 6\n"
        ));
    }

    #[test]
    fn on_time_is_a_dash_when_nothing_was_expected_and_no_note_after_rules_since() {
        let rows = vec![MemberPerformance {
            name: "New".into(),
            expected: 0,
            on_time: 0,
            leave_dates: vec![],
            sick_dates: vec![],
            missed_todo: vec![],
            missed_progress: vec![],
            tasks: 0,
            done: 0,
            blocked_days: 0,
            github: None,
        }];
        let file =
            format_performance_file(&rows, "2026-12-01", "2026-12-05", false, Some("2026-11-03"));
        assert!(!file.contains("> Misses before"));
        assert!(file.contains("| —"), "{file}");
    }

    #[test]
    fn github_columns_and_sections_for_linked_members_only() {
        let conn = fixture();
        conn.execute(
            "UPDATE members SET github_user = 'bima-dev', github_id = 1 WHERE discord_user_id = '1'",
            [],
        )
        .unwrap();
        for (node, n, author, merged) in [
            ("PR_210", 210, Some(1), Some("2026-11-04T08:00:00Z")),
            ("PR_300", 300, None, None),
        ] {
            conn.execute(
                "INSERT INTO github_prs (node_id, repo, number, title, state, is_draft, author_id,
                     author_login, created_at, merged_at, closed_at, updated_at)
                 VALUES (?1, 'api', ?2, 't', ?3, 0, ?4, 'x', '2026-11-03T00:00:00Z', ?5, ?5,
                     COALESCE(?5, '2026-11-03T00:00:00Z'))",
                params![
                    node,
                    n,
                    if merged.is_some() { "merged" } else { "open" },
                    author,
                    merged
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO github_reviews (node_id, pr_node_id, author_id, author_login, state, submitted_at)
             VALUES ('R_1', 'PR_300', 1, 'x', 'approved', '2026-11-05T07:00:00Z')",
            [],
        )
        .unwrap();

        let rows = performance(&conn, "2026-11-01", "2026-11-30", Some(&TimeZone::UTC)).unwrap();
        let file = format_performance_file(&rows, "2026-11-01", "2026-11-30", true, None);
        assert!(file.contains(
            "| Member | Tasks | Done | Blocked days | PRs merged | PRs open | Reviews | Sub-issues closed |"
        ));
        let output = file.split("## Output").nth(1).unwrap();
        assert_eq!(row_cells(output, "Bima")[4..], ["1", "0", "1", "0"]);
        assert_eq!(row_cells(output, "Alya")[4..], ["—", "—", "—", "—"]);
        assert!(file.contains("- PRs merged: api#210\n"), "{file}");
    }
}

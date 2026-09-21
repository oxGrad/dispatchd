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
/// nagged by the todo follow-up. Shared by the live `update_followup` nag
/// and `record_missed` below - a stated todo left with no report against
/// it is the sharper "missed /progress" signal, not just "posted zero
/// updates today" (which double-counts a total no-show already caught by
/// `members_missing_todo`, and would also flag someone with zero todos
/// who has nothing to report progress on in the first place).
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

/// One member's detail for the `/missed` report: every date in the
/// queried range they missed `/todo` entirely, and every date they left a
/// todo with no `/progress` report against it, each ascending. Built from
/// `missed_submissions` (see `record_missed` below), not computed live -
/// only members with at least one miss are returned by `missed_detail`, a
/// clean record isn't worth a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissedDetail {
    pub member: String,
    pub missed_todo_dates: Vec<String>,
    pub missed_update_dates: Vec<String>,
}

impl MissedDetail {
    /// Total missed days across both kinds - the ranking `/missed`'s
    /// summary table sorts by, most-missed first.
    fn missed_days(&self) -> usize {
        self.missed_todo_dates.len() + self.missed_update_dates.len()
    }
}

/// Records `date`'s miss snapshot into `missed_submissions`: one row per
/// member per kind - 'todo' from `members_missing_todo` (no todo
/// submitted at all), 'update' from `members_missing_update` (had a todo
/// today but left at least one without a `/progress` report against it -
/// the tighter, more actionable signal than "posted zero updates all
/// day", and one that correctly excludes a member with no todo at all,
/// since `members_missing_todo` already covers that case). A member
/// hitting both gets a row of each kind, so `/missed` can report on
/// either independently. `INSERT OR IGNORE` makes this idempotent - safe
/// to call more than once for the same date - though the ticker only
/// ever does so once, gated by its own `reminders_sent` marker (see
/// `discord::ticker`). Unlike `followups_sent`/`reminders_sent`, nothing
/// prunes this table - it's retained history, same as `entries`.
pub fn record_missed(conn: &Connection, date: &str) -> Result<()> {
    for id in members_missing_todo(conn, date)? {
        conn.execute(
            "INSERT OR IGNORE INTO missed_submissions (date, discord_user_id, kind) VALUES (?1, ?2, 'todo')",
            params![date, id],
        )?;
    }
    for id in members_missing_update(conn, date)? {
        conn.execute(
            "INSERT OR IGNORE INTO missed_submissions (date, discord_user_id, kind) VALUES (?1, ?2, 'update')",
            params![date, id],
        )?;
    }
    Ok(())
}

/// The `/missed` report's underlying data: one `MissedDetail` per member
/// with at least one miss in `[start, end]` (inclusive) per
/// `missed_submissions`, alphabetical by name (the summary table
/// `format_missed_report_file` renders below the detail re-sorts by day
/// count - this order is about finding a specific member's section
/// quickly).
/// `start`/`end` are trusted to already be validated `YYYY-MM-DD` strings
/// (see `recap::resolve_range`, reused by the command).
pub fn missed_detail(conn: &Connection, start: &str, end: &str) -> Result<Vec<MissedDetail>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(m.name, ms.discord_user_id) AS member, ms.date, ms.kind
         FROM missed_submissions ms
         LEFT JOIN members m ON m.discord_user_id = ms.discord_user_id
         WHERE ms.date >= ?1 AND ms.date <= ?2
         ORDER BY member ASC, ms.date ASC",
    )?;
    let rows = stmt
        .query_map(params![start, end], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut details: Vec<MissedDetail> = Vec::new();
    for (member, date, kind) in rows {
        if details.last().is_none_or(|d| d.member != member) {
            details.push(MissedDetail {
                member,
                missed_todo_dates: Vec::new(),
                missed_update_dates: Vec::new(),
            });
        }
        let detail = details.last_mut().expect("just pushed above");
        match kind.as_str() {
            "todo" => detail.missed_todo_dates.push(date),
            "update" => detail.missed_update_dates.push(date),
            _ => {}
        }
    }
    Ok(details)
}

/// Renders the `/missed` report as `.md` file content: a per-member
/// detail section - which exact dates they missed `/todo` and which they
/// missed `/progress` - then a summary table of day counts across the
/// whole range ranked most-missed first (`status::render_aligned_table`,
/// so the columns actually line up in the file). `None` when `details` is
/// empty - the caller shows a plain "nothing missed" message instead, no
/// file needed. Dates render short (`entries::short_date`, e.g. "Sep 15"),
/// same as everywhere else a date reaches a Discord reply.
pub fn format_missed_report_file(
    details: &[MissedDetail],
    start: &str,
    end: &str,
) -> Option<String> {
    if details.is_empty() {
        return None;
    }

    let mut out = format!("# Missed submissions ({start} to {end})\n");
    for detail in details {
        out.push_str(&format!("\n## {}\n", detail.member.replace('|', "/")));
        if !detail.missed_todo_dates.is_empty() {
            out.push_str(&format!(
                "- Missed /todo: {}\n",
                short_date_list(&detail.missed_todo_dates)
            ));
        }
        if !detail.missed_update_dates.is_empty() {
            out.push_str(&format!(
                "- Missed /progress: {}\n",
                short_date_list(&detail.missed_update_dates)
            ));
        }
    }

    let mut ranked: Vec<&MissedDetail> = details.iter().collect();
    ranked.sort_by(|a, b| {
        b.missed_days()
            .cmp(&a.missed_days())
            .then_with(|| a.member.cmp(&b.member))
    });
    let headers = ["Member", "Missed /todo", "Missed /progress"];
    let rows: Vec<Vec<String>> = ranked
        .iter()
        .map(|d| {
            vec![
                d.member.replace('|', "/"),
                d.missed_todo_dates.len().to_string(),
                d.missed_update_dates.len().to_string(),
            ]
        })
        .collect();
    out.push_str(&format!(
        "\n## Summary\n\n{}",
        crate::status::render_aligned_table(&headers, &rows)
    ));
    Some(out)
}

fn short_date_list(dates: &[String]) -> String {
    dates
        .iter()
        .map(|d| crate::entries::short_date(d))
        .collect::<Vec<_>>()
        .join(", ")
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

    #[test]
    fn record_missed_writes_a_row_per_kind_for_each_missing_member() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // no todo at all - todo miss only
        seed_member(&conn, "2", "Budi"); // todo left with no update - both

        entries::insert_todo(&conn, "2", DATE, "b", None, None).unwrap();

        record_missed(&conn, DATE).unwrap();

        let detail = missed_detail(&conn, DATE, DATE).unwrap();
        assert_eq!(
            detail,
            vec![
                MissedDetail {
                    member: "Alice".to_string(),
                    missed_todo_dates: vec![DATE.to_string()],
                    missed_update_dates: vec![],
                },
                MissedDetail {
                    member: "Budi".to_string(),
                    missed_todo_dates: vec![],
                    missed_update_dates: vec![DATE.to_string()],
                },
            ]
        );
    }

    #[test]
    fn record_missed_excludes_a_member_with_no_todo_from_the_update_miss() {
        // A total no-show (no todo, no update) is a todo miss, not an
        // "also missed update" - there's nothing to have reported
        // progress against in the first place.
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");

        record_missed(&conn, DATE).unwrap();

        let detail = missed_detail(&conn, DATE, DATE).unwrap();
        assert_eq!(detail[0].missed_todo_dates, vec![DATE.to_string()]);
        assert!(detail[0].missed_update_dates.is_empty());
    }

    #[test]
    fn record_missed_is_idempotent() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();

        record_missed(&conn, DATE).unwrap();
        record_missed(&conn, DATE).unwrap();

        let detail = missed_detail(&conn, DATE, DATE).unwrap();
        assert_eq!(detail[0].missed_todo_dates, Vec::<String>::new());
        assert_eq!(detail[0].missed_update_dates, vec![DATE.to_string()]);
    }

    #[test]
    fn missed_detail_is_empty_for_a_member_with_no_misses() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        let todo = entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();
        entries::insert_update(&conn, "1", DATE, "a", Some(todo), "done", "x", None).unwrap();

        record_missed(&conn, DATE).unwrap();

        assert!(missed_detail(&conn, DATE, DATE).unwrap().is_empty());
    }

    #[test]
    fn missed_detail_lists_every_missed_todo_date_in_range_and_excludes_outside_days() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // no todo on any of the three days

        record_missed(&conn, "2026-08-28").unwrap();
        record_missed(&conn, "2026-08-29").unwrap();
        record_missed(&conn, "2026-08-30").unwrap();

        let detail = missed_detail(&conn, "2026-08-28", "2026-08-29").unwrap();
        assert_eq!(detail.len(), 1);
        assert_eq!(
            detail[0].missed_todo_dates,
            vec!["2026-08-28".to_string(), "2026-08-29".to_string()]
        );
        assert!(detail[0].missed_update_dates.is_empty());
    }

    #[test]
    fn missed_detail_lists_every_missed_update_date_in_range_and_excludes_outside_days() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");

        // A todo every day, never updated - the tighter "missed /progress"
        // signal, not a blanket zero-activity day.
        for date in ["2026-08-28", "2026-08-29", "2026-08-30"] {
            entries::insert_todo(&conn, "1", date, "a", None, None).unwrap();
            record_missed(&conn, date).unwrap();
        }

        let detail = missed_detail(&conn, "2026-08-28", "2026-08-29").unwrap();
        assert_eq!(detail.len(), 1);
        assert!(detail[0].missed_todo_dates.is_empty());
        assert_eq!(
            detail[0].missed_update_dates,
            vec!["2026-08-28".to_string(), "2026-08-29".to_string()]
        );
    }

    #[test]
    fn missed_detail_is_ordered_alphabetically_by_member() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Zed"); // no todo, no update at all - todo miss only
        seed_member(&conn, "2", "Alice"); // ad-hoc update posted, no todo - todo miss only

        entries::insert_update(&conn, "2", DATE, "hotfix", None, "done", "x", None).unwrap();

        record_missed(&conn, DATE).unwrap();

        let detail = missed_detail(&conn, DATE, DATE).unwrap();
        let members: Vec<&str> = detail.iter().map(|d| d.member.as_str()).collect();
        assert_eq!(members, vec!["Alice", "Zed"]);
    }

    #[test]
    fn format_missed_report_file_is_none_when_details_are_empty() {
        assert_eq!(
            format_missed_report_file(&[], "2026-08-01", "2026-08-14"),
            None
        );
    }

    #[test]
    fn format_missed_report_file_lists_dates_per_member_then_an_aligned_ranked_summary_table() {
        let details = vec![
            MissedDetail {
                member: "Alice".to_string(),
                missed_todo_dates: vec![],
                missed_update_dates: vec!["2026-08-01".to_string()],
            },
            MissedDetail {
                member: "Zed".to_string(),
                missed_todo_dates: vec!["2026-08-01".to_string(), "2026-08-02".to_string()],
                missed_update_dates: vec!["2026-08-02".to_string()],
            },
        ];
        let out = format_missed_report_file(&details, "2026-08-01", "2026-08-14").unwrap();
        assert!(out.starts_with("# Missed submissions (2026-08-01 to 2026-08-14)\n"));
        assert!(out.contains("## Alice\n- Missed /progress: Aug 1\n"));
        assert!(out.contains("## Zed\n- Missed /todo: Aug 1, Aug 2\n- Missed /progress: Aug 2\n"));
        assert!(out.contains("## Summary\n\n| Member"));

        // Zed (2+1=3 missed days) outranks Alice (0+1=1) in the summary,
        // and every table line (header, separator, both rows) lines up.
        let table_lines: Vec<&str> = out.lines().skip_while(|l| !l.starts_with('|')).collect();
        assert_eq!(table_lines.len(), 4);
        let width = table_lines[0].chars().count();
        for line in &table_lines {
            assert_eq!(line.chars().count(), width, "misaligned line: {line:?}");
        }
        assert!(table_lines[2].contains("Zed"));
        assert!(table_lines[3].contains("Alice"));
    }
}

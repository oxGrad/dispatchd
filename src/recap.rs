use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};

pub const DEFAULT_RECAP_DAYS: i64 = 14;
pub const MAX_RECAP_DAYS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecapRow {
    pub member: String,
    pub task: String,
    /// `None` means the todo has never had a progress report. Always
    /// `Some` for an ad-hoc (unplanned) row, since those are update rows
    /// themselves.
    pub status: Option<String>,
    pub progress: Option<String>,
    pub blocker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRecap {
    pub date: String,
    pub rows: Vec<RecapRow>,
}

/// Resolves `/recap`'s `start`/`end` options into a validated `(start,
/// end)` pair of `YYYY-MM-DD` strings - both omitted defaults to the last
/// `DEFAULT_RECAP_DAYS` days ending today; either alone fills in from the
/// other. Returns a user-facing error string rather than `anyhow::Error`:
/// every failure here is bad caller input (unparsable date, inverted or
/// oversized range), not an internal fault.
pub fn resolve_range(
    start: Option<&str>,
    end: Option<&str>,
    today: &str,
) -> Result<(String, String), String> {
    let today_date = NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .map_err(|_| "⚠️ Internal error resolving today's date.".to_string())?;

    let end_date = match end {
        Some(e) => NaiveDate::parse_from_str(e, "%Y-%m-%d")
            .map_err(|_| format!("⚠️ Invalid end date {e:?} - use YYYY-MM-DD."))?,
        None => today_date,
    };
    let start_date = match start {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| format!("⚠️ Invalid start date {s:?} - use YYYY-MM-DD."))?,
        None => end_date - chrono::Duration::days(DEFAULT_RECAP_DAYS - 1),
    };

    if start_date > end_date {
        return Err("⚠️ start must be on or before end.".to_string());
    }
    let span_days = (end_date - start_date).num_days() + 1;
    if span_days > MAX_RECAP_DAYS {
        return Err(format!(
            "⚠️ That's {span_days} days - /recap covers at most {MAX_RECAP_DAYS}. Pick a narrower range."
        ));
    }

    Ok((
        start_date.format("%Y-%m-%d").to_string(),
        end_date.format("%Y-%m-%d").to_string(),
    ))
}

/// One `DayRecap` per calendar day in `[start, end]` (inclusive) that has
/// at least one entry - empty days are omitted rather than rendered blank.
/// `start`/`end` are trusted to already be validated `YYYY-MM-DD` strings
/// (see `resolve_range`).
pub fn recap_range(conn: &Connection, start: &str, end: &str) -> Result<Vec<DayRecap>> {
    let start_date = NaiveDate::parse_from_str(start, "%Y-%m-%d").context("invalid start date")?;
    let end_date = NaiveDate::parse_from_str(end, "%Y-%m-%d").context("invalid end date")?;

    let mut out = Vec::new();
    let mut date = start_date;
    while date <= end_date {
        let day = date.format("%Y-%m-%d").to_string();
        let rows = day_rows(conn, &day)?;
        if !rows.is_empty() {
            out.push(DayRecap { date: day, rows });
        }
        date = date.succ_opt().expect("date overflow");
    }
    Ok(out)
}

/// Every todo dated `date` (member resolved via `members`, falling back to
/// the raw `discord_user_id` for a departed member so historical rows never
/// silently vanish) with its latest linked update, plus that day's ad-hoc
/// updates as "(unplanned)" rows - sorted by `bucket_rank` then member name
/// then task.
fn day_rows(conn: &Connection, date: &str) -> Result<Vec<RecapRow>> {
    let mut todo_stmt = conn.prepare(
        "SELECT e.id, e.task, COALESCE(m.name, e.discord_user_id) FROM entries e
         LEFT JOIN members m ON m.discord_user_id = e.discord_user_id
         WHERE e.type = 'todo' AND e.date = ?1
         ORDER BY e.id",
    )?;
    let todos = todo_stmt
        .query_map(params![date], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut rows = Vec::with_capacity(todos.len());
    for (todo_id, task, member) in todos {
        let latest: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT status, progress, blocker FROM entries
                 WHERE type = 'update' AND todo_id = ?1
                 ORDER BY id DESC LIMIT 1",
                params![todo_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (status, progress, blocker) = match latest {
            Some((s, p, b)) => (Some(s), Some(p), b),
            None => (None, None, None),
        };
        rows.push(RecapRow {
            member,
            task,
            status,
            progress,
            blocker,
        });
    }

    let mut adhoc_stmt = conn.prepare(
        "SELECT e.task, e.status, e.progress, e.blocker, COALESCE(m.name, e.discord_user_id)
         FROM entries e
         LEFT JOIN members m ON m.discord_user_id = e.discord_user_id
         WHERE e.type = 'update' AND e.date = ?1 AND e.todo_id IS NULL
         ORDER BY e.id",
    )?;
    let adhoc = adhoc_stmt
        .query_map(params![date], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (task, status, progress, blocker, member) in adhoc {
        rows.push(RecapRow {
            member,
            task: format!("{task} (unplanned)"),
            status: Some(status),
            progress: Some(progress),
            blocker,
        });
    }

    rows.sort_by(|a, b| {
        bucket_rank(a.status.as_deref())
            .cmp(&bucket_rank(b.status.as_deref()))
            .then_with(|| a.member.cmp(&b.member))
            .then_with(|| a.task.cmp(&b.task))
    });
    Ok(rows)
}

/// blocked, then never-updated ("no progress"), then in-progress, then
/// done - the rows that need the lead's attention (blocked, then nothing
/// reported at all, then still in flight) lead the table, with wrapped-up
/// work trailing at the bottom. Any other status value (shouldn't happen -
/// the command only ever writes done/in_progress/blocked) sorts last
/// rather than panicking.
fn bucket_rank(status: Option<&str>) -> u8 {
    match status {
        Some("blocked") => 0,
        None => 1,
        Some("in_progress") => 2,
        Some("done") => 3,
        Some(_) => 4,
    }
}

fn sanitize_cell(s: &str) -> String {
    s.replace('\n', " ").replace('|', "/")
}

/// Renders one day's table in Discord markdown, no trailing newline -
/// callers join day blocks with "\n\n" and can feed the joined string
/// straight into `status::split_into_messages` for Discord's 2000-char cap.
pub fn format_day_table(day: &DayRecap) -> String {
    let mut out = format!(
        "**{}**\n| Member | Task | Status | Progress | Blocker |\n|---|---|---|---|---|",
        day.date
    );
    for row in &day.rows {
        let (glyph, label) = match &row.status {
            Some(status) => crate::status::status_glyph_label(status),
            None => ("❌", "no report yet".to_string()),
        };
        out.push_str(&format!(
            "\n| {} | {} | {glyph} {label} | {} | {} |",
            sanitize_cell(&row.member),
            sanitize_cell(&row.task),
            row.progress
                .as_deref()
                .map(sanitize_cell)
                .unwrap_or_else(|| "—".to_string()),
            row.blocker
                .as_deref()
                .map(sanitize_cell)
                .unwrap_or_else(|| "—".to_string()),
        ));
    }
    out
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

    fn seed_member(conn: &Connection, id: &str, name: &str, role: &str) {
        conn.execute(
            "INSERT INTO members (discord_user_id, name, role, is_lead) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, role, role == "lead"],
        )
        .unwrap();
    }

    // --- resolve_range ---

    #[test]
    fn resolve_range_defaults_to_last_14_days_when_both_omitted() {
        let got = resolve_range(None, None, "2026-09-14").unwrap();
        assert_eq!(got, ("2026-09-01".to_string(), "2026-09-14".to_string()));
    }

    #[test]
    fn resolve_range_defaults_start_when_only_end_given() {
        let got = resolve_range(None, Some("2026-09-05"), "2026-09-14").unwrap();
        assert_eq!(got, ("2026-08-23".to_string(), "2026-09-05".to_string()));
    }

    #[test]
    fn resolve_range_defaults_end_to_today_when_only_start_given() {
        let got = resolve_range(Some("2026-09-01"), None, "2026-09-14").unwrap();
        assert_eq!(got, ("2026-09-01".to_string(), "2026-09-14".to_string()));
    }

    #[test]
    fn resolve_range_accepts_an_explicit_range() {
        let got = resolve_range(Some("2026-08-01"), Some("2026-08-10"), "2026-09-14").unwrap();
        assert_eq!(got, ("2026-08-01".to_string(), "2026-08-10".to_string()));
    }

    #[test]
    fn resolve_range_rejects_start_after_end() {
        let err = resolve_range(Some("2026-09-10"), Some("2026-09-01"), "2026-09-14").unwrap_err();
        assert!(err.contains("start must be on or before end"), "{err}");
    }

    #[test]
    fn resolve_range_rejects_invalid_date_format() {
        let err = resolve_range(Some("not-a-date"), None, "2026-09-14").unwrap_err();
        assert!(err.contains("Invalid start date"), "{err}");

        let err = resolve_range(None, Some("2026-13-40"), "2026-09-14").unwrap_err();
        assert!(err.contains("Invalid end date"), "{err}");
    }

    #[test]
    fn resolve_range_rejects_a_span_over_the_max() {
        let err = resolve_range(Some("2026-01-01"), Some("2026-12-31"), "2026-09-14").unwrap_err();
        assert!(err.contains("60"), "{err}");
    }

    #[test]
    fn resolve_range_accepts_a_span_exactly_at_the_max() {
        // 60 days inclusive: 2026-07-01 .. 2026-08-29
        let got = resolve_range(Some("2026-07-01"), Some("2026-08-29"), "2026-09-14").unwrap();
        assert_eq!(got, ("2026-07-01".to_string(), "2026-08-29".to_string()));
    }

    // --- recap_range ---

    #[test]
    fn recap_range_is_empty_when_theres_nothing_in_range() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let got = recap_range(&conn, "2026-09-01", "2026-09-03").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn recap_range_excludes_todos_outside_the_date_range() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", "2026-08-31", "Outside", None, None).unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-03").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn recap_range_buckets_a_never_updated_todo_as_no_report_yet() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", "2026-09-01", "Write tests", None, None).unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-01").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].date, "2026-09-01");
        assert_eq!(
            got[0].rows,
            vec![RecapRow {
                member: "Alice".to_string(),
                task: "Write tests".to_string(),
                status: None,
                progress: None,
                blocker: None,
            }]
        );
    }

    #[test]
    fn recap_range_uses_the_latest_update_for_a_todo_updated_more_than_once() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let todo = entries::insert_todo(&conn, "1", "2026-09-01", "Ship it", None, None).unwrap();
        entries::insert_update(
            &conn,
            "1",
            "2026-09-01",
            "Ship it",
            Some(todo),
            "in_progress",
            "started",
            None,
        )
        .unwrap();
        entries::insert_update(
            &conn,
            "1",
            "2026-09-02",
            "Ship it",
            Some(todo),
            "done",
            "shipped",
            None,
        )
        .unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-01").unwrap();
        assert_eq!(got[0].rows[0].status.as_deref(), Some("done"));
        assert_eq!(got[0].rows[0].progress.as_deref(), Some("shipped"));
    }

    #[test]
    fn recap_range_includes_ad_hoc_updates_as_unplanned_rows() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_update(
            &conn,
            "1",
            "2026-09-01",
            "Hotfix prod",
            None,
            "done",
            "added an index",
            Some("was waiting on DBA"),
        )
        .unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-01").unwrap();
        assert_eq!(
            got[0].rows,
            vec![RecapRow {
                member: "Alice".to_string(),
                task: "Hotfix prod (unplanned)".to_string(),
                status: Some("done".to_string()),
                progress: Some("added an index".to_string()),
                blocker: Some("was waiting on DBA".to_string()),
            }]
        );
    }

    #[test]
    fn recap_range_falls_back_to_the_raw_id_for_a_member_not_on_the_roster() {
        let conn = open_test_db();
        // "99" is never seeded into `members`.
        entries::insert_todo(&conn, "99", "2026-09-01", "Ghost task", None, None).unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-01").unwrap();
        assert_eq!(got[0].rows[0].member, "99");
    }

    #[test]
    fn recap_range_orders_blocked_then_no_report_then_in_progress_then_done() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        seed_member(&conn, "2", "Zed", "senior");

        let done = entries::insert_todo(&conn, "2", "2026-09-01", "Done task", None, None).unwrap();
        entries::insert_update(
            &conn,
            "2",
            "2026-09-01",
            "Done task",
            Some(done),
            "done",
            "x",
            None,
        )
        .unwrap();

        let in_progress =
            entries::insert_todo(&conn, "1", "2026-09-01", "In progress task", None, None).unwrap();
        entries::insert_update(
            &conn,
            "1",
            "2026-09-01",
            "In progress task",
            Some(in_progress),
            "in_progress",
            "x",
            None,
        )
        .unwrap();

        entries::insert_todo(&conn, "1", "2026-09-01", "No report task", None, None).unwrap();

        let blocked =
            entries::insert_todo(&conn, "2", "2026-09-01", "Blocked task", None, None).unwrap();
        entries::insert_update(
            &conn,
            "2",
            "2026-09-01",
            "Blocked task",
            Some(blocked),
            "blocked",
            "x",
            Some("stuck"),
        )
        .unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-01").unwrap();
        let tasks: Vec<&str> = got[0].rows.iter().map(|r| r.task.as_str()).collect();
        assert_eq!(
            tasks,
            vec![
                "Blocked task",
                "No report task",
                "In progress task",
                "Done task",
            ]
        );
    }

    #[test]
    fn recap_range_orders_same_bucket_rows_by_member_name() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Zed", "senior");
        seed_member(&conn, "2", "Alice", "lead");
        entries::insert_todo(&conn, "1", "2026-09-01", "Zed's task", None, None).unwrap();
        entries::insert_todo(&conn, "2", "2026-09-01", "Alice's task", None, None).unwrap();

        let got = recap_range(&conn, "2026-09-01", "2026-09-01").unwrap();
        let members: Vec<&str> = got[0].rows.iter().map(|r| r.member.as_str()).collect();
        assert_eq!(members, vec!["Alice", "Zed"]);
    }

    #[test]
    fn recap_range_produces_one_day_recap_per_non_empty_day_in_range() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        entries::insert_todo(&conn, "1", "2026-09-01", "Day one", None, None).unwrap();
        entries::insert_todo(&conn, "1", "2026-09-03", "Day three", None, None).unwrap();
        // 2026-09-02 has nothing - should be skipped entirely.

        let got = recap_range(&conn, "2026-09-01", "2026-09-03").unwrap();
        let dates: Vec<&str> = got.iter().map(|d| d.date.as_str()).collect();
        assert_eq!(dates, vec!["2026-09-01", "2026-09-03"]);
    }

    // --- format_day_table ---

    #[test]
    fn format_day_table_renders_header_and_rows() {
        let day = DayRecap {
            date: "2026-09-01".to_string(),
            rows: vec![
                RecapRow {
                    member: "Alice".to_string(),
                    task: "Refactor auth".to_string(),
                    status: Some("blocked".to_string()),
                    progress: Some("stuck on the parser".to_string()),
                    blocker: Some("needs review".to_string()),
                },
                RecapRow {
                    member: "Budi".to_string(),
                    task: "Design audit".to_string(),
                    status: None,
                    progress: None,
                    blocker: None,
                },
            ],
        };

        assert_eq!(
            format_day_table(&day),
            "**2026-09-01**\n\
             | Member | Task | Status | Progress | Blocker |\n\
             |---|---|---|---|---|\n\
             | Alice | Refactor auth | ⛔ blocked | stuck on the parser | needs review |\n\
             | Budi | Design audit | ❌ no report yet | — | — |"
        );
    }

    #[test]
    fn format_day_table_sanitizes_pipes_and_newlines_in_cell_text() {
        let day = DayRecap {
            date: "2026-09-01".to_string(),
            rows: vec![RecapRow {
                member: "Alice".to_string(),
                task: "A | B".to_string(),
                status: Some("done".to_string()),
                progress: Some("line one\nline two".to_string()),
                blocker: None,
            }],
        };

        let out = format_day_table(&day);
        assert!(out.contains("A / B"));
        assert!(out.contains("line one line two"));
        assert_eq!(out.lines().count(), 4);
    }
}

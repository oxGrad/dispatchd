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

/// Builds a `RecapRow` for each `(todo_id, task, member)`, filling in the
/// status/progress/blocker from that todo's latest linked update (`None`
/// for all three if it's never been reported on).
fn rows_for_todos(conn: &Connection, todos: Vec<(i64, String, String)>) -> Result<Vec<RecapRow>> {
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
    Ok(rows)
}

fn sort_rows(rows: &mut [RecapRow]) {
    rows.sort_by(|a, b| {
        bucket_rank(a.status.as_deref())
            .cmp(&bucket_rank(b.status.as_deref()))
            .then_with(|| a.member.cmp(&b.member))
            .then_with(|| a.task.cmp(&b.task))
    });
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

    let mut rows = rows_for_todos(conn, todos)?;

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

    sort_rows(&mut rows);
    Ok(rows)
}

/// Todos from *before* `date` that got a progress report filed on `date` -
/// a carried-over task worked on today. `day_rows(date)` misses these
/// entirely: it only pulls todos whose own `date` matches, and such an
/// update always carries a `todo_id`, so it's excluded from `day_rows`'s
/// ad-hoc (`todo_id IS NULL`) query too. Not used by `/recap`/`/missed`,
/// whose per-day tables are keyed by a todo's origin day on purpose (a
/// long-lived todo already shows there with its latest-ever status - see
/// `recap_range_uses_the_latest_update_for_a_todo_updated_more_than_once` -
/// so re-surfacing it on every day it's touched would just duplicate the
/// row); only `today_recap`, backing the ticker's day summary, needs
/// today's carried-over activity to show up on today specifically.
fn carried_over_rows(conn: &Connection, date: &str) -> Result<Vec<RecapRow>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.id, e.task, COALESCE(m.name, e.discord_user_id) FROM entries e
         LEFT JOIN members m ON m.discord_user_id = e.discord_user_id
         WHERE e.type = 'todo' AND e.date < ?1
           AND EXISTS (
             SELECT 1 FROM entries u
             WHERE u.type = 'update' AND u.todo_id = e.id AND u.date = ?1
           )
         ORDER BY e.id",
    )?;
    let todos = stmt
        .query_map(params![date], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows_for_todos(conn, todos)
}

/// The ticker's day-summary view of `date`: `day_rows(date)` plus
/// `carried_over_rows(date)` (today's activity on a task carried in from
/// an earlier day), merged and sorted the same way. `None` when there's
/// nothing to show at all, same convention as `recap_range`.
pub fn today_recap(conn: &Connection, date: &str) -> Result<Option<DayRecap>> {
    let mut rows = day_rows(conn, date)?;
    rows.extend(carried_over_rows(conn, date)?);
    if rows.is_empty() {
        return Ok(None);
    }
    sort_rows(&mut rows);
    Ok(Some(DayRecap {
        date: date.to_string(),
        rows,
    }))
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

/// Renders one day's table as a column-aligned GFM table
/// (`status::render_aligned_table`), meant for a `.md` file attachment
/// rather than squeezed into Discord's 2000-char message cap - alignment
/// only reads right in a fixed-width viewer (a raw/opened .md file), not
/// chat text.
pub fn format_day_table_file(day: &DayRecap) -> String {
    let headers = ["Member", "Task", "Status", "Progress", "Blocker"];
    let rows: Vec<Vec<String>> = day
        .rows
        .iter()
        .map(|row| {
            let (glyph, label) = match &row.status {
                Some(status) => crate::status::status_glyph_label(status),
                None => ("❌", "no report yet".to_string()),
            };
            vec![
                sanitize_cell(&row.member),
                sanitize_cell(&row.task),
                format!("{glyph} {label}"),
                row.progress
                    .as_deref()
                    .map(sanitize_cell)
                    .unwrap_or_else(|| "—".to_string()),
                row.blocker
                    .as_deref()
                    .map(sanitize_cell)
                    .unwrap_or_else(|| "—".to_string()),
            ]
        })
        .collect();

    format!(
        "# {}\n\n{}",
        day.date,
        crate::status::render_aligned_table(&headers, &rows)
    )
}

/// Renders the full `/recap` report as `.md` file content: one
/// column-aligned day table (`format_day_table_file`) per day in `days`,
/// separated by a blank line - or `None` when `days` is empty, so the
/// caller can show a plain "no activity" message instead of an empty file.
pub fn format_recap_file(days: &[DayRecap]) -> Option<String> {
    if days.is_empty() {
        return None;
    }
    Some(
        days.iter()
            .map(format_day_table_file)
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
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

    // --- today_recap ---

    #[test]
    fn today_recap_includes_a_report_filed_today_on_a_carried_over_todo() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let todo = entries::insert_todo(&conn, "1", "2026-09-01", "Fix bug X", None, None).unwrap();
        entries::insert_update(
            &conn,
            "1",
            "2026-09-03",
            "Fix bug X",
            Some(todo),
            "in_progress",
            "still digging",
            None,
        )
        .unwrap();

        // day_rows(date) alone still misses it - the bug this guards against.
        assert!(day_rows(&conn, "2026-09-03").unwrap().is_empty());

        let got = today_recap(&conn, "2026-09-03").unwrap().unwrap();
        assert_eq!(
            got.rows,
            vec![RecapRow {
                member: "Alice".to_string(),
                task: "Fix bug X".to_string(),
                status: Some("in_progress".to_string()),
                progress: Some("still digging".to_string()),
                blocker: None,
            }]
        );
    }

    #[test]
    fn today_recap_does_not_duplicate_a_todo_reported_on_its_own_day() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        let todo = entries::insert_todo(&conn, "1", "2026-09-01", "Same day", None, None).unwrap();
        entries::insert_update(
            &conn,
            "1",
            "2026-09-01",
            "Same day",
            Some(todo),
            "in_progress",
            "working on it",
            None,
        )
        .unwrap();

        let got = today_recap(&conn, "2026-09-01").unwrap().unwrap();
        assert_eq!(got.rows.len(), 1);
    }

    #[test]
    fn today_recap_is_none_when_theres_nothing_at_all() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice", "lead");
        assert_eq!(today_recap(&conn, "2026-09-01").unwrap(), None);
    }

    // --- format_day_table_file ---

    #[test]
    fn format_day_table_file_pads_every_column_to_the_same_width() {
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

        let out = format_day_table_file(&day);
        let lines: Vec<&str> = out.lines().collect();
        // "# <date>", blank, header, separator, 2 rows.
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "# 2026-09-01");
        assert_eq!(lines[1], "");
        // Every table row (header, separator, and data) must line up
        // char-for-char - that's the whole point of a file attachment
        // over squeezing this into a chat message.
        let table_lines = &lines[2..];
        let width = table_lines[0].chars().count();
        for line in table_lines {
            assert_eq!(line.chars().count(), width, "misaligned line: {line:?}");
        }
        assert!(table_lines[0].contains("Member") && table_lines[0].contains("Blocker"));
        assert!(table_lines[1].starts_with("|-"));
        assert!(table_lines[2].contains("Alice") && table_lines[2].contains("⛔ blocked"));
        assert!(table_lines[3].contains("Budi") && table_lines[3].contains("❌ no report yet"));
    }

    #[test]
    fn format_day_table_file_sanitizes_pipes_and_newlines_in_cell_text() {
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

        let out = format_day_table_file(&day);
        assert!(out.contains("A / B"));
        assert!(out.contains("line one line two"));
        assert_eq!(out.lines().count(), 5);
    }

    #[test]
    fn format_recap_file_is_none_for_an_empty_range() {
        assert_eq!(format_recap_file(&[]), None);
    }

    #[test]
    fn format_recap_file_joins_one_table_per_day() {
        let days = vec![
            DayRecap {
                date: "2026-09-01".to_string(),
                rows: vec![RecapRow {
                    member: "Alice".to_string(),
                    task: "Ship it".to_string(),
                    status: Some("done".to_string()),
                    progress: Some("shipped".to_string()),
                    blocker: None,
                }],
            },
            DayRecap {
                date: "2026-09-02".to_string(),
                rows: vec![RecapRow {
                    member: "Budi".to_string(),
                    task: "Design audit".to_string(),
                    status: None,
                    progress: None,
                    blocker: None,
                }],
            },
        ];

        let out = format_recap_file(&days).unwrap();
        assert_eq!(
            out,
            format!(
                "{}\n\n{}",
                format_day_table_file(&days[0]),
                format_day_table_file(&days[1])
            )
        );
    }
}

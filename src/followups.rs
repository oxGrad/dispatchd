use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::cutoffs::{DayCutoffs, Rules, day_cutoffs};

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

const NOT_ON_LEAVE: &str =
    "discord_user_id NOT IN (SELECT discord_user_id FROM member_leave WHERE date = ?1)";

/// Members (not `viewer`, not on leave for `date`) with no `type = 'todo'`
/// row for `date` created before `todo_cutoff` (a UTC RFC3339 instant). One
/// todo before the cut-off is enough; later ones don't rescue a miss.
pub fn members_missing_todo(
    conn: &Connection,
    date: &str,
    todo_cutoff: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT discord_user_id FROM members
         WHERE role != 'viewer' AND {NOT_ON_LEAVE}
           AND discord_user_id NOT IN (
             SELECT discord_user_id FROM entries
             WHERE type = 'todo' AND date = ?1 AND created_at < ?2
         )"
    ))?;
    let ids = stmt
        .query_map(params![date, todo_cutoff], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Members (not `viewer`, not on leave) who posted a todo for `date` before
/// `progress_cutoff` that has no `/progress` report created before
/// `progress_cutoff` against it. A todo added after the cut-off isn't
/// judged (nothing to report on yet); a report after it doesn't count.
/// Someone with no todo isn't included - only the todo miss covers them.
/// Shared by the live `update_followup` nag and `record_missed`.
pub fn members_missing_update(
    conn: &Connection,
    date: &str,
    progress_cutoff: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT e.discord_user_id FROM entries e
         JOIN members m ON m.discord_user_id = e.discord_user_id
         WHERE m.role != 'viewer' AND e.type = 'todo' AND e.date = ?1
           AND e.created_at < ?2
           AND e.{NOT_ON_LEAVE}
           AND e.id NOT IN (
             SELECT todo_id FROM entries
             WHERE type = 'update' AND todo_id IS NOT NULL AND created_at < ?2)"
    ))?;
    let ids = stmt
        .query_map(params![date, progress_cutoff], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Members (not `viewer`, not on leave) with no `entries` row of any type
/// for `date` at all - the set the day summary's missing-submissions
/// message nags. Distinct from the two cut-off-aware queries above.
pub fn members_with_no_activity(conn: &Connection, date: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT discord_user_id FROM members
         WHERE role != 'viewer' AND {NOT_ON_LEAVE}
           AND discord_user_id NOT IN (
             SELECT discord_user_id FROM entries WHERE date = ?1
         )"
    ))?;
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
/// member per kind (skipping a kind `day` waives) - 'todo' from
/// `members_missing_todo` (no todo submitted before the cut-off), 'update' from `members_missing_update` (had a todo
/// today but left at least one without a `/progress` report against it -
/// the tighter, more actionable signal than "posted zero updates all
/// day", and one that correctly excludes a member with no todo at all,
/// since `members_missing_todo` already covers that case). A member
/// hitting both gets a row of each kind, so `/missed` can report on
/// either independently. Writes the day's `missed_recorded` marker in
/// the same transaction, so a day is never half-recorded or recorded
/// without its marker (which the ticker and `backfill_missed` check).
/// `INSERT OR IGNORE` throughout makes this idempotent - safe to call more
/// than once for the same date. Unlike `followups_sent`/`reminders_sent`,
/// nothing prunes `missed_submissions` - it's retained history, same as
/// `entries`.
pub fn record_missed(conn: &Connection, date: &str, day: &DayCutoffs) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    insert_misses(&tx, date, day, None)?;
    tx.execute(
        "INSERT OR IGNORE INTO reminders_sent (date, type) VALUES (?1, 'missed_recorded')",
        params![date],
    )?;
    tx.commit()?;
    Ok(())
}

/// Inserts `date`'s miss rows, skipping a waived kind, for everyone or
/// just `only`.
fn insert_misses(
    conn: &Connection,
    date: &str,
    day: &DayCutoffs,
    only: Option<&str>,
) -> Result<()> {
    let kinds = [
        (
            "todo",
            day.todo_waived,
            members_missing_todo(conn, date, &day.todo_cutoff)?,
        ),
        (
            "update",
            day.progress_waived,
            members_missing_update(conn, date, &day.progress_cutoff)?,
        ),
    ];
    for (kind, waived, ids) in kinds {
        if waived {
            continue;
        }
        for id in ids.iter().filter(|i| only.is_none_or(|o| o == i.as_str())) {
            conn.execute(
                "INSERT OR IGNORE INTO missed_submissions (date, discord_user_id, kind) VALUES (?1, ?2, ?3)",
                params![date, id, kind],
            )?;
        }
    }
    Ok(())
}

/// Re-judges one member's recorded days after their leave changed. Only
/// days with a `missed_recorded` marker are touched. On or after
/// `rules_since` the member's rows are deleted and re-derived under the
/// cut-off rules (waived kinds skipped). Before it (or with no
/// `rules_since`) the old end-of-day verdicts stand: rows are deleted only
/// if the member is on leave that date now, never inserted - so removing
/// leave can't re-judge an old day, and a leave removal on a day that
/// never had leave can't erase an old miss.
// Used by `/team leave` (Task 7).
#[allow(dead_code)]
pub fn rederive_member(
    conn: &Connection,
    discord_user_id: &str,
    dates: &[String],
    rules: &Rules,
    rules_since: Option<&str>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for date in dates {
        if !crate::reminders::already_sent(&tx, date, "missed_recorded")? {
            continue;
        }
        if rules_since.is_some_and(|since| date.as_str() >= since) {
            tx.execute(
                "DELETE FROM missed_submissions WHERE date = ?1 AND discord_user_id = ?2",
                params![date, discord_user_id],
            )?;
            let day = day_cutoffs(&tx, rules, date)?;
            insert_misses(&tx, date, &day, Some(discord_user_id))?;
        } else {
            tx.execute(
                "DELETE FROM missed_submissions WHERE date = ?1 AND discord_user_id = ?2
                 AND discord_user_id IN (SELECT discord_user_id FROM member_leave WHERE date = ?1)",
                params![date, discord_user_id],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// How far back `backfill_missed` looks. Must stay well under
/// `maintenance`'s 90-day `reminders_sent` retention, or a day whose
/// `missed_recorded` marker was pruned would be recorded a second time.
const MISSED_BACKFILL_DAYS: i64 = 14;

/// Records the miss snapshot for any past day in the last
/// `MISSED_BACKFILL_DAYS` the ticker never got to - the bot was down at
/// `day_summary_time` and only came back after midnight, so without this
/// `/missed` would silently report nobody missing anything that day. Only
/// days with at least one entry count: a day nobody submitted anything is
/// far more likely a holiday (or the bot, and with it the ritual, being
/// off) than the whole team missing it, and recording it would blame
/// everyone. Weekends are skipped unless `include_weekends`. Uses today's
/// roster, not the roster as it was on that day. Returns the dates
/// recorded.
pub fn backfill_missed(
    conn: &Connection,
    today: &str,
    include_weekends: bool,
    rules: &Rules,
) -> Result<Vec<String>> {
    use jiff::ToSpan;
    use jiff::civil::{Date, Weekday};

    let today_date = Date::strptime("%Y-%m-%d", today)?;
    let since = (today_date - MISSED_BACKFILL_DAYS.days()).to_string();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT date FROM entries
         WHERE date >= ?1 AND date < ?2
           AND date NOT IN (SELECT date FROM reminders_sent WHERE type = 'missed_recorded')
         ORDER BY date",
    )?;
    let dates = stmt
        .query_map(params![since, today], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut recorded = Vec::new();
    for date in dates {
        let weekday = Date::strptime("%Y-%m-%d", &date)?.weekday();
        if !include_weekends && matches!(weekday, Weekday::Saturday | Weekday::Sunday) {
            continue;
        }
        let day = day_cutoffs(conn, rules, &date)?;
        record_missed(conn, &date, &day)?;
        recorded.push(date);
    }
    Ok(recorded)
}

/// The `/missed` report's underlying data: one `MissedDetail` per member
/// with at least one miss in `[start, end]` (inclusive) per
/// `missed_submissions`, alphabetical by name (the summary table
/// `format_missed_report_file` renders below the detail re-sorts by day
/// count - this order is about finding a specific member's section
/// quickly).
/// `start`/`end` are trusted to already be validated `YYYY-MM-DD` strings
/// (see `recap::resolve_day_or_range`, reused by the command).
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
    use crate::leave;
    use jiff::Timestamp;

    /// A cut-off far in the future: every entry counts, which is what the
    /// pre-existing tests assume.
    const OPEN: &str = "9999-12-31T00:00:00Z";
    fn open_day() -> DayCutoffs {
        DayCutoffs {
            todo_cutoff: OPEN.into(),
            progress_cutoff: OPEN.into(),
            todo_waived: false,
            progress_waived: false,
        }
    }
    fn set_created_at(conn: &Connection, id: i64, ts: &str) {
        conn.execute(
            "UPDATE entries SET created_at = ?1 WHERE id = ?2",
            params![ts, id],
        )
        .unwrap();
    }
    fn seed_leave(conn: &Connection, id: &str, date: &str) {
        leave::add(conn, id, &[date.to_string()], leave::Reason::Sick).unwrap();
    }
    /// UTC rules (09:00/11:00/15:00/15:50, 120 s tolerance), no uptime.
    fn utc_rules() -> Rules {
        use jiff::civil::time;
        Rules {
            tz: jiff::tz::TimeZone::UTC,
            todo_time: time(9, 0, 0, 0),
            todo_cutoff: time(11, 0, 0, 0),
            update_time: time(15, 0, 0, 0),
            progress_cutoff: time(15, 50, 0, 0),
            tolerance: jiff::SignedDuration::from_secs(120),
        }
    }
    /// One uptime span from `from`'s midnight to `to`'s 23:59 UTC.
    fn cover(conn: &Connection, from: &str, to: &str) {
        let id =
            crate::cutoffs::start_span(conn, format!("{from}T00:00:00Z").parse().unwrap()).unwrap();
        let end: Timestamp = format!("{to}T23:59:00Z").parse().unwrap();
        crate::cutoffs::touch_span(conn, id, end).unwrap();
    }
    /// UTC rules plus one uptime span covering all of `date`.
    fn covered_utc_rules(conn: &Connection, date: &str) -> Rules {
        cover(conn, date, date);
        utc_rules()
    }
    /// Distinct `discord_user_id`s in `missed_submissions`, sorted.
    fn missed_ids(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT DISTINCT discord_user_id FROM missed_submissions ORDER BY 1")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

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

        let missing = members_missing_todo(&conn, DATE, OPEN).unwrap();
        assert_eq!(missing, vec!["2".to_string()]);
    }

    #[test]
    fn members_missing_todo_excludes_viewers() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        seed_viewer(&conn, "2", "Watcher");

        let missing = members_missing_todo(&conn, DATE, OPEN).unwrap();
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

        let missing = members_missing_update(&conn, DATE, OPEN).unwrap();
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
    fn backfill_missed_records_only_unrecorded_past_working_days_with_activity() {
        let conn = open_test_db();
        let rules = utc_rules();
        cover(&conn, "2026-09-10", "2026-10-02");
        seed_member(&conn, "1", "Alice");
        seed_member(&conn, "2", "Budi");
        for date in [
            "2026-09-10", // older than the backfill window
            "2026-09-27", // Sunday
            "2026-09-29", // Tuesday, never recorded - the one to backfill
            "2026-09-30", // Wednesday, already recorded below
            "2026-10-01", // today - the ticker's job, not backfill's
        ] {
            entries::insert_todo(&conn, "1", date, "a", None, None).unwrap();
        }
        // 2026-09-28 (Monday) has no entries at all - skipped as a likely holiday.
        record_missed(&conn, "2026-09-30", &open_day()).unwrap();
        // Alice posted her 09-29 todo and report before the cut-offs (the
        // inserts above stamp the real clock); Budi posted nothing.
        let alice_todo: i64 = conn
            .query_row(
                "SELECT id FROM entries WHERE discord_user_id = '1' AND date = '2026-09-29'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        set_created_at(&conn, alice_todo, "2026-09-29T08:00:00Z");
        let report = entries::insert_update(
            &conn,
            "1",
            "2026-09-29",
            "a",
            Some(alice_todo),
            "done",
            "x",
            None,
        )
        .unwrap();
        set_created_at(&conn, report, "2026-09-29T12:00:00Z");

        let recorded = backfill_missed(&conn, "2026-10-01", false, &rules).unwrap();
        assert_eq!(recorded, vec!["2026-09-29".to_string()]);
        assert!(crate::reminders::already_sent(&conn, "2026-09-29", "missed_recorded").unwrap());
        let missed: Vec<(String, String)> = conn
            .prepare(
                "SELECT discord_user_id, kind FROM missed_submissions
                 WHERE date = '2026-09-29' ORDER BY 1, 2",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(missed, vec![("2".to_string(), "todo".to_string())]);

        // Idempotent: nothing left to backfill. Weekends opt in.
        assert!(
            backfill_missed(&conn, "2026-10-01", false, &rules)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            backfill_missed(&conn, "2026-10-01", true, &rules).unwrap(),
            vec!["2026-09-27".to_string()]
        );
    }

    #[test]
    fn record_missed_writes_a_row_per_kind_for_each_missing_member() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // no todo at all - todo miss only
        seed_member(&conn, "2", "Budi"); // todo left with no update - both

        entries::insert_todo(&conn, "2", DATE, "b", None, None).unwrap();

        record_missed(&conn, DATE, &open_day()).unwrap();

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

        record_missed(&conn, DATE, &open_day()).unwrap();

        let detail = missed_detail(&conn, DATE, DATE).unwrap();
        assert_eq!(detail[0].missed_todo_dates, vec![DATE.to_string()]);
        assert!(detail[0].missed_update_dates.is_empty());
    }

    #[test]
    fn record_missed_is_idempotent() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();

        record_missed(&conn, DATE, &open_day()).unwrap();
        record_missed(&conn, DATE, &open_day()).unwrap();

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

        record_missed(&conn, DATE, &open_day()).unwrap();

        assert!(missed_detail(&conn, DATE, DATE).unwrap().is_empty());
    }

    #[test]
    fn missed_detail_lists_every_missed_todo_date_in_range_and_excludes_outside_days() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // no todo on any of the three days

        record_missed(&conn, "2026-08-28", &open_day()).unwrap();
        record_missed(&conn, "2026-08-29", &open_day()).unwrap();
        record_missed(&conn, "2026-08-30", &open_day()).unwrap();

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
            record_missed(&conn, date, &open_day()).unwrap();
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

        record_missed(&conn, DATE, &open_day()).unwrap();

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

    #[test]
    fn missing_todo_counts_a_todo_after_the_cutoff_as_missed() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        seed_member(&conn, "2", "B");
        let a = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
        set_created_at(&conn, a, "2026-08-29T10:59:00Z");
        let b = entries::insert_todo(&conn, "2", DATE, "y", None, None).unwrap();
        set_created_at(&conn, b, "2026-08-29T11:01:00Z");
        assert_eq!(
            members_missing_todo(&conn, DATE, "2026-08-29T11:00:00Z").unwrap(),
            vec!["2"]
        );
    }

    #[test]
    fn one_todo_before_the_cutoff_is_enough_even_with_a_later_one() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let a = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
        set_created_at(&conn, a, "2026-08-29T09:15:00Z");
        let b = entries::insert_todo(&conn, "1", DATE, "y", None, None).unwrap();
        set_created_at(&conn, b, "2026-08-29T13:00:00Z");
        assert!(
            members_missing_todo(&conn, DATE, "2026-08-29T11:00:00Z")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_todo_respects_a_non_utc_cutoff() {
        // Asia/Jakarta 11:00 = 04:00Z.
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        seed_member(&conn, "2", "B");
        let a = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
        set_created_at(&conn, a, "2026-08-29T03:59:00Z");
        let b = entries::insert_todo(&conn, "2", DATE, "y", None, None).unwrap();
        set_created_at(&conn, b, "2026-08-29T04:01:00Z");
        let tz = jiff::tz::TimeZone::get("Asia/Jakarta").unwrap();
        let cutoff = crate::cutoffs::instant(DATE, jiff::civil::time(11, 0, 0, 0), &tz).unwrap();
        assert_eq!(
            members_missing_todo(&conn, DATE, &cutoff.to_string()).unwrap(),
            vec!["2"]
        );
    }

    #[test]
    fn missing_update_ignores_todos_added_after_the_progress_cutoff() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let t = entries::insert_todo(&conn, "1", DATE, "tomorrow's plan", None, None).unwrap();
        set_created_at(&conn, t, "2026-08-29T16:10:00Z");
        assert!(
            members_missing_update(&conn, DATE, "2026-08-29T15:50:00Z")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_report_after_the_progress_cutoff_does_not_count() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let t = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
        set_created_at(&conn, t, "2026-08-29T09:00:00Z");
        let u = entries::insert_update(&conn, "1", DATE, "x", Some(t), "done", "ok", None).unwrap();
        set_created_at(&conn, u, "2026-08-29T15:55:00Z");
        assert_eq!(
            members_missing_update(&conn, DATE, "2026-08-29T15:50:00Z").unwrap(),
            vec!["1"]
        );
    }

    #[test]
    fn members_on_leave_are_excluded_everywhere() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        seed_leave(&conn, "1", DATE);
        assert!(members_missing_todo(&conn, DATE, OPEN).unwrap().is_empty());
        assert!(members_with_no_activity(&conn, DATE).unwrap().is_empty());
        entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
        assert!(
            members_missing_update(&conn, DATE, OPEN)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn record_missed_skips_a_waived_kind() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let day = DayCutoffs {
            todo_waived: true,
            ..open_day()
        };
        record_missed(&conn, DATE, &day).unwrap();
        let kinds: Vec<String> = conn
            .prepare("SELECT kind FROM missed_submissions")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(kinds.is_empty(), "{kinds:?}");
        assert!(crate::reminders::already_sent(&conn, DATE, "missed_recorded").unwrap());
    }

    #[test]
    fn record_missed_skips_a_waived_progress_kind() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let todo = entries::insert_todo(&conn, "1", DATE, "x", None, None).unwrap();
        set_created_at(&conn, todo, &format!("{DATE}T08:00:00Z"));
        let day = DayCutoffs {
            progress_cutoff: format!("{DATE}T15:50:00Z"),
            progress_waived: true,
            ..open_day()
        };
        record_missed(&conn, DATE, &day).unwrap();
        let updates: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM missed_submissions WHERE kind = 'update'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(updates, 0);
        assert!(crate::reminders::already_sent(&conn, DATE, "missed_recorded").unwrap());
    }

    #[test]
    fn backfill_records_nothing_for_a_day_the_bot_was_not_up() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        entries::insert_todo(&conn, "1", "2026-09-29", "x", None, None).unwrap();
        // No uptime coverage: both windows waived, but the day is still marked.
        let got = backfill_missed(&conn, "2026-10-01", false, &utc_rules()).unwrap();
        assert_eq!(got, vec!["2026-09-29"]);
        assert!(missed_ids(&conn).is_empty());
    }

    #[test]
    fn rederive_member_clears_then_restores_misses() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        seed_member(&conn, "2", "B");
        let rules = covered_utc_rules(&conn, DATE);
        record_missed(
            &conn,
            DATE,
            &crate::cutoffs::day_cutoffs(&conn, &rules, DATE).unwrap(),
        )
        .unwrap();
        seed_leave(&conn, "1", DATE);
        rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-01-01")).unwrap();
        assert_eq!(missed_ids(&conn), vec!["2"]);
        leave::remove(&conn, "1", &[DATE.into()]).unwrap();
        rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-01-01")).unwrap();
        assert_eq!(missed_ids(&conn), vec!["1", "2"]);
    }

    #[test]
    fn rederive_member_before_rules_since_only_clears_for_leave() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let rules = covered_utc_rules(&conn, DATE);
        record_missed(&conn, DATE, &open_day()).unwrap();
        // No leave: an old-rule miss is left alone.
        rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-09-01")).unwrap();
        assert_eq!(missed_ids(&conn), vec!["1"]);
        // Leave added: cleared. Removed again: not re-judged under the new rules.
        seed_leave(&conn, "1", DATE);
        rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-09-01")).unwrap();
        assert!(missed_ids(&conn).is_empty());
        leave::remove(&conn, "1", &[DATE.into()]).unwrap();
        rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-09-01")).unwrap();
        assert!(missed_ids(&conn).is_empty());
    }

    #[test]
    fn rederive_member_ignores_unrecorded_days() {
        let conn = open_test_db();
        seed_member(&conn, "1", "A");
        let rules = covered_utc_rules(&conn, DATE);
        rederive_member(&conn, "1", &[DATE.into()], &rules, Some("2026-01-01")).unwrap();
        assert!(missed_ids(&conn).is_empty());
        assert!(!crate::reminders::already_sent(&conn, DATE, "missed_recorded").unwrap());
    }
}

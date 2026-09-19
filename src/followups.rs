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
/// nagged by the todo follow-up.
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

/// Members with no `type = 'update'` row for `date` at all - i.e. posted
/// no `/progress` report whatsoever today, regardless of whether they had
/// a todo to report against. Distinct from `members_missing_update`
/// above, which only tracks a todo left specifically unmatched; this is
/// the whole-day miss `record_missed` below persists. Excludes role
/// `viewer`, same as `members_missing_todo`.
pub fn members_missing_any_update(conn: &Connection, date: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT discord_user_id FROM members
         WHERE role != 'viewer'
           AND discord_user_id NOT IN (
             SELECT discord_user_id FROM entries WHERE type = 'update' AND date = ?1
         )",
    )?;
    let ids = stmt
        .query_map(params![date], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// One row of the `/missed` report: how many days in a queried range a
/// member missed `/todo`, `/progress`, or both entirely. Built from
/// `missed_submissions` (see `record_missed` below), not computed live -
/// only members with at least one miss are returned by `missed_summary`,
/// a clean record isn't worth a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissedSummary {
    pub member: String,
    pub missed_todo_days: i64,
    pub missed_update_days: i64,
}

/// Records `date`'s miss snapshot into `missed_submissions`: one row per
/// member per kind ('todo' from `members_missing_todo`, 'update' from
/// `members_missing_any_update`) they submitted nothing for at all today.
/// A member missing both gets a row of each kind, so `/missed` can report
/// on either independently. `INSERT OR IGNORE` makes this idempotent -
/// safe to call more than once for the same date - though the ticker only
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
    for id in members_missing_any_update(conn, date)? {
        conn.execute(
            "INSERT OR IGNORE INTO missed_submissions (date, discord_user_id, kind) VALUES (?1, ?2, 'update')",
            params![date, id],
        )?;
    }
    Ok(())
}

/// The `/missed` report: every member with at least one missed `/todo` or
/// `/progress` day in `[start, end]` (inclusive) per `missed_submissions`,
/// most-missed-days first then by name. `start`/`end` are trusted to
/// already be validated `YYYY-MM-DD` strings (see `recap::resolve_range`,
/// reused by the command).
pub fn missed_summary(conn: &Connection, start: &str, end: &str) -> Result<Vec<MissedSummary>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(m.name, ms.discord_user_id) AS member,
                SUM(ms.kind = 'todo') AS missed_todo,
                SUM(ms.kind = 'update') AS missed_update
         FROM missed_submissions ms
         LEFT JOIN members m ON m.discord_user_id = ms.discord_user_id
         WHERE ms.date >= ?1 AND ms.date <= ?2
         GROUP BY ms.discord_user_id
         ORDER BY (SUM(ms.kind = 'todo') + SUM(ms.kind = 'update')) DESC, member ASC",
    )?;
    let rows = stmt
        .query_map(params![start, end], |row| {
            Ok(MissedSummary {
                member: row.get(0)?,
                missed_todo_days: row.get(1)?,
                missed_update_days: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Renders the `/missed` report body: a markdown table, one row per member
/// with at least one miss in `[start, end]`, or a plain "nothing missed"
/// line when `rows` is empty. No trailing newline - same convention as
/// `recap::format_day_table`, and callers can feed the result straight
/// into `status::split_into_messages` for Discord's 2000-char cap (though
/// a 6-person team's worth of rows is never going to need it).
pub fn format_missed_summary(rows: &[MissedSummary], start: &str, end: &str) -> String {
    if rows.is_empty() {
        return format!("✅ No missed submissions between {start} and {end}.");
    }
    let mut out = format!(
        "📉 **Missed submissions ({start} to {end})**\n| Member | Missed /todo | Missed /progress |\n|---|---|---|"
    );
    for row in rows {
        out.push_str(&format!(
            "\n| {} | {} | {} |",
            row.member.replace('|', "/"),
            row.missed_todo_days,
            row.missed_update_days
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
    fn members_missing_any_update_ignores_whether_a_todo_exists() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // todo, update posted
        seed_member(&conn, "2", "Budi"); // todo, no update at all
        seed_member(&conn, "3", "Citra"); // no todo, ad-hoc update posted

        let todo1 = entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();
        entries::insert_update(&conn, "1", DATE, "a", Some(todo1), "done", "x", None).unwrap();
        entries::insert_todo(&conn, "2", DATE, "b", None, None).unwrap();
        entries::insert_update(&conn, "3", DATE, "hotfix", None, "done", "shipped", None).unwrap();

        let missing = members_missing_any_update(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["2".to_string()]);
    }

    #[test]
    fn members_missing_any_update_excludes_viewers() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        seed_viewer(&conn, "2", "Watcher");

        let missing = members_missing_any_update(&conn, DATE).unwrap();
        assert_eq!(missing, vec!["1".to_string()]);
    }

    #[test]
    fn record_missed_writes_a_row_per_kind_for_each_missing_member() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice"); // misses both
        seed_member(&conn, "2", "Budi"); // todo only, misses update

        entries::insert_todo(&conn, "2", DATE, "b", None, None).unwrap();

        record_missed(&conn, DATE).unwrap();

        let summary = missed_summary(&conn, DATE, DATE).unwrap();
        assert_eq!(
            summary,
            vec![
                MissedSummary {
                    member: "Alice".to_string(),
                    missed_todo_days: 1,
                    missed_update_days: 1,
                },
                MissedSummary {
                    member: "Budi".to_string(),
                    missed_todo_days: 0,
                    missed_update_days: 1,
                },
            ]
        );
    }

    #[test]
    fn record_missed_is_idempotent() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");

        record_missed(&conn, DATE).unwrap();
        record_missed(&conn, DATE).unwrap();

        let summary = missed_summary(&conn, DATE, DATE).unwrap();
        assert_eq!(summary[0].missed_todo_days, 1);
        assert_eq!(summary[0].missed_update_days, 1);
    }

    #[test]
    fn missed_summary_is_empty_for_a_member_with_no_misses() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");
        entries::insert_todo(&conn, "1", DATE, "a", None, None).unwrap();
        entries::insert_update(&conn, "1", DATE, "a", None, "done", "x", None).unwrap();

        record_missed(&conn, DATE).unwrap();

        assert!(missed_summary(&conn, DATE, DATE).unwrap().is_empty());
    }

    #[test]
    fn missed_summary_aggregates_across_the_date_range_and_excludes_outside_days() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Alice");

        record_missed(&conn, "2026-08-28").unwrap();
        record_missed(&conn, "2026-08-29").unwrap();
        record_missed(&conn, "2026-08-30").unwrap();

        let summary = missed_summary(&conn, "2026-08-28", "2026-08-29").unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].missed_todo_days, 2);
        assert_eq!(summary[0].missed_update_days, 2);
    }

    #[test]
    fn missed_summary_orders_most_missed_days_first_then_by_name() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Zed"); // misses both kinds - 2 total
        seed_member(&conn, "2", "Alice"); // misses just todo - 1 total

        entries::insert_update(&conn, "2", DATE, "hotfix", None, "done", "x", None).unwrap();

        record_missed(&conn, DATE).unwrap();

        let summary = missed_summary(&conn, DATE, DATE).unwrap();
        let members: Vec<&str> = summary.iter().map(|s| s.member.as_str()).collect();
        assert_eq!(members, vec!["Zed", "Alice"]);
    }

    #[test]
    fn format_missed_summary_reports_nothing_missed_when_rows_are_empty() {
        assert_eq!(
            format_missed_summary(&[], "2026-08-01", "2026-08-14"),
            "✅ No missed submissions between 2026-08-01 and 2026-08-14."
        );
    }

    #[test]
    fn format_missed_summary_renders_a_row_per_member() {
        let rows = vec![
            MissedSummary {
                member: "Zed".to_string(),
                missed_todo_days: 2,
                missed_update_days: 2,
            },
            MissedSummary {
                member: "Alice".to_string(),
                missed_todo_days: 0,
                missed_update_days: 1,
            },
        ];
        assert_eq!(
            format_missed_summary(&rows, "2026-08-01", "2026-08-14"),
            "📉 **Missed submissions (2026-08-01 to 2026-08-14)**\n\
             | Member | Missed /todo | Missed /progress |\n\
             |---|---|---|\n\
             | Zed | 2 | 2 |\n\
             | Alice | 0 | 1 |"
        );
    }
}

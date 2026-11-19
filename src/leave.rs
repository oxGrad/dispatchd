//! Per-member leave/sick days (`member_leave`), serenity-free.

use anyhow::{Context, Result};
use jiff::civil::{Date, Weekday};
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Leave,
    Sick,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Leave => "leave",
            Reason::Sick => "sick",
        }
    }

    pub fn parse(s: &str) -> Option<Reason> {
        match s {
            "leave" => Some(Reason::Leave),
            "sick" => Some(Reason::Sick),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reason::Leave => "Leave",
            Reason::Sick => "Sick",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveDay {
    pub date: String,
    pub discord_user_id: String,
    pub name: String,
    pub reason: Reason,
}

/// Inclusive, ascending dates from `start` to `end`; Sat/Sun dropped unless
/// `include_weekends`.
pub fn ritual_days(start: &str, end: &str, include_weekends: bool) -> Result<Vec<String>> {
    let mut d: Date = start
        .parse()
        .with_context(|| format!("bad date {start:?}"))?;
    let end: Date = end.parse().with_context(|| format!("bad date {end:?}"))?;
    let mut out = Vec::new();
    while d <= end {
        if include_weekends || !matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday) {
            out.push(d.to_string());
        }
        d = d.tomorrow()?;
    }
    Ok(out)
}

/// Upserts one row per date; returns rows written. Not a transaction of its
/// own: `followups::change_leave` wraps it with the re-judge.
pub fn add(
    conn: &Connection,
    discord_user_id: &str,
    dates: &[String],
    reason: Reason,
) -> Result<usize> {
    let mut n = 0;
    for date in dates {
        n += conn.execute(
            "INSERT INTO member_leave (date, discord_user_id, reason) VALUES (?1, ?2, ?3)
             ON CONFLICT(date, discord_user_id) DO UPDATE SET reason = excluded.reason",
            params![date, discord_user_id, reason.as_str()],
        )?;
    }
    Ok(n)
}

/// Deletes the given dates for a member; returns the dates that actually had
/// a row, in `dates` order (ascending from `ritual_days`). Not a transaction
/// of its own, same as `add`.
pub fn remove(conn: &Connection, discord_user_id: &str, dates: &[String]) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for date in dates {
        if conn.execute(
            "DELETE FROM member_leave WHERE date = ?1 AND discord_user_id = ?2",
            params![date, discord_user_id],
        )? > 0
        {
            removed.push(date.clone());
        }
    }
    Ok(removed)
}

/// Leave rows in `[start, end]`, ordered by date then name.
pub fn list(conn: &Connection, start: &str, end: &str) -> Result<Vec<LeaveDay>> {
    let mut stmt = conn.prepare(
        "SELECT l.date, l.discord_user_id, COALESCE(m.name, l.discord_user_id), l.reason
         FROM member_leave l LEFT JOIN members m ON m.discord_user_id = l.discord_user_id
         WHERE l.date BETWEEN ?1 AND ?2
         ORDER BY l.date, COALESCE(m.name, l.discord_user_id)",
    )?;
    let rows = stmt.query_map([start, end], |r| {
        let reason: String = r.get(3)?;
        Ok(LeaveDay {
            date: r.get(0)?,
            discord_user_id: r.get(1)?,
            name: r.get(2)?,
            reason: Reason::parse(&reason).unwrap_or(Reason::Leave),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn ritual_days_skips_weekends_unless_included() {
        // 2026-11-20 is a Friday.
        assert_eq!(
            ritual_days("2026-11-20", "2026-11-23", false).unwrap(),
            vec!["2026-11-20", "2026-11-23"]
        );
        assert_eq!(
            ritual_days("2026-11-20", "2026-11-23", true).unwrap().len(),
            4
        );
    }

    #[test]
    fn add_upserts_the_reason_and_remove_deletes() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Bima");
        let days = vec!["2026-11-20".to_string(), "2026-11-23".to_string()];
        assert_eq!(add(&conn, "1", &days, Reason::Leave).unwrap(), 2);
        add(&conn, "1", &days[..1], Reason::Sick).unwrap();
        let rows = list(&conn, "2026-11-01", "2026-11-30").unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.date.as_str(), r.reason))
                .collect::<Vec<_>>(),
            vec![("2026-11-20", Reason::Sick), ("2026-11-23", Reason::Leave)]
        );
        assert_eq!(rows[0].name, "Bima");
        assert_eq!(remove(&conn, "1", &days).unwrap(), days);
        assert!(list(&conn, "2026-11-01", "2026-11-30").unwrap().is_empty());
    }

    #[test]
    fn remove_returns_only_the_dates_it_deleted() {
        let conn = open_test_db();
        seed_member(&conn, "1", "Bima");
        seed_member(&conn, "2", "Alya");
        let had = vec!["2026-11-18".to_string(), "2026-11-20".to_string()];
        add(&conn, "1", &had, Reason::Leave).unwrap();
        add(&conn, "2", &["2026-11-19".to_string()], Reason::Sick).unwrap();
        let range = ritual_days("2026-11-17", "2026-11-20", false).unwrap();
        assert_eq!(remove(&conn, "1", &range).unwrap(), had);
        assert!(remove(&conn, "1", &range).unwrap().is_empty());
        assert_eq!(list(&conn, "2026-11-01", "2026-11-30").unwrap().len(), 1);
    }
}

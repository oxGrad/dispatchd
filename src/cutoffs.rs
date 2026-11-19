//! Todo/progress cut-off instants, bot uptime spans, and the downtime
//! waiver derived from them.

use anyhow::Result;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct Rules {
    pub tz: TimeZone,
    pub todo_time: Time,
    pub todo_cutoff: Time,
    pub update_time: Time,
    pub progress_cutoff: Time,
    pub tolerance: SignedDuration,
}

impl Rules {
    pub fn from_config(c: &Config) -> Rules {
        Rules {
            tz: c.timezone.clone(),
            todo_time: c.todo_time,
            todo_cutoff: c.todo_cutoff_time,
            update_time: c.update_time,
            progress_cutoff: c.progress_cutoff_time,
            tolerance: SignedDuration::from_secs(2 * c.ticker_interval_seconds as i64),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayCutoffs {
    pub todo_cutoff: String,
    pub progress_cutoff: String,
    pub todo_waived: bool,
    pub progress_waived: bool,
}

/// The UTC instant of local `time` on `date` (YYYY-MM-DD) in `tz`.
pub fn instant(date: &str, time: Time, tz: &TimeZone) -> Result<Timestamp> {
    let dt = Date::strptime("%Y-%m-%d", date)?.to_datetime(time);
    Ok(tz.to_timestamp(dt)?)
}

/// Whether the spans cover all of `[lo, hi]`. Only a span's end gets
/// `tolerance` (a restart gap is prev.last_seen -> next.started_at), so a
/// span that starts inside the window leaves the start uncovered.
pub fn covered(
    spans: &[(Timestamp, Timestamp)],
    lo: Timestamp,
    hi: Timestamp,
    tolerance: SignedDuration,
) -> bool {
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|s| s.0);
    let mut cur = lo;
    for (start, last_seen) in sorted {
        if start > cur {
            return false;
        }
        if let Ok(end) = last_seen.checked_add(tolerance) {
            cur = cur.max(end);
        }
        if cur >= hi {
            return true;
        }
    }
    false
}

pub fn start_span(conn: &Connection, now: Timestamp) -> Result<i64> {
    let now = now.to_string();
    conn.execute(
        "INSERT INTO uptime_spans (started_at, last_seen) VALUES (?1, ?1)",
        params![now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn touch_span(conn: &Connection, id: i64, now: Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE uptime_spans SET last_seen = ?1 WHERE id = ?2",
        params![now.to_string(), id],
    )?;
    Ok(())
}

pub fn day_cutoffs(conn: &Connection, rules: &Rules, date: &str) -> Result<DayCutoffs> {
    let todo_lo = instant(date, rules.todo_time, &rules.tz)?;
    let todo_hi = instant(date, rules.todo_cutoff, &rules.tz)?;
    let prog_lo = instant(date, rules.update_time, &rules.tz)?;
    let prog_hi = instant(date, rules.progress_cutoff, &rules.tz)?;

    let limit = todo_hi.max(prog_hi).to_string();
    let mut stmt = conn.prepare(
        "SELECT started_at, last_seen FROM uptime_spans WHERE started_at < ?1 ORDER BY started_at",
    )?;
    let spans = stmt
        .query_map(params![limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(a, b)| Ok((a.parse::<Timestamp>()?, b.parse::<Timestamp>()?)))
        .collect::<Result<Vec<_>>>()?;

    Ok(DayCutoffs {
        todo_cutoff: todo_hi.to_string(),
        progress_cutoff: prog_hi.to_string(),
        todo_waived: !covered(&spans, todo_lo, todo_hi, rules.tolerance),
        progress_waived: !covered(&spans, prog_lo, prog_hi, rules.tolerance),
    })
}

const RULES_SINCE_KEY: &str = "cutoff_rules_since";

/// Sets (once) the first day the cut-off rules apply: today, or tomorrow if
/// today's misses were already recorded. Returns the stored value.
pub fn ensure_rules_since(conn: &Connection, today: &str) -> Result<String> {
    let start = if crate::reminders::already_sent(conn, today, "missed_recorded")? {
        Date::strptime("%Y-%m-%d", today)?.tomorrow()?.to_string()
    } else {
        today.to_string()
    };
    conn.execute(
        "INSERT OR IGNORE INTO app_meta (key, value) VALUES (?1, ?2)",
        params![RULES_SINCE_KEY, start],
    )?;
    Ok(rules_since(conn)?.expect("just inserted"))
}

pub fn rules_since(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            params![RULES_SINCE_KEY],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::time;

    fn open_test_db() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("d.sqlite3");
        crate::db::open(&path).unwrap()
    }

    fn utc_rules() -> Rules {
        Rules {
            tz: TimeZone::UTC,
            todo_time: time(9, 0, 0, 0),
            todo_cutoff: time(11, 0, 0, 0),
            update_time: time(15, 0, 0, 0),
            progress_cutoff: time(15, 50, 0, 0),
            tolerance: TOL,
        }
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }
    const TOL: SignedDuration = SignedDuration::from_secs(120);

    #[test]
    fn instant_converts_local_time_to_utc() {
        let tz = TimeZone::get("Asia/Jakarta").unwrap();
        assert_eq!(
            instant("2026-11-19", time(11, 0, 0, 0), &tz)
                .unwrap()
                .to_string(),
            "2026-11-19T04:00:00Z"
        );
    }

    #[test]
    fn covered_by_one_span_across_the_window() {
        let s = [(ts("2026-11-19T01:00:00Z"), ts("2026-11-19T05:00:00Z"))];
        assert!(covered(
            &s,
            ts("2026-11-19T02:00:00Z"),
            ts("2026-11-19T04:00:00Z"),
            TOL
        ));
    }

    #[test]
    fn a_restart_gap_within_tolerance_is_still_covered() {
        let s = [
            (ts("2026-11-19T01:00:00Z"), ts("2026-11-19T02:59:00Z")),
            (ts("2026-11-19T03:00:30Z"), ts("2026-11-19T05:00:00Z")),
        ];
        assert!(covered(
            &s,
            ts("2026-11-19T02:00:00Z"),
            ts("2026-11-19T04:00:00Z"),
            TOL
        ));
    }

    #[test]
    fn a_gap_longer_than_tolerance_is_not_covered() {
        let s = [
            (ts("2026-11-19T01:00:00Z"), ts("2026-11-19T02:30:00Z")),
            (ts("2026-11-19T03:30:00Z"), ts("2026-11-19T05:00:00Z")),
        ];
        assert!(!covered(
            &s,
            ts("2026-11-19T02:00:00Z"),
            ts("2026-11-19T04:00:00Z"),
            TOL
        ));
    }

    #[test]
    fn a_first_span_starting_inside_the_window_is_not_covered() {
        let s = [(ts("2026-11-19T03:00:00Z"), ts("2026-11-19T05:00:00Z"))];
        let (lo, hi) = (ts("2026-11-19T02:00:00Z"), ts("2026-11-19T04:00:00Z"));
        assert!(!covered(&s, lo, hi, TOL));
        assert!(!covered(&[], lo, hi, TOL));
    }

    #[test]
    fn day_cutoffs_waives_only_the_window_with_downtime() {
        // UTC rules: todo 09:00-11:00, progress 15:00-15:50; bot down 10:30-11:30.
        let conn = open_test_db();
        let a = start_span(&conn, ts("2026-11-19T08:00:00Z")).unwrap();
        touch_span(&conn, a, ts("2026-11-19T10:30:00Z")).unwrap();
        let b = start_span(&conn, ts("2026-11-19T11:30:00Z")).unwrap();
        touch_span(&conn, b, ts("2026-11-19T17:00:00Z")).unwrap();
        let d = day_cutoffs(&conn, &utc_rules(), "2026-11-19").unwrap();
        assert_eq!(
            d,
            DayCutoffs {
                todo_cutoff: "2026-11-19T11:00:00Z".into(),
                progress_cutoff: "2026-11-19T15:50:00Z".into(),
                todo_waived: true,
                progress_waived: false,
            }
        );
    }

    #[test]
    fn rules_since_is_set_once_and_skips_an_already_recorded_today() {
        let conn = open_test_db();
        crate::reminders::mark_sent(&conn, "2026-11-19", "missed_recorded").unwrap();
        assert_eq!(
            ensure_rules_since(&conn, "2026-11-19").unwrap(),
            "2026-11-20"
        );
        assert_eq!(
            ensure_rules_since(&conn, "2026-11-25").unwrap(),
            "2026-11-20"
        );
        assert_eq!(rules_since(&conn).unwrap().as_deref(), Some("2026-11-20"));
    }
}

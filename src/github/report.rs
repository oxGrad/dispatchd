//! The reports built on the synced GitHub data (design phase 3): the
//! day-summary headline and its extra sections, `/team github`'s
//! per-person breakdown, and `/recap`'s stakeholder-facing deliverables
//! summary. DB queries and pure formatting only - no serenity types - so
//! everything here is unit-tested against a real temp-file DB.
//!
//! GitHub stores UTC timestamps; every "today" / "in range" here means
//! calendar days in the configured timezone, turned into a UTC `Window`.
//! Activity is attributed to roster members only (matched on
//! `members.github_id`): bots and outside contributors never appear under
//! anyone, and never count toward team totals.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{Context, Result};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rusqlite::{Connection, OptionalExtension, params};

use crate::recap::{self, DayRecap, RecapRow, sanitize_cell, text_cell};
use crate::sow::{self, SowRef};

/// How far back a todo's linked issue still counts as "on a todo" for the
/// day summary's "GitHub work not on a todo" section - matches the
/// sync's backfill window.
const LINKED_TODO_LOOKBACK_DAYS: i64 = 14;

// ---------------------------------------------------------------------------
// Time windows
// ---------------------------------------------------------------------------

/// The UTC span `[lo, hi)` covering local calendar days `start..=end`, as
/// RFC3339 strings in GitHub's own `...Z` format, so stored timestamps
/// compare against them as plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub lo: String,
    pub hi: String,
}

impl Window {
    pub fn contains(&self, t: &str) -> bool {
        t >= self.lo.as_str() && t < self.hi.as_str()
    }

    fn contains_opt(&self, t: &Option<String>) -> bool {
        t.as_deref().is_some_and(|t| self.contains(t))
    }
}

fn local_midnight_utc(date: Date, tz: &TimeZone) -> Result<String> {
    // jiff's "compatible" rule: a midnight skipped by a DST gap (some zones)
    // becomes the first instant after the gap, a repeated one the earlier.
    let instant = date.to_zoned_datetime(tz.clone())?.timestamp();
    Ok(format!("{instant:.0}"))
}

/// `start..=end` (`YYYY-MM-DD`, local days in `tz`) as a UTC `Window`.
pub fn window(start: &str, end: &str, tz: &TimeZone) -> Result<Window> {
    let start = Date::strptime("%Y-%m-%d", start).context("invalid start date")?;
    let end = Date::strptime("%Y-%m-%d", end).context("invalid end date")?;
    Ok(Window {
        lo: local_midnight_utc(start, tz)?,
        hi: local_midnight_utc(end.tomorrow()?, tz)?,
    })
}

/// A GitHub UTC timestamp's local calendar day as `"Oct 14"`.
fn local_short_date(t: &str, tz: &TimeZone) -> String {
    t.parse::<Timestamp>()
        .map(|t| {
            t.to_zoned_datetime(tz.clone())
                .strftime("%b %-d")
                .to_string()
        })
        .unwrap_or_else(|_| t.to_string())
}

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------

/// `repo#number`, how every issue/PR is written in reports (plain text,
/// not a link - link syntax would wreck the column alignment).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemRef {
    pub repo: String,
    pub number: i64,
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

#[derive(Debug, Clone)]
struct RosterMember {
    name: String,
    github_user: Option<String>,
    github_id: Option<i64>,
}

/// Everyone the reports attribute work to: the roster minus `viewer`s
/// (they watch, they don't work the standup), sorted by name - never by
/// volume.
fn roster(conn: &Connection) -> Result<Vec<RosterMember>> {
    let mut stmt = conn.prepare(
        "SELECT name, github_user, github_id FROM members
         WHERE role != 'viewer'
         ORDER BY name COLLATE NOCASE, discord_user_id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RosterMember {
                name: row.get(0)?,
                github_user: row.get(1)?,
                github_id: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `"D.P.1.2 ×3, D.P.1.3"` for a list of (normalised) SOW refs, ordered
/// by milestone then deliverable; `None` when there are no refs.
fn sow_summary(refs: &[String]) -> Option<String> {
    if refs.is_empty() {
        return None;
    }
    let mut counts: BTreeMap<(Option<SowRef>, String), usize> = BTreeMap::new();
    for r in refs {
        *counts.entry((sow::parse(r), r.clone())).or_default() += 1;
    }
    Some(
        counts
            .into_iter()
            .map(|((_, r), n)| if n > 1 { format!("{r} ×{n}") } else { r })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn pr_state_label(state: &str, is_draft: bool) -> &str {
    match (state, is_draft) {
        ("open", true) => "draft",
        (state, _) => state,
    }
}

fn review_state_label(state: &str) -> String {
    state.replace('_', " ")
}

/// Roster index by resolved GitHub id - how activity rows find their member.
fn index_by_github_id(roster: &[RosterMember]) -> BTreeMap<i64, usize> {
    roster
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.github_id.map(|id| (id, i)))
        .collect()
}

/// The SOW ref an issue belongs to: its top-level ancestor's (a sub-issue
/// takes its deliverable's - in a deliverable -> task -> sub-task chain,
/// the top one's), its own when it's top-level. `None` when the chain's
/// top isn't stored. `UNION` (not `ALL`) ends the walk on a cycle.
fn issue_sow(conn: &Connection, node_id: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "WITH RECURSIVE chain(parent_node_id, sow_ref) AS (
                 SELECT parent_node_id, sow_ref FROM github_issues WHERE node_id = ?1
                 UNION
                 SELECT i.parent_node_id, i.sow_ref FROM github_issues i
                 JOIN chain ON i.node_id = chain.parent_node_id
             )
             SELECT sow_ref FROM chain WHERE parent_node_id IS NULL",
        )?
        .query_row([node_id], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten())
}

// ---------------------------------------------------------------------------
// Per-person activity: /team github and the day-summary headline
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubIssueClosed {
    pub item: ItemRef,
    /// The parent's SOW ref, normalised.
    pub sow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemberActivity {
    pub name: String,
    /// `github_user` set in members.toml.
    pub linked: bool,
    /// `github_user` set but not yet resolved to an id by the sync.
    pub unresolved: bool,
    pub prs_merged: Vec<ItemRef>,
    pub prs_opened: Vec<ItemRef>,
    pub sub_issues_closed: Vec<SubIssueClosed>,
    pub issues_opened: Vec<ItemRef>,
    /// Distinct PRs reviewed (not review submissions).
    pub reviewed: Vec<ItemRef>,
}

impl MemberActivity {
    fn is_empty(&self) -> bool {
        self.prs_merged.is_empty()
            && self.prs_opened.is_empty()
            && self.sub_issues_closed.is_empty()
            && self.issues_opened.is_empty()
            && self.reviewed.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TeamTotals {
    pub prs_merged: usize,
    /// One entry per distinct sub-issue closed: its SOW ref, if any.
    pub sub_issue_sows: Vec<Option<String>>,
    /// Distinct (reviewer, PR) pairs.
    pub reviews: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TeamActivity {
    pub members: Vec<MemberActivity>,
    pub totals: TeamTotals,
}

/// Who did what on GitHub in `w`, per the design's attribution rules -
/// credit goes to whoever did the work, never whoever clicked the button:
/// - PR merged / opened: the PR's author (opened = created in `w` and
///   still open at its end);
/// - sub-issue closed (as completed, in `w`): the authors of the merged
///   PRs linked to it, else its assignees; the SOW ref is the parent's;
/// - issue opened: its author;
/// - review: the reviewer, once per PR however many rounds.
pub fn team_activity(conn: &Connection, w: &Window) -> Result<TeamActivity> {
    let roster = roster(conn)?;
    let mut members: Vec<MemberActivity> = roster
        .iter()
        .map(|m| MemberActivity {
            name: m.name.clone(),
            linked: m.github_user.is_some(),
            unresolved: m.github_user.is_some() && m.github_id.is_none(),
            ..MemberActivity::default()
        })
        .collect();
    let by_id = index_by_github_id(&roster);
    let mut totals = TeamTotals::default();

    let mut stmt = conn.prepare(
        "SELECT repo, number, author_id, created_at, merged_at, closed_at FROM github_prs
         WHERE author_id IS NOT NULL
           AND ((merged_at >= ?1 AND merged_at < ?2) OR (created_at >= ?1 AND created_at < ?2))
         ORDER BY repo, number",
    )?;
    let prs = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                ItemRef {
                    repo: row.get(0)?,
                    number: row.get(1)?,
                },
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (item, author, created, merged, closed) in prs {
        let Some(&i) = by_id.get(&author) else {
            continue;
        };
        if w.contains_opt(&merged) {
            members[i].prs_merged.push(item);
            totals.prs_merged += 1;
        } else if w.contains(&created)
            && merged.as_deref().is_none_or(|t| t >= w.hi.as_str())
            && closed.as_deref().is_none_or(|t| t >= w.hi.as_str())
        {
            members[i].prs_opened.push(item);
        }
    }

    let mut stmt = conn.prepare(
        "SELECT i.node_id, i.repo, i.number FROM github_issues i
         JOIN github_issues parent ON parent.node_id = i.parent_node_id
         WHERE i.state = 'closed' AND COALESCE(i.state_reason, 'completed') = 'completed'
           AND i.closed_at >= ?1 AND i.closed_at < ?2
         ORDER BY i.repo, i.number",
    )?;
    let closed = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ItemRef {
                    repo: row.get(1)?,
                    number: row.get(2)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Every merged linked PR's author, `None` for a deleted account: a
    // merged PR by a bot or an outsider still means the assignee didn't
    // do the work, so only "no merged PR at all" falls back to assignees.
    let mut pr_authors = conn.prepare(
        "SELECT p.author_id FROM github_pr_issues pi
         JOIN github_prs p ON p.node_id = pi.pr_node_id
         WHERE pi.issue_node_id = ?1 AND p.state = 'merged'",
    )?;
    let mut assignees =
        conn.prepare("SELECT github_id FROM github_issue_assignees WHERE issue_node_id = ?1")?;
    for (node_id, item) in closed {
        let authors: Vec<Option<i64>> = pr_authors
            .query_map([&node_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let workers: Vec<i64> = if authors.is_empty() {
            assignees
                .query_map([&node_id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        } else {
            authors.into_iter().flatten().collect()
        };
        let credited: BTreeSet<usize> = workers
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        if credited.is_empty() {
            continue;
        }
        let sow_ref = issue_sow(conn, &node_id)?;
        for i in credited {
            members[i].sub_issues_closed.push(SubIssueClosed {
                item: item.clone(),
                sow: sow_ref.clone(),
            });
        }
        totals.sub_issue_sows.push(sow_ref);
    }

    let mut stmt = conn.prepare(
        "SELECT repo, number, author_id FROM github_issues
         WHERE author_id IS NOT NULL AND created_at >= ?1 AND created_at < ?2
         ORDER BY repo, number",
    )?;
    let opened = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                ItemRef {
                    repo: row.get(0)?,
                    number: row.get(1)?,
                },
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (item, author) in opened {
        if let Some(&i) = by_id.get(&author) {
            members[i].issues_opened.push(item);
        }
    }

    for (i, reviewed) in reviews_by_member(conn, w, &by_id)? {
        totals.reviews += reviewed.len();
        members[i].reviewed = reviewed.into_keys().collect();
    }

    Ok(TeamActivity { members, totals })
}

/// Per roster member (index), each PR they reviewed in `w` with their
/// latest review state on it. A PR author's own "reviews" (GitHub files a
/// `commented` one each time they reply to a review thread) aren't reviews.
fn reviews_by_member(
    conn: &Connection,
    w: &Window,
    by_id: &BTreeMap<i64, usize>,
) -> Result<BTreeMap<usize, BTreeMap<ItemRef, String>>> {
    let mut stmt = conn.prepare(
        "SELECT p.repo, p.number, r.author_id, r.state FROM github_reviews r
         JOIN github_prs p ON p.node_id = r.pr_node_id
         WHERE r.author_id IS NOT NULL AND r.submitted_at >= ?1 AND r.submitted_at < ?2
           AND r.author_id IS NOT p.author_id
         ORDER BY r.submitted_at",
    )?;
    let rows = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                ItemRef {
                    repo: row.get(0)?,
                    number: row.get(1)?,
                },
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: BTreeMap<usize, BTreeMap<ItemRef, String>> = BTreeMap::new();
    for (item, author, state) in rows {
        if let Some(&i) = by_id.get(&author) {
            // Ordered by submission time, so the last write is the latest.
            out.entry(i).or_default().insert(item, state);
        }
    }
    Ok(out)
}

/// The team totals line: `"3 PRs merged · 1 sub-issue closed (D.P.1.2) ·
/// 4 reviews"`, each segment only when non-zero; `None` when all are zero.
pub fn team_line(t: &TeamTotals) -> Option<String> {
    let mut segments = Vec::new();
    if t.prs_merged > 0 {
        segments.push(plural(t.prs_merged, "PR merged", "PRs merged"));
    }
    if !t.sub_issue_sows.is_empty() {
        let mut seg = plural(
            t.sub_issue_sows.len(),
            "sub-issue closed",
            "sub-issues closed",
        );
        let sows: Vec<String> = t.sub_issue_sows.iter().flatten().cloned().collect();
        if let Some(summary) = sow_summary(&sows) {
            seg.push_str(&format!(" ({summary})"));
        }
        segments.push(seg);
    }
    if t.reviews > 0 {
        segments.push(plural(t.reviews, "review", "reviews"));
    }
    (!segments.is_empty()).then(|| segments.join(" · "))
}

/// The day summary's headline line; `None` on a day with no team GitHub
/// activity (the line is simply left out).
pub fn headline(t: &TeamTotals) -> Option<String> {
    team_line(t).map(|line| format!("GitHub: {line}"))
}

fn items(list: &[ItemRef]) -> String {
    list.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One member's line. A single day lists the items; a range only counts
/// them (item lists would blow the message size - `/recap` has the detail).
fn member_line(m: &MemberActivity, single_day: bool) -> String {
    if !m.linked {
        return format!("**{}** — no GitHub account linked", m.name);
    }
    if m.unresolved {
        return format!(
            "**{}** — GitHub account not resolved yet (next sync)",
            m.name
        );
    }
    if m.is_empty() {
        return format!("**{}** — no GitHub activity", m.name);
    }
    let with_items = |count: String, list: &[ItemRef]| {
        if single_day {
            format!("{count} ({})", items(list))
        } else {
            count
        }
    };
    let mut segments = Vec::new();
    if !m.prs_merged.is_empty() {
        segments.push(with_items(
            plural(m.prs_merged.len(), "PR merged", "PRs merged"),
            &m.prs_merged,
        ));
    }
    if !m.sub_issues_closed.is_empty() {
        let mut seg = plural(
            m.sub_issues_closed.len(),
            "sub-issue closed",
            "sub-issues closed",
        );
        let sows: Vec<String> = m
            .sub_issues_closed
            .iter()
            .filter_map(|s| s.sow.clone())
            .collect();
        let mut detail = Vec::new();
        if single_day {
            detail.push(items(
                &m.sub_issues_closed
                    .iter()
                    .map(|s| s.item.clone())
                    .collect::<Vec<_>>(),
            ));
        }
        detail.extend(sow_summary(&sows));
        if !detail.is_empty() {
            seg.push_str(&format!(" ({})", detail.join(", ")));
        }
        segments.push(seg);
    }
    if !m.prs_opened.is_empty() {
        segments.push(with_items(
            plural(m.prs_opened.len(), "PR opened", "PRs opened"),
            &m.prs_opened,
        ));
    }
    if !m.issues_opened.is_empty() {
        segments.push(with_items(
            plural(m.issues_opened.len(), "issue opened", "issues opened"),
            &m.issues_opened,
        ));
    }
    if !m.reviewed.is_empty() {
        segments.push(plural(m.reviewed.len(), "review", "reviews"));
    }
    format!("**{}** — {}", m.name, segments.join(" · "))
}

/// `/team github`'s whole reply: header, team line, one line per member.
pub fn format_team_github(a: &TeamActivity, label: &str, single_day: bool) -> String {
    let mut out = format!(
        "🐙 **GitHub — {label}**\nTeam: {}\n",
        team_line(&a.totals).unwrap_or_else(|| "no GitHub activity".to_string())
    );
    if a.members.is_empty() {
        out.push_str("\nNo team members configured yet - see members.toml.");
        return out;
    }
    out.push('\n');
    out.push_str(
        &a.members
            .iter()
            .map(|m| member_line(m, single_day))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out
}

// ---------------------------------------------------------------------------
// Day sections: "GitHub work not on a todo" and "Code reviews"
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnplannedItem {
    pub author: String,
    /// `"PR api#224 Bump serde"` / `"Issue api#230 Webhook signature"`.
    pub item: String,
    pub sow: Option<String>,
    /// `"merged"` | `"opened"` | `"closed"`.
    pub today: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLine {
    pub reviewer: String,
    /// Each PR with the reviewer's latest review state that day.
    pub reviewed: Vec<(ItemRef, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DayGithub {
    pub unplanned: Vec<UnplannedItem>,
    pub reviews: Vec<ReviewLine>,
}

impl DayGithub {
    pub fn is_empty(&self) -> bool {
        self.unplanned.is_empty() && self.reviews.is_empty()
    }
}

/// Issues and PRs already "on a todo" as of `date`: issues todos from the
/// last `LINKED_TODO_LOOKBACK_DAYS` were created from, the PRs closing
/// them, and PRs that day's reports were filed from.
fn linked_to_todos(conn: &Connection, date: &str) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut stmt = conn.prepare(
        "SELECT github_issue_id FROM entries
         WHERE type = 'todo' AND github_issue_id IS NOT NULL
           AND date >= date(?1, ?2) AND date <= ?1",
    )?;
    let issues: BTreeSet<String> = stmt
        .query_map(
            params![date, format!("-{LINKED_TODO_LOOKBACK_DAYS} days")],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut prs: BTreeSet<String> = conn
        .prepare(
            "SELECT github_pr_id FROM entries
             WHERE type = 'update' AND github_pr_id IS NOT NULL AND date = ?1",
        )?
        .query_map([date], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut closing =
        conn.prepare("SELECT pr_node_id FROM github_pr_issues WHERE issue_node_id = ?1")?;
    for issue in &issues {
        let ids: Vec<String> = closing
            .query_map([issue], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        prs.extend(ids);
    }
    Ok((issues, prs))
}

/// The two extra day-summary sections for local day `date` (`w` is that
/// day's window): roster members' PRs merged/opened and issues
/// opened/closed that aren't on a todo, and who reviewed what.
pub fn day_github(conn: &Connection, date: &str, w: &Window) -> Result<DayGithub> {
    let roster = roster(conn)?;
    let by_id = index_by_github_id(&roster);
    let (linked_issues, linked_prs) = linked_to_todos(conn, date)?;
    // Keyed by roster index, so the section sorts in roster (name) order.
    let mut unplanned: Vec<(usize, UnplannedItem)> = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT node_id, repo, number, title, author_id, created_at, merged_at, closed_at
         FROM github_prs
         WHERE author_id IS NOT NULL
           AND ((merged_at >= ?1 AND merged_at < ?2) OR (created_at >= ?1 AND created_at < ?2))",
    )?;
    let prs = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ItemRef {
                    repo: row.get(1)?,
                    number: row.get(2)?,
                },
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut pr_issues = conn.prepare(
        "SELECT issue_node_id FROM github_pr_issues WHERE pr_node_id = ?1 ORDER BY issue_node_id",
    )?;
    for (node_id, item, title, author, created, merged, closed) in prs {
        let Some(&i) = by_id.get(&author) else {
            continue;
        };
        // Also covers a PR closing a linked issue: linked_to_todos adds those.
        if linked_prs.contains(&node_id) {
            continue;
        }
        let closes: Vec<String> = pr_issues
            .query_map([&node_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut sow = None;
        for issue in &closes {
            if let Some(found) = issue_sow(conn, issue)? {
                sow = Some(found);
                break;
            }
        }
        // "opened" as team_activity counts it: still open at the day's end,
        // so a PR abandoned the day it was opened isn't listed.
        let today = if w.contains_opt(&merged) {
            "merged"
        } else if w.contains(&created) && closed.as_deref().is_none_or(|t| t >= w.hi.as_str()) {
            "opened"
        } else {
            continue;
        };
        unplanned.push((
            i,
            UnplannedItem {
                author: roster[i].name.clone(),
                item: format!("PR {item} {title}"),
                sow,
                today,
            },
        ));
    }

    // `closed` only for a close as completed, like every other report here:
    // a not-planned / duplicate close isn't work done.
    let mut stmt = conn.prepare(
        "SELECT node_id, repo, number, title, author_id, created_at,
                CASE WHEN state = 'closed' AND COALESCE(state_reason, 'completed') = 'completed'
                     THEN closed_at END
         FROM github_issues
         WHERE author_id IS NOT NULL
           AND ((created_at >= ?1 AND created_at < ?2) OR (closed_at >= ?1 AND closed_at < ?2))",
    )?;
    let issues = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ItemRef {
                    repo: row.get(1)?,
                    number: row.get(2)?,
                },
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (node_id, item, title, author, created, closed) in issues {
        let Some(&i) = by_id.get(&author) else {
            continue;
        };
        if linked_issues.contains(&node_id) {
            continue;
        }
        let today = if w.contains_opt(&closed) {
            "closed"
        } else if w.contains(&created) {
            "opened"
        } else {
            continue;
        };
        unplanned.push((
            i,
            UnplannedItem {
                author: roster[i].name.clone(),
                item: format!("Issue {item} {title}"),
                sow: issue_sow(conn, &node_id)?,
                today,
            },
        ));
    }
    unplanned.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.item.cmp(&y.item)));
    let unplanned = unplanned.into_iter().map(|(_, u)| u).collect();

    let reviews = reviews_by_member(conn, w, &by_id)?
        .into_iter()
        .map(|(i, reviewed)| ReviewLine {
            reviewer: roster[i].name.clone(),
            reviewed: reviewed.into_iter().collect(),
        })
        .collect();

    Ok(DayGithub { unplanned, reviews })
}

fn heading(level: usize, text: &str) -> String {
    format!("{} {text}", "#".repeat(level))
}

/// The day's two GitHub sections as markdown at heading `level`, each left
/// out when empty; an empty string when both are.
pub fn format_day_sections(d: &DayGithub, level: usize) -> String {
    let mut parts = Vec::new();
    if !d.unplanned.is_empty() {
        let rows: Vec<Vec<String>> = d
            .unplanned
            .iter()
            .map(|u| {
                vec![
                    sanitize_cell(&u.author),
                    sanitize_cell(&u.item),
                    text_cell(u.sow.as_deref()),
                    u.today.to_string(),
                ]
            })
            .collect();
        parts.push(format!(
            "{}\n\n{}",
            heading(level, "GitHub work not on a todo"),
            crate::status::render_aligned_table(&["Author", "Item", "SOW", "Today"], &rows)
        ));
    }
    if !d.reviews.is_empty() {
        let rows: Vec<Vec<String>> = d
            .reviews
            .iter()
            .map(|r| {
                vec![
                    sanitize_cell(&r.reviewer),
                    r.reviewed
                        .iter()
                        .map(|(item, state)| format!("{item} ({})", review_state_label(state)))
                        .collect::<Vec<_>>()
                        .join(", "),
                ]
            })
            .collect();
        parts.push(format!(
            "{}\n\n{}",
            heading(level, "Code reviews"),
            crate::status::render_aligned_table(&["Reviewer", "Reviewed"], &rows)
        ));
    }
    parts.join("\n\n")
}

/// The `GitHub` cell for each standup row: a todo's issue plus every PR
/// linked to it with its state (`api#205 · PR api#219 open`), or an
/// unplanned report's PR (`PR web#222 merged`), else `—`. Until `/todo add
/// issue:` (design phase 4) sets these links, every cell is `—`.
pub fn github_cells(conn: &Connection, rows: &[RecapRow]) -> Result<Vec<String>> {
    let mut issue_stmt =
        conn.prepare("SELECT repo, number FROM github_issues WHERE node_id = ?1")?;
    let mut pr_stmt =
        conn.prepare("SELECT repo, number, state, is_draft FROM github_prs WHERE node_id = ?1")?;
    let mut closing_stmt = conn.prepare(
        "SELECT p.repo, p.number, p.state, p.is_draft FROM github_pr_issues pi
         JOIN github_prs p ON p.node_id = pi.pr_node_id
         WHERE pi.issue_node_id = ?1 ORDER BY p.repo, p.number",
    )?;
    let pr_label = |row: &rusqlite::Row| -> rusqlite::Result<String> {
        let item = ItemRef {
            repo: row.get(0)?,
            number: row.get(1)?,
        };
        let state: String = row.get(2)?;
        let is_draft: bool = row.get(3)?;
        Ok(format!("PR {item} {}", pr_state_label(&state, is_draft)))
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut parts = Vec::new();
        if let Some(issue) = &row.github_issue_id {
            let item = issue_stmt
                .query_row([issue], |r| {
                    Ok(ItemRef {
                        repo: r.get(0)?,
                        number: r.get(1)?,
                    })
                })
                .optional()?;
            if let Some(item) = item {
                parts.push(item.to_string());
                let prs: Vec<String> = closing_stmt
                    .query_map([issue], pr_label)?
                    .collect::<rusqlite::Result<_>>()?;
                parts.extend(prs);
            }
        }
        let pr = row
            .github_pr_id
            .as_ref()
            .map(|pr| pr_stmt.query_row([pr], pr_label).optional())
            .transpose()?
            .flatten();
        parts.extend(pr);
        out.push(if parts.is_empty() {
            "—".to_string()
        } else {
            parts.join(" · ")
        });
    }
    Ok(out)
}

/// One day as the GitHub-era report renders it: `date` at heading
/// `level`, then a `Standup` table (SOW + GitHub columns) and the two
/// GitHub sections one level down. Used for the day summary (level 1) and
/// `/recap`'s member detail (level 3).
pub fn format_github_day(
    conn: &Connection,
    date: &str,
    day: Option<&DayRecap>,
    gh: &DayGithub,
    level: usize,
) -> Result<String> {
    let standup = match day {
        Some(day) if !day.rows.is_empty() => {
            recap::format_standup_table(&day.rows, &github_cells(conn, &day.rows)?)
        }
        _ => "No `/todo` or `/progress` submissions.".to_string(),
    };
    let mut out = format!(
        "{}\n\n{}\n\n{standup}",
        heading(level, date),
        heading(level + 1, "Standup")
    );
    let sections = format_day_sections(gh, level + 1);
    if !sections.is_empty() {
        out.push_str("\n\n");
        out.push_str(&sections);
    }
    Ok(out)
}

/// The day summary with GitHub configured: the headline line (when the
/// team did anything on GitHub that day) and the attachment (when there's
/// any standup or GitHub activity at all).
pub fn day_summary(
    conn: &Connection,
    date: &str,
    tz: &TimeZone,
    day: Option<&DayRecap>,
) -> Result<(Option<String>, Option<String>)> {
    let w = window(date, date, tz)?;
    let headline = headline(&team_activity(conn, &w)?.totals);
    let gh = day_github(conn, date, &w)?;
    if day.is_none() && gh.is_empty() {
        return Ok((headline, None));
    }
    Ok((headline, Some(format_github_day(conn, date, day, &gh, 1)?)))
}

// ---------------------------------------------------------------------------
// /recap: deliverables
// ---------------------------------------------------------------------------

/// What a deliverables-table row is keyed on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeliverableKey {
    /// A parsed SOW ref (`D.P.1.2`, or a legacy `M1D2` todo ref).
    Sow(SowRef),
    /// A todo's free-text SOW ref that doesn't parse; grouped as typed.
    Raw(String),
    /// Work with no SOW ref at all - the `—` row, always last.
    Unlinked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubIssueState {
    Blocked,
    NotStarted,
    InProgress,
    /// A linked todo was reported done, but the issue itself is open.
    ReportedDone,
    Closed,
}

impl SubIssueState {
    fn label(self) -> &'static str {
        match self {
            SubIssueState::Blocked => "⛔ blocked",
            SubIssueState::NotStarted => "⬜ not started",
            SubIssueState::InProgress => "⏳ in progress",
            SubIssueState::ReportedDone => "✅ reported done",
            SubIssueState::Closed => "✅ closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubIssueRow {
    pub item: ItemRef,
    pub title: String,
    pub state: SubIssueState,
    /// `"api#219 open"`, ...
    pub prs: Vec<String>,
    pub last_activity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deliverable {
    pub key: DeliverableKey,
    pub title: String,
    /// `(closed as of the range end, total, closed within the range)`;
    /// `None` when there's no GitHub parent issue for the ref.
    pub sub_issues: Option<(usize, usize, usize)>,
    pub prs_merged: Option<usize>,
    /// `(done, total)` of in-range todos carrying this ref.
    pub todos: (usize, usize),
    pub breakdown: Vec<SubIssueRow>,
    pub closed_before: Vec<ItemRef>,
}

struct Child {
    node_id: String,
    item: ItemRef,
    title: String,
    state: String,
    state_reason: Option<String>,
    created_at: String,
    closed_at: Option<String>,
    updated_at: String,
}

struct LinkedPr {
    item: ItemRef,
    state: String,
    is_draft: bool,
    created_at: String,
    merged_at: Option<String>,
    closed_at: Option<String>,
    updated_at: String,
}

impl LinkedPr {
    /// When it was merged or closed; `None` while it's open.
    fn finished(&self) -> Option<&str> {
        self.merged_at
            .as_deref()
            .or(self.closed_at.as_deref())
            .filter(|_| self.state != "open")
    }
}

/// Whether an issue/PR may have been worked on in `w`. Only its latest
/// `updated_at` is stored, so one touched after the range too can't be
/// told apart from one idle during it: anything that existed before the
/// range ended, wasn't finished before it began and has been updated since
/// it began counts (a merge or close inside the range always does).
fn touched_in(w: &Window, created: &str, updated: &str, finished: Option<&str>) -> bool {
    created < w.hi.as_str()
        && updated >= w.lo.as_str()
        && finished.is_none_or(|t| t >= w.lo.as_str())
}

fn linked_prs(stmt: &mut rusqlite::Statement, issue: &str) -> Result<Vec<LinkedPr>> {
    Ok(stmt
        .query_map([issue], |row| {
            Ok(LinkedPr {
                item: ItemRef {
                    repo: row.get(0)?,
                    number: row.get(1)?,
                },
                state: row.get(2)?,
                is_draft: row.get(3)?,
                created_at: row.get(4)?,
                merged_at: row.get(5)?,
                closed_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Books PRs linked to deliverable `key` (to its parent issue or anywhere
/// below it, dropped sub-issues included): they're never `—`-row work,
/// those merged in `w` count toward its `PRs merged`, and any activity in
/// `w` lists it.
fn note_prs(
    prs: &[LinkedPr],
    key: &DeliverableKey,
    w: &Window,
    sow_prs: &mut BTreeSet<ItemRef>,
    merged_by_key: &mut BTreeMap<DeliverableKey, BTreeSet<ItemRef>>,
    active: &mut BTreeSet<DeliverableKey>,
) {
    for pr in prs {
        sow_prs.insert(pr.item.clone());
        if w.contains_opt(&pr.merged_at) {
            merged_by_key
                .entry(key.clone())
                .or_default()
                .insert(pr.item.clone());
        }
        if touched_in(w, &pr.created_at, &pr.updated_at, pr.finished()) {
            active.insert(key.clone());
        }
    }
}

struct TodoStatus {
    sow_ref: Option<String>,
    status: Option<String>,
}

/// The latest `/progress` status (and blocker, and its date) on any todo
/// created from `issue`, up to `end` (inclusive).
fn linked_todo_status(
    conn: &Connection,
    issue: &str,
    end: &str,
) -> Result<Option<(String, Option<String>, String)>> {
    Ok(conn
        .prepare_cached(
            "SELECT u.status, u.blocker, u.date FROM entries u
             JOIN entries t ON t.id = u.todo_id
             WHERE u.type = 'update' AND t.github_issue_id = ?1 AND u.date <= ?2
             ORDER BY u.id DESC LIMIT 1",
        )?
        .query_row(params![issue, end], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?)
}

/// Every deliverable with any activity in local days `start..=end` (`w` is
/// their window), per the design's rules - see the "Deliverables table"
/// and "Per-deliverable breakdown" sections of the design doc. Person-free
/// by construction: nothing here reads who did anything.
pub fn deliverables(
    conn: &Connection,
    start: &str,
    end: &str,
    w: &Window,
    tz: &TimeZone,
) -> Result<Vec<Deliverable>> {
    // In-range todos with their status as of `end` (like every other
    // number here), plus unplanned `/progress` reports with their own (no
    // todo, so no SOW ref: the `—` row).
    let mut stmt = conn.prepare(
        "SELECT t.sow_ref,
                (SELECT u.status FROM entries u
                 WHERE u.type = 'update' AND u.todo_id = t.id AND u.date <= ?2
                 ORDER BY u.id DESC LIMIT 1)
         FROM entries t WHERE t.type = 'todo' AND t.date >= ?1 AND t.date <= ?2
         UNION ALL
         SELECT NULL, status FROM entries
         WHERE type = 'update' AND todo_id IS NULL AND date >= ?1 AND date <= ?2",
    )?;
    let todos: Vec<TodoStatus> = stmt
        .query_map(params![start, end], |row| {
            Ok(TodoStatus {
                sow_ref: row
                    .get::<_, Option<String>>(0)?
                    .filter(|s| !s.trim().is_empty()),
                status: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut out: BTreeMap<DeliverableKey, Deliverable> = BTreeMap::new();
    let key_of = |raw: &str| match sow::parse(raw) {
        Some(sow_ref) => DeliverableKey::Sow(sow_ref),
        None => DeliverableKey::Raw(raw.trim().to_string()),
    };
    // Named by its ref until a parent issue's title names it.
    let blank = |key: DeliverableKey| Deliverable {
        title: match &key {
            DeliverableKey::Unlinked => "Not linked to a deliverable".to_string(),
            key => key_label(key),
        },
        key,
        sub_issues: None,
        prs_merged: None,
        todos: (0, 0),
        breakdown: Vec::new(),
        closed_before: Vec::new(),
    };

    // Parent issues carrying a SOW ref, grouped by it (two parents tagged
    // the same ref make one deliverable; the lowest-numbered names it).
    let mut stmt = conn.prepare(
        "SELECT node_id, title, sow_ref FROM github_issues
         WHERE parent_node_id IS NULL AND sow_ref IS NOT NULL
         ORDER BY repo, number",
    )?;
    let parents: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut children_stmt = conn.prepare(
        "SELECT node_id, repo, number, title, state, state_reason, created_at, closed_at,
                updated_at
         FROM github_issues WHERE parent_node_id = ?1 ORDER BY repo, number",
    )?;
    // PRs linked to an issue or to anything below it (a task's sub-tasks):
    // a deliverable -> task -> sub-task chain's PRs are the deliverable's
    // work. `UNION` (not `ALL`) ends the walk on a cycle.
    let mut prs_stmt = conn.prepare(
        "WITH RECURSIVE tree(node_id) AS (
             SELECT ?1
             UNION
             SELECT i.node_id FROM github_issues i JOIN tree ON i.parent_node_id = tree.node_id
         )
         SELECT DISTINCT p.repo, p.number, p.state, p.is_draft, p.created_at, p.merged_at,
                p.closed_at, p.updated_at
         FROM tree
         JOIN github_pr_issues pi ON pi.issue_node_id = tree.node_id
         JOIN github_prs p ON p.node_id = pi.pr_node_id
         ORDER BY p.repo, p.number",
    )?;
    let mut sow_prs: BTreeSet<ItemRef> = BTreeSet::new();
    let mut active: BTreeSet<DeliverableKey> = BTreeSet::new();
    let mut merged_by_key: BTreeMap<DeliverableKey, BTreeSet<ItemRef>> = BTreeMap::new();

    for (parent_id, parent_title, sow_raw) in parents {
        let key = key_of(&sow_raw);
        let entry = out.entry(key.clone()).or_insert_with(|| {
            let mut d = blank(key.clone());
            // A title that's only the ref strips to nothing: keep the ref.
            let title = sow::strip_from_title(&parent_title);
            if !title.is_empty() {
                d.title = title;
            }
            d
        });
        let counts = entry.sub_issues.get_or_insert((0, 0, 0));
        // The parent's tree: PRs linked to it directly, to any sub-issue
        // (dropped ones included) or further down.
        note_prs(
            &linked_prs(&mut prs_stmt, &parent_id)?,
            &key,
            w,
            &mut sow_prs,
            &mut merged_by_key,
            &mut active,
        );

        let children: Vec<Child> = children_stmt
            .query_map([&parent_id], |row| {
                Ok(Child {
                    node_id: row.get(0)?,
                    item: ItemRef {
                        repo: row.get(1)?,
                        number: row.get(2)?,
                    },
                    title: row.get(3)?,
                    state: row.get(4)?,
                    state_reason: row.get(5)?,
                    created_at: row.get(6)?,
                    closed_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        for child in children {
            let dropped = child.state == "closed"
                && matches!(
                    child.state_reason.as_deref(),
                    Some("not_planned" | "duplicate")
                );
            // Dropped, or not a sub-issue yet as of `end`: neither counts.
            if dropped || child.created_at.as_str() >= w.hi.as_str() {
                continue;
            }
            let closed_at = child
                .closed_at
                .as_deref()
                .filter(|_| child.state == "closed");
            if touched_in(w, &child.created_at, &child.updated_at, closed_at) {
                active.insert(key.clone());
            }

            counts.1 += 1;
            let closed_by_end = closed_at.is_some_and(|t| t < w.hi.as_str());
            if closed_by_end {
                counts.0 += 1;
            }
            if closed_at.is_some_and(|t| w.contains(t)) {
                counts.2 += 1;
            }
            if closed_at.is_some_and(|t| t < w.lo.as_str()) {
                entry.closed_before.push(child.item.clone());
                continue;
            }

            let todo = linked_todo_status(conn, &child.node_id, end)?;
            // Every PR that existed by the range end - not filtered on
            // `updated_at`, which a later push or comment moves past it.
            let prs = linked_prs(&mut prs_stmt, &child.node_id)?;
            let prs_shown: Vec<&LinkedPr> = prs
                .iter()
                .filter(|pr| pr.created_at.as_str() < w.hi.as_str())
                .collect();
            // Open as of the range end, not now: one merged since was
            // still in progress then.
            let open_at_end = prs_shown
                .iter()
                .any(|pr| pr.finished().is_none_or(|t| t >= w.hi.as_str()));
            let (state, last_activity) = if closed_by_end {
                (
                    SubIssueState::Closed,
                    child
                        .closed_at
                        .as_deref()
                        .map(|t| format!("{} closed", local_short_date(t, tz))),
                )
            } else {
                // Only the latest `updated_at` is stored: past the range end,
                // fall back to the last event still dated inside it.
                let hi = w.hi.as_str();
                let pr_activity = prs_shown
                    .iter()
                    .map(|pr| {
                        if pr.updated_at.as_str() < hi {
                            (pr.updated_at.as_str(), "updated")
                        } else if let Some(t) = pr.finished().filter(|t| *t < hi) {
                            (t, pr.state.as_str())
                        } else {
                            (pr.created_at.as_str(), "opened")
                        }
                    })
                    .max()
                    .map(|(t, what)| format!("{} PR {what}", local_short_date(t, tz)));
                match todo {
                    Some((status, blocker, date)) if status == "blocked" => (
                        SubIssueState::Blocked,
                        Some(match blocker {
                            Some(b) => format!("{} \"{}\"", crate::entries::short_date(&date), b),
                            None => format!("{} blocked", crate::entries::short_date(&date)),
                        }),
                    ),
                    Some((status, _, date)) => (
                        if status == "done" {
                            SubIssueState::ReportedDone
                        } else {
                            SubIssueState::InProgress
                        },
                        pr_activity.or_else(|| {
                            Some(format!("{} reported", crate::entries::short_date(&date)))
                        }),
                    ),
                    None if open_at_end => (SubIssueState::InProgress, pr_activity),
                    None => (SubIssueState::NotStarted, None),
                }
            };
            entry.breakdown.push(SubIssueRow {
                item: child.item,
                title: child.title,
                state,
                prs: prs_shown
                    .iter()
                    .map(|pr| format!("{} {}", pr.item, pr_state_label(&pr.state, pr.is_draft)))
                    .collect(),
                last_activity,
            });
        }
    }
    for (key, merged) in merged_by_key {
        if let Some(d) = out.get_mut(&key) {
            d.prs_merged = Some(merged.len());
        }
    }
    for d in out.values_mut() {
        if d.sub_issues.is_some() && d.prs_merged.is_none() {
            d.prs_merged = Some(0);
        }
        d.breakdown
            .sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.item.cmp(&b.item)));
    }

    // Todos: counted under their (normalised) ref, or the `—` row.
    for todo in &todos {
        let key = match &todo.sow_ref {
            Some(raw) => key_of(raw),
            None => DeliverableKey::Unlinked,
        };
        let entry = out.entry(key.clone()).or_insert_with(|| blank(key.clone()));
        entry.todos.1 += 1;
        if todo.status.as_deref() == Some("done") {
            entry.todos.0 += 1;
        }
        active.insert(key);
    }

    // The `—` row's PRs: roster members' PRs merged in range that don't
    // close any deliverable's sub-issue (bots never count).
    let roster_ids: BTreeSet<i64> = roster(conn)?.iter().filter_map(|m| m.github_id).collect();
    let mut stmt = conn.prepare(
        "SELECT repo, number, author_id FROM github_prs
         WHERE merged_at >= ?1 AND merged_at < ?2 AND author_id IS NOT NULL",
    )?;
    let merged: Vec<(ItemRef, i64)> = stmt
        .query_map(params![w.lo, w.hi], |row| {
            Ok((
                ItemRef {
                    repo: row.get(0)?,
                    number: row.get(1)?,
                },
                row.get(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let unlinked_prs = merged
        .iter()
        .filter(|(item, author)| roster_ids.contains(author) && !sow_prs.contains(item))
        .count();
    if unlinked_prs > 0 {
        let entry = out
            .entry(DeliverableKey::Unlinked)
            .or_insert_with(|| blank(DeliverableKey::Unlinked));
        entry.prs_merged = Some(unlinked_prs);
        active.insert(DeliverableKey::Unlinked);
    }

    Ok(out
        .into_values()
        .filter(|d| active.contains(&d.key))
        .collect())
}

fn key_label(key: &DeliverableKey) -> String {
    match key {
        DeliverableKey::Sow(sow_ref) => sow_ref.to_string(),
        DeliverableKey::Raw(raw) => raw.clone(),
        DeliverableKey::Unlinked => "—".to_string(),
    }
}

/// `/recap`'s deliverables sections: the summary table, then one
/// breakdown table per deliverable that has GitHub sub-issues.
pub fn format_deliverables(ds: &[Deliverable]) -> String {
    let rows: Vec<Vec<String>> = ds
        .iter()
        .map(|d| {
            vec![
                sanitize_cell(&key_label(&d.key)),
                sanitize_cell(&d.title),
                d.sub_issues
                    .map(|(closed, total, new)| format!("{closed}/{total} closed (+{new})"))
                    .unwrap_or_else(|| "—".into()),
                d.prs_merged
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
                if d.todos.1 == 0 {
                    "—".into()
                } else {
                    format!("{}/{}", d.todos.0, d.todos.1)
                },
            ]
        })
        .collect();
    let mut out = format!(
        "## Deliverables\n\n{}",
        crate::status::render_aligned_table(
            &[
                "SOW",
                "Deliverable",
                "Sub-issues",
                "PRs merged",
                "Todos done"
            ],
            &rows
        )
    );
    for d in ds {
        let (Some((closed, total, _)), DeliverableKey::Sow(_)) = (d.sub_issues, &d.key) else {
            continue;
        };
        if d.breakdown.is_empty() && d.closed_before.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n\n### {} — {} ({closed}/{total} closed)",
            key_label(&d.key),
            sanitize_cell(&d.title)
        ));
        if !d.breakdown.is_empty() {
            let rows: Vec<Vec<String>> = d
                .breakdown
                .iter()
                .map(|r| {
                    vec![
                        sanitize_cell(&format!("{} {}", r.item, r.title)),
                        r.state.label().to_string(),
                        if r.prs.is_empty() {
                            "—".into()
                        } else {
                            r.prs.join(", ")
                        },
                        text_cell(r.last_activity.as_deref()),
                    ]
                })
                .collect();
            out.push_str("\n\n");
            out.push_str(&crate::status::render_aligned_table(
                &["Sub-issue", "State", "PRs", "Last activity"],
                &rows,
            ));
        }
        if !d.closed_before.is_empty() {
            out.push_str(&format!(
                "\n\nClosed before this period: {}",
                items(&d.closed_before)
            ));
        }
    }
    out
}

/// `/recap`'s attachment with GitHub configured: the person-free
/// deliverables report, plus - only with `include_member_detail` - the
/// per-day standup + GitHub tables (names included). `None` when there's
/// nothing to show at all.
pub fn recap_file(
    conn: &Connection,
    start: &str,
    end: &str,
    tz: &TimeZone,
    include_member_detail: bool,
) -> Result<Option<String>> {
    let w = window(start, end, tz)?;
    let ds = deliverables(conn, start, end, &w, tz)?;

    let mut days = Vec::new();
    if include_member_detail {
        let start_date = Date::strptime("%Y-%m-%d", start)?;
        let end_date = Date::strptime("%Y-%m-%d", end)?;
        let mut date = start_date;
        while date <= end_date {
            let day = date.to_string();
            let recap_day = recap::day_recap(conn, &day)?;
            let gh = day_github(conn, &day, &window(&day, &day, tz)?)?;
            if recap_day.is_some() || !gh.is_empty() {
                days.push(format_github_day(conn, &day, recap_day.as_ref(), &gh, 3)?);
            }
            date = date.tomorrow()?;
        }
    }
    if ds.is_empty() && days.is_empty() {
        return Ok(None);
    }

    let title = if start == end {
        format!("# Recap {start}")
    } else {
        format!("# Recap {start} → {end}")
    };
    let mut out = format!("{title}\n\n");
    if ds.is_empty() {
        out.push_str("## Deliverables\n\nNo deliverable activity in this period.");
    } else {
        out.push_str(&format_deliverables(&ds));
    }
    if !days.is_empty() {
        out.push_str("\n\n## Member detail by day\n\n");
        out.push_str(&days.join("\n\n"));
    }
    Ok(Some(out))
}

/// `/team github`'s reply for local days `start..=end`.
pub fn team_github(conn: &Connection, start: &str, end: &str, tz: &TimeZone) -> Result<String> {
    let w = window(start, end, tz)?;
    let activity = team_activity(conn, &w)?;
    let label = if start == end {
        start.to_string()
    } else {
        format!("{start} to {end}")
    };
    Ok(format_team_github(&activity, &label, start == end))
}

#[cfg(test)]
mod tests;

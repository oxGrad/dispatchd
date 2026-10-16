//! The GitHub activity sync: a background loop (spawned next to the
//! standup ticker, independent of it) that every `sync_interval_minutes`
//! pulls PRs, issues and reviews updated since the last run into the
//! `github_*` tables. A failed run is logged and recorded in
//! `github_sync_state.last_sync_error`, then simply retried next interval;
//! it never touches the standup ritual.
//!
//! `collect` does all the fetching and no DB writes, so `sync_once` (the
//! bot) and `preview` (`dispatchd github preview`) run the exact same
//! pipeline. The decisions (`since_for`, `parents_to_expand`,
//! `format_preview`) are pure and unit-tested.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use serde_json::Value;

use super::auth::{AppKey, InstallationToken};
use super::client::Client;
use super::graphql::{self, Issue, Origin, Page, PullRequest, SearchKind, SubIssueList};
use super::store;
use crate::config::GithubConfig;
use crate::sow::SowRef;

/// How far back the very first sync reaches - the same window
/// `followups::backfill_missed` covers.
pub const BACKFILL_DAYS: i64 = 14;
/// Each incremental search re-reads this much before the high-water mark:
/// GitHub's search index can lag a write by a minute or two, so an item
/// updated just before the last run may only have become searchable after
/// it. Re-reading is harmless - every write is an upsert.
const OVERLAP_MINUTES: i64 = 10;
/// GitHub stops serving search results past 1,000 (10 pages of 100).
const MAX_PAGES: usize = 10;
/// How many node-level GraphQL errors a run logs individually.
const MAX_LOGGED_SKIPS: usize = 5;

/// Mints and caches installation tokens for one App + org, and owns the
/// one HTTP client every request of a run shares.
pub struct TokenSource {
    client: Client,
    app_id: u64,
    org: String,
    key: AppKey,
    installation_id: Option<u64>,
    token: Option<InstallationToken>,
}

impl TokenSource {
    pub fn new(github: &GithubConfig, key: AppKey) -> TokenSource {
        TokenSource {
            client: Client::new(),
            app_id: github.app_id,
            org: github.org.clone(),
            key,
            installation_id: None,
            token: None,
        }
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// A valid installation token, minting a new one when there's none
    /// cached or the cached one is close to expiry. Cheap when cached, so
    /// it's called before every request: a long run (a big backfill on a
    /// slow link) can outlive one token.
    pub async fn token(&mut self) -> Result<String> {
        let now = Utc::now().timestamp();
        if let Some(token) = self.token.as_ref().filter(|t| !t.needs_refresh(now)) {
            return Ok(token.token.clone());
        }
        let jwt = self.key.app_jwt(self.app_id, now)?;
        let installation_id = match self.installation_id {
            Some(id) => id,
            None => {
                let id = self.client.org_installation(&jwt, &self.org).await?.id;
                self.installation_id = Some(id);
                id
            }
        };
        let token = self
            .client
            .installation_token(&jwt, installation_id, &self.org)
            .await?;
        let value = token.token.clone();
        self.token = Some(token);
        Ok(value)
    }

    /// Forgets the cached token and installation, so the next `token()`
    /// starts from scratch - after a failed run, in case the token was
    /// revoked or the App reinstalled under a new installation id.
    pub fn reset(&mut self) {
        self.token = None;
        self.installation_id = None;
    }

    async fn graphql(
        &mut self,
        query: &str,
        variables: Value,
        skipped: &mut Vec<String>,
    ) -> Result<Value> {
        let token = self.token().await?;
        let response = self.client.graphql(&token, query, variables).await?;
        skipped.extend(response.skipped);
        Ok(response.data)
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Where an incremental search starts: `OVERLAP_MINUTES` before the stored
/// high-water mark, or `BACKFILL_DAYS` back on the very first run (or if
/// the stored mark is unreadable).
pub fn since_for(high_water: Option<&str>, now: DateTime<Utc>) -> String {
    let start = high_water
        .and_then(|hw| DateTime::parse_from_rfc3339(hw).ok())
        .map(|hw| hw.with_timezone(&Utc) - chrono::Duration::minutes(OVERLAP_MINUTES))
        .unwrap_or_else(|| now - chrono::Duration::days(BACKFILL_DAYS));
    rfc3339(start)
}

struct SearchResult<T> {
    items: Vec<T>,
    /// Where the next search should resume from (see `fetch_search`).
    high_water: Option<String>,
}

/// Pages through one org search. Normally the high-water mark is the
/// newest hit seen. If GitHub's 1,000-result cap cuts the search short, it
/// is instead the last hit's position in the `sort:updated-asc` order, so
/// the next run resumes from where this one stopped rather than jumping
/// past results it never fetched.
async fn fetch_search<T>(
    tokens: &mut TokenSource,
    query: &str,
    q: &str,
    parse: fn(Value) -> Result<Page<T>>,
    what: &str,
    skipped: &mut Vec<String>,
) -> Result<SearchResult<T>> {
    let mut items = Vec::new();
    let mut high_water: Option<String> = None;
    let mut cursor: Option<String> = None;
    for page_number in 1..=MAX_PAGES {
        let data = tokens
            .graphql(
                query,
                serde_json::json!({ "q": q, "cursor": cursor }),
                skipped,
            )
            .await
            .with_context(|| format!("{what} search failed"))?;
        let page = parse(data)?;
        items.extend(page.items);
        high_water = high_water.max(page.high_water);
        if !page.has_next_page {
            break;
        }
        if page_number == MAX_PAGES {
            eprintln!(
                "github sync: {what} search hit GitHub's 1,000-result cap; the rest is \
                 picked up from {} on the next run",
                page.last_updated.as_deref().unwrap_or("the same point")
            );
            high_water = page.last_updated;
            break;
        }
        cursor = page.end_cursor;
    }
    Ok(SearchResult { items, high_water })
}

/// Everything one run fetched, before anything is written.
pub struct Collected {
    pub prs: Vec<PullRequest>,
    pub issues: Vec<Issue>,
    pub sub_issue_lists: Vec<SubIssueList>,
    pub pr_high_water: Option<String>,
    pub issue_high_water: Option<String>,
    /// Node-level GraphQL errors tolerated along the way (items outside
    /// the installation, deleted ids).
    pub skipped: Vec<String>,
}

/// Which deliverables to refetch every sub-issue of: the top-level,
/// SOW-tagged issue behind each *fresh* row - a search hit updated after
/// `previous_high_water` (not one re-read only because of the overlap
/// window), or an issue fetched by id because a PR links to it. A parent
/// riding along with a row isn't fresh by itself. Without the freshness check, the
/// overlap would make every run re-expand the newest item's deliverable
/// forever.
pub fn parents_to_expand(issues: &[Issue], previous_high_water: Option<&str>) -> Vec<String> {
    let by_id: BTreeMap<&str, &Issue> = issues.iter().map(|i| (i.node_id.as_str(), i)).collect();
    let is_fresh = |i: &Issue| match i.origin {
        Origin::SearchHit => previous_high_water.is_none_or(|hw| i.updated_at.as_str() > hw),
        Origin::ById => true,
        Origin::Parent | Origin::SubIssue => false,
    };
    let is_deliverable = |i: &Issue| i.parent_node_id.is_none() && i.sow_ref.is_some();
    issues
        .iter()
        .filter(|i| is_fresh(i))
        .filter_map(|i| match &i.parent_node_id {
            None => Some(i),
            Some(parent) => by_id.get(parent.as_str()).copied(),
        })
        .filter(|i| is_deliverable(i))
        .map(|i| i.node_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The whole fetch pipeline, no DB writes:
/// 1. PRs and issues updated since each `since` (searches);
/// 2. issues a PR links to that `missing` says aren't stored yet (by id);
/// 3. every sub-issue of each deliverable with fresh activity
///    (`parents_to_expand`), so untouched sub-issues are known too.
pub async fn collect(
    tokens: &mut TokenSource,
    org: &str,
    pr_since: &str,
    issue_since: &str,
    previous_issue_high_water: Option<&str>,
    missing: impl Fn(&[String]) -> Result<Vec<String>>,
) -> Result<Collected> {
    let mut skipped = Vec::new();
    let prs = fetch_search(
        tokens,
        &graphql::pr_search_query(),
        &graphql::search_string(org, SearchKind::Pr, pr_since),
        graphql::parse_pr_page,
        "PR",
        &mut skipped,
    )
    .await?;
    let issues = fetch_search(
        tokens,
        &graphql::issue_search_query(),
        &graphql::search_string(org, SearchKind::Issue, issue_since),
        graphql::parse_issue_page,
        "issue",
        &mut skipped,
    )
    .await?;
    let mut all_issues = issues.items;

    let known: BTreeSet<&str> = all_issues.iter().map(|i| i.node_id.as_str()).collect();
    let linked: Vec<String> = prs
        .items
        .iter()
        .flat_map(|pr| pr.closing_issue_ids.iter())
        .filter(|id| !known.contains(id.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let to_fetch = missing(&linked)?;
    let query = graphql::issues_by_id_query();
    for batch in to_fetch.chunks(graphql::NODES_BATCH) {
        let data = tokens
            .graphql(&query, serde_json::json!({ "ids": batch }), &mut skipped)
            .await
            .context("fetching PR-linked issues failed")?;
        all_issues.extend(graphql::parse_issues_by_id(data)?);
    }

    let mut sub_issue_lists = Vec::new();
    let query = graphql::sub_issues_query();
    for batch in
        parents_to_expand(&all_issues, previous_issue_high_water).chunks(graphql::SUB_ISSUE_BATCH)
    {
        let data = tokens
            .graphql(&query, serde_json::json!({ "ids": batch }), &mut skipped)
            .await
            .context("fetching sub-issues failed")?;
        sub_issue_lists.extend(graphql::parse_sub_issues(data)?);
    }
    for list in &sub_issue_lists {
        all_issues.extend(list.children.iter().cloned());
    }

    Ok(Collected {
        prs: prs.items,
        issues: all_issues,
        sub_issue_lists,
        pr_high_water: prs.high_water,
        issue_high_water: issues.high_water,
        skipped,
    })
}

/// What one sync run stored.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub prs: usize,
    /// Distinct issues written (search hits, their parents, linked issues
    /// and sub-issues together).
    pub issues: usize,
    pub sub_issue_parents: usize,
    pub members_resolved: usize,
    pub skipped: usize,
}

fn log_skipped(skipped: &[String]) {
    let distinct: BTreeSet<&str> = skipped.iter().map(String::as_str).collect();
    for message in distinct.iter().take(MAX_LOGGED_SKIPS) {
        eprintln!("github sync: skipped an inaccessible item: {message}");
    }
    if distinct.len() > MAX_LOGGED_SKIPS {
        eprintln!(
            "github sync: ...and {} more inaccessible items",
            distinct.len() - MAX_LOGGED_SKIPS
        );
    }
}

/// One full sync pass: `collect`, then one `store::write_batch`
/// transaction, then numeric ids for members whose `github_user` isn't
/// resolved yet. The DB lock is only held while reading or writing, never
/// across a request.
pub async fn sync_once(
    db: &Arc<Mutex<Connection>>,
    tokens: &mut TokenSource,
    org: &str,
    now: DateTime<Utc>,
) -> Result<SyncReport> {
    let (pr_hw, issue_hw) = {
        let conn = db.lock().expect("db mutex poisoned");
        (
            store::get_state(&conn, store::PR_HIGH_WATER)?,
            store::get_state(&conn, store::ISSUE_HIGH_WATER)?,
        )
    };
    let collected = collect(
        tokens,
        org,
        &since_for(pr_hw.as_deref(), now),
        &since_for(issue_hw.as_deref(), now),
        issue_hw.as_deref(),
        |ids| {
            let conn = db.lock().expect("db mutex poisoned");
            store::missing_issue_ids(&conn, ids)
        },
    )
    .await?;
    log_skipped(&collected.skipped);

    let mut warnings = BTreeSet::new();
    for issue in &collected.issues {
        for warning in &issue.warnings {
            warnings.insert(format!("{}#{}: {warning}", issue.repo, issue.number));
        }
    }
    for warning in &warnings {
        eprintln!("github sync: {warning}");
    }

    {
        let conn = db.lock().expect("db mutex poisoned");
        store::write_batch(
            &conn,
            &store::Batch {
                issues: &collected.issues,
                prs: &collected.prs,
                sub_issue_lists: &collected.sub_issue_lists,
                pr_high_water: collected.pr_high_water.as_deref(),
                issue_high_water: collected.issue_high_water.as_deref(),
            },
        )?;
    }

    // Activity is already stored at this point, so one member's failed
    // lookup is logged and retried next run rather than failing this one.
    let unresolved = {
        let conn = db.lock().expect("db mutex poisoned");
        store::members_needing_github_id(&conn)?
    };
    let client = tokens.client();
    let mut members_resolved = 0;
    for (discord_user_id, login) in unresolved {
        let lookup = match tokens.token().await {
            Ok(token) => client.user_id(&token, &login).await,
            Err(e) => Err(e),
        };
        match lookup {
            Ok(Some(github_id)) => {
                let conn = db.lock().expect("db mutex poisoned");
                store::set_member_github_id(&conn, &discord_user_id, github_id)?;
                members_resolved += 1;
            }
            Ok(None) => eprintln!(
                "github sync: github_user {login:?} (member {discord_user_id}) doesn't exist on \
                 GitHub - renamed? fix it in members.toml"
            ),
            Err(e) => eprintln!(
                "github sync: couldn't look up github_user {login:?} (member \
                 {discord_user_id}), retrying next run: {e:#}"
            ),
        }
    }

    {
        let conn = db.lock().expect("db mutex poisoned");
        store::set_state(&conn, store::LAST_SYNC_AT, &rfc3339(now))?;
        store::clear_state(&conn, store::LAST_SYNC_ERROR)?;
    }

    Ok(SyncReport {
        prs: collected
            .prs
            .iter()
            .map(|pr| pr.node_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        issues: collected
            .issues
            .iter()
            .map(|i| i.node_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        sub_issue_parents: collected.sub_issue_lists.len(),
        members_resolved,
        skipped: collected.skipped.len(),
    })
}

/// The background loop: one `sync_once` right away, then every
/// `sync_interval_minutes`. Runs for the life of the bot.
pub async fn run_loop(db: Arc<Mutex<Connection>>, github: GithubConfig, key: AppKey) {
    let mut tokens = TokenSource::new(&github, key);
    let mut interval = tokio::time::interval(Duration::from_secs(
        u64::from(github.sync_interval_minutes) * 60,
    ));
    loop {
        interval.tick().await;
        let now = Utc::now();
        match sync_once(&db, &mut tokens, &github.org, now).await {
            Ok(report) => println!(
                "github sync: {} PRs, {} issues ({} deliverables' sub-issues), {} member ids \
                 resolved, {} inaccessible items skipped",
                report.prs,
                report.issues,
                report.sub_issue_parents,
                report.members_resolved,
                report.skipped
            ),
            Err(e) => {
                eprintln!("github sync failed: {e:#}");
                tokens.reset();
                let conn = db.lock().expect("db mutex poisoned");
                if let Err(e) = store::record_sync_error(&conn, &format!("{}: {e:#}", rfc3339(now)))
                {
                    eprintln!("failed to record the github sync error: {e}");
                }
            }
        }
    }
}

/// `dispatchd github preview --days N`: runs the sync's whole fetch
/// pipeline (`collect` - searches, linked issues by id, sub-issues) once
/// against the real org and prints a summary, without opening the DB.
pub async fn preview(github: &GithubConfig, key: AppKey, days: u32) -> Result<String> {
    let mut tokens = TokenSource::new(github, key);
    let since = rfc3339(Utc::now() - chrono::Duration::days(i64::from(days)));
    let collected = collect(&mut tokens, &github.org, &since, &since, None, |ids| {
        Ok(ids.to_vec())
    })
    .await?;
    Ok(format_preview(&github.org, &since, &collected))
}

#[derive(Default)]
struct AuthorCounts {
    prs_opened: usize,
    prs_merged: usize,
    reviews: usize,
}

/// Pure - `preview`'s output: org-wide totals plus a per-login breakdown,
/// sorted by login (not by volume). Counts only the search hits themselves
/// (deduplicated), and only activity inside the window: a search hit only
/// means *something* changed since `since`.
pub fn format_preview(org: &str, since: &str, collected: &Collected) -> String {
    let in_window = |t: &Option<String>| t.as_deref().is_some_and(|t| t >= since);
    let login = |a: &graphql::Actor| a.login.clone().unwrap_or_else(|| "(ghost)".into());

    let prs: BTreeMap<&str, &PullRequest> = collected
        .prs
        .iter()
        .map(|pr| (pr.node_id.as_str(), pr))
        .collect();
    let hits: BTreeMap<&str, &Issue> = collected
        .issues
        .iter()
        .filter(|i| i.origin == Origin::SearchHit)
        .map(|i| (i.node_id.as_str(), i))
        .collect();

    let mut by_author: BTreeMap<String, AuthorCounts> = BTreeMap::new();
    let mut reviews = 0;
    for pr in prs.values() {
        if pr.created_at.as_str() >= since {
            by_author.entry(login(&pr.author)).or_default().prs_opened += 1;
        }
        if in_window(&pr.merged_at) {
            by_author.entry(login(&pr.author)).or_default().prs_merged += 1;
        }
        for review in pr
            .reviews
            .iter()
            .filter(|r| r.submitted_at.as_str() >= since)
        {
            by_author.entry(login(&review.author)).or_default().reviews += 1;
            reviews += 1;
        }
    }
    let merged = prs.values().filter(|pr| in_window(&pr.merged_at)).count();
    let closed = hits.values().filter(|i| in_window(&i.closed_at)).count();
    let deliverables: BTreeSet<SowRef> = collected
        .issues
        .iter()
        .filter(|i| i.parent_node_id.is_none())
        .filter_map(|i| i.sow_ref)
        .collect();

    let mut out = format!(
        "GitHub activity in {org} since {since}:\n  {} PRs updated ({merged} merged), {} issues updated ({closed} closed), {reviews} reviews\n",
        prs.len(),
        hits.len(),
    );
    if !deliverables.is_empty() {
        out.push_str(&format!(
            "  deliverables seen: {} ({} sub-issue lists fetched)\n",
            deliverables
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            collected.sub_issue_lists.len()
        ));
    }
    if !collected.skipped.is_empty() {
        out.push_str(&format!(
            "  skipped {} inaccessible item(s) (outside the App's installation, or deleted)\n",
            collected.skipped.len()
        ));
    }
    if !by_author.is_empty() {
        out.push_str("  by GitHub user:\n");
        let width = by_author.keys().map(String::len).max().unwrap_or(0);
        for (login, c) in &by_author {
            out.push_str(&format!(
                "    {login:<width$}  {} opened, {} merged, {} reviews\n",
                c.prs_opened, c.prs_merged, c.reviews
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::graphql::{Actor, Review};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-16T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn since_for_backfills_on_the_first_run() {
        assert_eq!(since_for(None, now()), "2026-10-02T02:00:00Z");
    }

    #[test]
    fn since_for_overlaps_the_high_water_mark() {
        assert_eq!(
            since_for(Some("2026-10-15T09:00:00Z"), now()),
            "2026-10-15T08:50:00Z"
        );
    }

    #[test]
    fn since_for_ignores_a_garbled_high_water_mark() {
        assert_eq!(since_for(Some("yesterday"), now()), "2026-10-02T02:00:00Z");
    }

    fn actor(login: &str) -> Actor {
        Actor {
            id: Some(1),
            login: Some(login.into()),
        }
    }

    fn pr(
        author: &str,
        created: &str,
        merged: Option<&str>,
        reviewers: &[(&str, &str)],
    ) -> PullRequest {
        PullRequest {
            node_id: format!("PR_{author}_{created}"),
            repo: "api".into(),
            number: 1,
            title: "t".into(),
            state: if merged.is_some() { "merged" } else { "open" }.into(),
            is_draft: false,
            author: actor(author),
            created_at: created.into(),
            updated_at: created.into(),
            merged_at: merged.map(Into::into),
            closed_at: merged.map(Into::into),
            closing_issue_ids: vec![],
            reviews: reviewers
                .iter()
                .map(|(login, at)| Review {
                    node_id: format!("R_{login}_{at}"),
                    author: actor(login),
                    state: "approved".into(),
                    submitted_at: (*at).into(),
                })
                .collect(),
        }
    }

    fn issue(
        node_id: &str,
        parent: Option<&str>,
        sow: Option<(u32, u32)>,
        updated: &str,
        origin: Origin,
    ) -> Issue {
        Issue {
            node_id: node_id.into(),
            repo: "api".into(),
            number: 1,
            title: "t".into(),
            state: "open".into(),
            state_reason: None,
            author: actor("sari"),
            created_at: "2026-10-01T00:00:00Z".into(),
            updated_at: updated.into(),
            closed_at: None,
            assignee_ids: vec![],
            sow_ref: sow.map(|(m, d)| SowRef {
                milestone: m,
                deliverable: Some(d),
            }),
            parent_node_id: parent.map(Into::into),
            origin,
            warnings: vec![],
        }
    }

    #[test]
    fn parents_to_expand_takes_the_deliverable_behind_each_fresh_row() {
        let issues = vec![
            issue(
                "I_201",
                Some("I_100"),
                None,
                "2026-10-15T09:00:00Z",
                Origin::SearchHit,
            ),
            issue(
                "I_100",
                None,
                Some((1, 2)),
                "2026-10-01T00:00:00Z",
                Origin::Parent,
            ),
            // A top-level deliverable updated itself.
            issue(
                "I_300",
                None,
                Some((1, 3)),
                "2026-10-15T09:30:00Z",
                Origin::SearchHit,
            ),
            // Top-level but no SOW ref: not a deliverable.
            issue(
                "I_400",
                None,
                None,
                "2026-10-15T09:30:00Z",
                Origin::SearchHit,
            ),
        ];
        assert_eq!(
            parents_to_expand(&issues, Some("2026-10-15T08:00:00Z")),
            vec!["I_100", "I_300"]
        );
    }

    #[test]
    fn parents_to_expand_skips_hits_only_re_read_by_the_overlap() {
        let issues = vec![
            issue(
                "I_201",
                Some("I_100"),
                None,
                "2026-10-15T09:00:00Z",
                Origin::SearchHit,
            ),
            issue(
                "I_100",
                None,
                Some((1, 2)),
                "2026-10-01T00:00:00Z",
                Origin::Parent,
            ),
        ];
        // The stored high-water mark is this very hit: nothing new.
        assert!(parents_to_expand(&issues, Some("2026-10-15T09:00:00Z")).is_empty());
        // First run: everything is fresh.
        assert_eq!(parents_to_expand(&issues, None), vec!["I_100"]);
    }

    #[test]
    fn parents_to_expand_always_expands_issues_fetched_by_id() {
        let issues = vec![
            issue(
                "I_201",
                Some("I_100"),
                None,
                "2026-09-01T00:00:00Z",
                Origin::ById,
            ),
            issue(
                "I_100",
                None,
                Some((1, 2)),
                "2026-09-01T00:00:00Z",
                Origin::Parent,
            ),
        ];
        assert_eq!(
            parents_to_expand(&issues, Some("2026-10-15T09:00:00Z")),
            vec!["I_100"]
        );
    }

    fn collected(prs: Vec<PullRequest>, issues: Vec<Issue>) -> Collected {
        Collected {
            prs,
            issues,
            sub_issue_lists: vec![],
            pr_high_water: None,
            issue_high_water: None,
            skipped: vec![],
        }
    }

    #[test]
    fn format_preview_counts_only_search_hits_inside_the_window_by_login() {
        let since = "2026-10-15T00:00:00Z";
        let merged = pr(
            "budi-dev",
            "2026-10-10T00:00:00Z",
            Some("2026-10-15T08:00:00Z"),
            &[
                ("rina", "2026-10-15T07:00:00Z"),
                ("sari", "2026-10-09T00:00:00Z"),
            ],
        );
        let prs = vec![
            merged.clone(),
            // The same PR again, as it could come back on two pages.
            merged,
            pr("sari", "2026-10-15T09:00:00Z", None, &[]),
        ];
        let mut closed = issue(
            "I_1",
            Some("I_100"),
            None,
            "2026-10-15T08:00:00Z",
            Origin::SearchHit,
        );
        closed.closed_at = Some("2026-10-15T08:00:00Z".into());
        let issues = vec![
            closed.clone(),
            closed,
            // The parent riding along: not updated in the window, not counted.
            issue(
                "I_100",
                None,
                Some((1, 10)),
                "2026-10-01T00:00:00Z",
                Origin::Parent,
            ),
            issue(
                "I_2",
                None,
                Some((1, 2)),
                "2026-10-15T08:00:00Z",
                Origin::SearchHit,
            ),
        ];

        let out = format_preview("oxgrad", since, &collected(prs, issues));

        assert!(
            out.contains("2 PRs updated (1 merged), 2 issues updated (1 closed), 1 reviews"),
            "{out}"
        );
        // Numeric order, not string order (D.P.1.10 after D.P.1.2).
        assert!(
            out.contains("deliverables seen: D.P.1.2, D.P.1.10"),
            "{out}"
        );
        assert!(
            out.contains("budi-dev  0 opened, 1 merged, 0 reviews"),
            "{out}"
        );
        assert!(
            out.contains("rina      0 opened, 0 merged, 1 reviews"),
            "{out}"
        );
        assert!(
            out.contains("sari      1 opened, 0 merged, 0 reviews"),
            "{out}"
        );
        // Sorted by login, never by volume.
        let budi = out.find("budi-dev  ").unwrap();
        let rina = out.find("rina  ").unwrap();
        let sari = out.find("sari  ").unwrap();
        assert!(budi < rina && rina < sari);
    }

    #[test]
    fn format_preview_mentions_skipped_items() {
        let mut c = collected(vec![], vec![]);
        c.skipped = vec!["Resource not accessible by integration".into()];
        let out = format_preview("oxgrad", "2026-10-15T00:00:00Z", &c);
        assert!(out.contains("skipped 1 inaccessible item(s)"), "{out}");
    }

    #[test]
    fn format_preview_with_nothing_found() {
        let out = format_preview("oxgrad", "2026-10-15T00:00:00Z", &collected(vec![], vec![]));
        assert_eq!(
            out,
            "GitHub activity in oxgrad since 2026-10-15T00:00:00Z:\n  0 PRs updated (0 merged), 0 issues updated (0 closed), 0 reviews\n"
        );
    }
}

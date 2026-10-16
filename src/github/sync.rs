//! The GitHub activity sync: a background loop (spawned next to the
//! standup ticker, independent of it) that every `sync_interval_minutes`
//! pulls PRs, issues and reviews updated since the last run into the
//! `github_*` tables. A failed run is logged and recorded in
//! `github_sync_state.last_sync_error`, then simply retried next interval;
//! it never touches the standup ritual.
//!
//! Network I/O lives in `fetch_*` / `sync_once` / `run_loop`; the decisions
//! (`since_for`, `format_preview`) are pure and unit-tested.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;

use super::auth::{AppKey, InstallationToken};
use super::client::Client;
use super::graphql::{self, Issue, PullRequest, SearchKind};
use super::store;
use crate::config::GithubConfig;

/// How far back the very first sync reaches - the same window
/// `followups::backfill_missed` covers.
pub const BACKFILL_DAYS: i64 = 14;
/// Each incremental search re-reads this much before the high-water mark:
/// GitHub's search index can lag a write by a minute or two, so an item
/// updated just before the last run may only have become searchable after
/// it. Re-reading is harmless - every write is an upsert.
const OVERLAP_MINUTES: i64 = 10;
/// GitHub stops serving search results past 1,000 (20 pages of 50).
const MAX_PAGES: usize = 20;

/// Mints and caches installation tokens for one App + org.
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

    /// A valid installation token, minting a new one when there's none
    /// cached or the cached one is close to expiry.
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

/// Every PR in the org updated since `since` (all pages), plus the newest
/// `updatedAt` among them.
pub async fn fetch_prs(
    client: &Client,
    token: &str,
    org: &str,
    since: &str,
) -> Result<(Vec<PullRequest>, Option<String>)> {
    let query = graphql::pr_search_query();
    let q = graphql::search_string(org, SearchKind::Pr, since);
    let mut out = Vec::new();
    let mut high_water: Option<String> = None;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let data = client
            .graphql(
                token,
                &query,
                serde_json::json!({ "q": q, "cursor": cursor }),
            )
            .await
            .context("PR search failed")?;
        let page = graphql::parse_pr_page(data)?;
        out.extend(page.items);
        high_water = high_water.max(page.high_water);
        if !page.has_next_page {
            break;
        }
        cursor = page.end_cursor;
    }
    Ok((out, high_water))
}

/// Every issue in the org updated since `since` (all pages), each followed
/// by its parent when it has one, plus the newest `updatedAt` among the
/// search hits themselves (see `graphql::Page::high_water`).
pub async fn fetch_issues(
    client: &Client,
    token: &str,
    org: &str,
    since: &str,
) -> Result<(Vec<Issue>, Option<String>)> {
    let query = graphql::issue_search_query();
    let q = graphql::search_string(org, SearchKind::Issue, since);
    let mut out = Vec::new();
    let mut high_water: Option<String> = None;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let data = client
            .graphql(
                token,
                &query,
                serde_json::json!({ "q": q, "cursor": cursor }),
            )
            .await
            .context("issue search failed")?;
        let page = graphql::parse_issue_page(data)?;
        out.extend(page.items);
        high_water = high_water.max(page.high_water);
        if !page.has_next_page {
            break;
        }
        cursor = page.end_cursor;
    }
    Ok((out, high_water))
}

async fn fetch_by_ids(
    client: &Client,
    token: &str,
    query: &str,
    ids: &[String],
    parse: fn(serde_json::Value) -> Result<Vec<Issue>>,
) -> Result<Vec<Issue>> {
    let mut out = Vec::new();
    for batch in ids.chunks(graphql::NODES_BATCH) {
        let data = client
            .graphql(token, query, serde_json::json!({ "ids": batch }))
            .await?;
        out.extend(parse(data)?);
    }
    Ok(out)
}

/// What one sync run stored.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub prs: usize,
    pub issues: usize,
    pub sub_issue_parents: usize,
    pub members_resolved: usize,
}

/// One full sync pass:
/// 1. PRs and issues updated since each high-water mark (searches);
/// 2. issues a PR links to that aren't stored yet (by id);
/// 3. every sub-issue of each deliverable (SOW-tagged parent) touched
///    this run, so untouched sub-issues are known too;
/// 4. numeric ids for members whose `github_user` isn't resolved yet.
///
/// The DB lock is only held while writing, never across a request.
pub async fn sync_once(
    db: &Arc<Mutex<Connection>>,
    client: &Client,
    tokens: &mut TokenSource,
    org: &str,
    now: DateTime<Utc>,
) -> Result<SyncReport> {
    let token = tokens.token().await?;
    let (pr_hw, issue_hw) = {
        let conn = db.lock().expect("db mutex poisoned");
        (
            store::get_state(&conn, store::PR_HIGH_WATER)?,
            store::get_state(&conn, store::ISSUE_HIGH_WATER)?,
        )
    };

    let (prs, pr_search_hw) =
        fetch_prs(client, &token, org, &since_for(pr_hw.as_deref(), now)).await?;
    let (mut issues, issue_search_hw) =
        fetch_issues(client, &token, org, &since_for(issue_hw.as_deref(), now)).await?;

    let linked: Vec<String> = prs
        .iter()
        .flat_map(|pr| pr.closing_issue_ids.iter().cloned())
        .collect();
    let missing = {
        let conn = db.lock().expect("db mutex poisoned");
        let known: BTreeSet<&str> = issues.iter().map(|i| i.node_id.as_str()).collect();
        let unseen: Vec<String> = linked
            .into_iter()
            .filter(|id| !known.contains(id.as_str()))
            .collect();
        store::missing_issue_ids(&conn, &unseen)?
    };
    if !missing.is_empty() {
        issues.extend(
            fetch_by_ids(
                client,
                &token,
                &graphql::issues_by_id_query(),
                &missing,
                graphql::parse_issues_by_id,
            )
            .await
            .context("fetching PR-linked issues failed")?,
        );
    }

    let parents: Vec<String> = issues
        .iter()
        .filter(|i| i.parent_node_id.is_none() && i.sow_ref.is_some())
        .map(|i| i.node_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !parents.is_empty() {
        issues.extend(
            fetch_by_ids(
                client,
                &token,
                &graphql::sub_issues_query(),
                &parents,
                graphql::parse_sub_issues,
            )
            .await
            .context("fetching sub-issues failed")?,
        );
    }

    let warnings: BTreeSet<String> = issues
        .iter()
        .flat_map(|i| {
            i.warnings
                .iter()
                .map(move |w| format!("{}#{}: {w}", i.repo, i.number))
        })
        .collect();
    for warning in &warnings {
        eprintln!("github sync: {warning}");
    }

    {
        let conn = db.lock().expect("db mutex poisoned");
        for issue in &issues {
            store::upsert_issue(&conn, issue)?;
        }
        for pr in &prs {
            store::upsert_pr(&conn, pr)?;
        }
        // Raised only after every write above succeeded, so a failed run
        // re-fetches the same window next time rather than skipping it.
        if let Some(hw) = &pr_search_hw {
            store::raise_high_water(&conn, store::PR_HIGH_WATER, hw)?;
        }
        if let Some(hw) = &issue_search_hw {
            store::raise_high_water(&conn, store::ISSUE_HIGH_WATER, hw)?;
        }
    }

    let unresolved = {
        let conn = db.lock().expect("db mutex poisoned");
        store::members_needing_github_id(&conn)?
    };
    let mut members_resolved = 0;
    for (discord_user_id, login) in unresolved {
        match client.user_id(&token, &login).await? {
            Some(github_id) => {
                let conn = db.lock().expect("db mutex poisoned");
                store::set_member_github_id(&conn, &discord_user_id, github_id)?;
                members_resolved += 1;
            }
            None => eprintln!(
                "github sync: github_user {login:?} (member {discord_user_id}) doesn't exist on \
                 GitHub - renamed? fix it in members.toml"
            ),
        }
    }

    {
        let conn = db.lock().expect("db mutex poisoned");
        store::set_state(&conn, store::LAST_SYNC_AT, &rfc3339(now))?;
        store::clear_state(&conn, store::LAST_SYNC_ERROR)?;
    }

    Ok(SyncReport {
        prs: prs.len(),
        issues: issues.len(),
        sub_issue_parents: parents.len(),
        members_resolved,
    })
}

/// The background loop: one `sync_once` right away, then every
/// `sync_interval_minutes`. Runs for the life of the bot.
pub async fn run_loop(db: Arc<Mutex<Connection>>, github: GithubConfig, key: AppKey) {
    let client = Client::new();
    let mut tokens = TokenSource::new(&github, key);
    let mut interval = tokio::time::interval(Duration::from_secs(
        u64::from(github.sync_interval_minutes) * 60,
    ));
    loop {
        interval.tick().await;
        let now = Utc::now();
        match sync_once(&db, &client, &mut tokens, &github.org, now).await {
            Ok(report) => println!(
                "github sync: {} PRs, {} issues ({} deliverables' sub-issues), {} member ids resolved",
                report.prs, report.issues, report.sub_issue_parents, report.members_resolved
            ),
            Err(e) => {
                eprintln!("github sync failed: {e:#}");
                tokens.reset();
                let conn = db.lock().expect("db mutex poisoned");
                if let Err(e) = store::set_state(
                    &conn,
                    store::LAST_SYNC_ERROR,
                    &format!("{}: {e:#}", rfc3339(now)),
                ) {
                    eprintln!("failed to record the github sync error: {e}");
                }
            }
        }
    }
}

/// `dispatchd github preview --days N`: fetches the org's recent PRs and
/// issues with the real queries and prints what a sync would see, without
/// opening the DB - a way to confirm the integration end to end from the
/// CLI.
pub async fn preview(github: &GithubConfig, key: AppKey, days: u32) -> Result<String> {
    let client = Client::new();
    let mut tokens = TokenSource::new(github, key);
    let token = tokens.token().await?;
    let since = rfc3339(Utc::now() - chrono::Duration::days(i64::from(days)));
    let (prs, _) = fetch_prs(&client, &token, &github.org, &since).await?;
    let (issues, _) = fetch_issues(&client, &token, &github.org, &since).await?;
    Ok(format_preview(&github.org, &since, &prs, &issues))
}

#[derive(Default)]
struct AuthorCounts {
    prs_opened: usize,
    prs_merged: usize,
    reviews: usize,
}

/// Pure - `preview`'s output: org-wide totals plus a per-login breakdown,
/// sorted by login (not by volume). Only counts activity inside the window
/// (`since` and later), since a search hit only means *something* changed.
pub fn format_preview(org: &str, since: &str, prs: &[PullRequest], issues: &[Issue]) -> String {
    let in_window = |t: &Option<String>| t.as_deref().is_some_and(|t| t >= since);
    let mut by_author: BTreeMap<String, AuthorCounts> = BTreeMap::new();
    let login = |a: &graphql::Actor| a.login.clone().unwrap_or_else(|| "(ghost)".into());
    let mut reviews = 0;
    for pr in prs {
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
    let merged = prs.iter().filter(|pr| in_window(&pr.merged_at)).count();
    let issue_ids: BTreeSet<&str> = issues.iter().map(|i| i.node_id.as_str()).collect();
    let closed = issues
        .iter()
        .filter(|i| in_window(&i.closed_at))
        .map(|i| i.node_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let deliverables: BTreeSet<String> = issues
        .iter()
        .filter(|i| i.parent_node_id.is_none())
        .filter_map(|i| i.sow_ref.map(|s| s.to_string()))
        .collect();

    let mut out = format!(
        "GitHub activity in {org} since {since}:\n  {} PRs updated ({merged} merged), {} issues updated ({closed} closed), {reviews} reviews\n",
        prs.len(),
        issue_ids.len(),
    );
    if !deliverables.is_empty() {
        out.push_str(&format!(
            "  deliverables seen: {}\n",
            deliverables.into_iter().collect::<Vec<_>>().join(", ")
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
    use crate::sow::SowRef;

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

    fn issue(node_id: &str, closed: Option<&str>, sow: Option<(u32, u32)>) -> Issue {
        Issue {
            node_id: node_id.into(),
            repo: "api".into(),
            number: 1,
            title: "t".into(),
            state: if closed.is_some() { "closed" } else { "open" }.into(),
            state_reason: None,
            author: actor("sari"),
            created_at: "2026-10-01T00:00:00Z".into(),
            updated_at: "2026-10-15T00:00:00Z".into(),
            closed_at: closed.map(Into::into),
            assignee_ids: vec![],
            sow_ref: sow.map(|(m, d)| SowRef {
                milestone: m,
                deliverable: Some(d),
            }),
            parent_node_id: None,
            warnings: vec![],
        }
    }

    #[test]
    fn format_preview_counts_only_activity_inside_the_window_by_login() {
        let since = "2026-10-15T00:00:00Z";
        let prs = vec![
            // Opened before the window, merged inside it.
            pr(
                "budi-dev",
                "2026-10-10T00:00:00Z",
                Some("2026-10-15T08:00:00Z"),
                &[
                    ("rina", "2026-10-15T07:00:00Z"),
                    ("sari", "2026-10-09T00:00:00Z"),
                ],
            ),
            pr("sari", "2026-10-15T09:00:00Z", None, &[]),
        ];
        let issues = vec![
            issue("I_1", Some("2026-10-15T08:00:00Z"), None),
            issue("I_1", Some("2026-10-15T08:00:00Z"), None),
            issue("I_100", None, Some((1, 2))),
        ];

        let out = format_preview("oxgrad", since, &prs, &issues);

        assert!(
            out.contains("2 PRs updated (1 merged), 2 issues updated (1 closed), 1 reviews"),
            "{out}"
        );
        assert!(out.contains("deliverables seen: D.P.1.2"), "{out}");
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
    fn format_preview_with_nothing_found() {
        let out = format_preview("oxgrad", "2026-10-15T00:00:00Z", &[], &[]);
        assert_eq!(
            out,
            "GitHub activity in oxgrad since 2026-10-15T00:00:00Z:\n  0 PRs updated (0 merged), 0 issues updated (0 closed), 0 reviews\n"
        );
    }
}

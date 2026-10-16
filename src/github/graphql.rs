//! The GraphQL queries the sync runs, and pure parsers turning their
//! `data` into plain rows. No I/O here - `sync.rs` sends the queries
//! (`client::Client::graphql`) and `store.rs` writes the rows - so all of
//! the response handling is unit-tested against recorded-shape JSON.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::sow::{self, SowRef};

/// Search results per page; GitHub's maximum for a connection.
pub const PAGE_SIZE: u32 = 100;
/// IDs per `issues_by_id_query` lookup; GitHub's maximum for `nodes(ids:)`.
pub const NODES_BATCH: usize = 100;
/// Parents per `sub_issues_query`. Each parent can expand to 100 children
/// x (20 assignees + 30 labels), and GitHub refuses any query whose
/// worst case passes 500,000 nodes - 100 parents would be ~510k, 20 is ~102k.
pub const SUB_ISSUE_BATCH: usize = 20;
/// Children per parent in `sub_issues_query`; a list that hits this is
/// reported incomplete (`SubIssueList::complete`).
pub const SUB_ISSUES_PER_PARENT: u32 = 100;

/// GraphQL rejects a query that defines a fragment it doesn't use, so the
/// PR search (no issue fields) gets `ACTOR_FRAGMENT` alone and the issue
/// queries get both.
const ACTOR_FRAGMENT: &str = r#"
fragment ActorFields on Actor { login ... on User { databaseId } }
"#;
const ISSUE_FRAGMENT: &str = r#"
fragment IssueFields on Issue {
  id number title state stateReason createdAt updatedAt closedAt
  repository { name }
  author { ...ActorFields }
  assignees(first: 20) { nodes { databaseId } }
  labels(first: 30) { nodes { name } }
}
"#;

/// PRs in the org updated at or after `$q`'s `updated:>=` bound, oldest
/// first, with the issues each closes (the sidebar "Development" link) and
/// its reviews.
pub fn pr_search_query() -> String {
    format!(
        r#"query($q: String!, $cursor: String) {{
  search(type: ISSUE, query: $q, first: {PAGE_SIZE}, after: $cursor) {{
    pageInfo {{ hasNextPage endCursor }}
    nodes {{
      ... on PullRequest {{
        id number title state isDraft createdAt updatedAt mergedAt closedAt
        repository {{ name }}
        author {{ ...ActorFields }}
        closingIssuesReferences(first: 20) {{ nodes {{ id }} }}
        reviews(first: 100) {{ nodes {{ id state submittedAt author {{ ...ActorFields }} }} }}
      }}
    }}
  }}
}}
{ACTOR_FRAGMENT}"#
    )
}

/// An issue plus its parent, plus the parent's own parent id - without
/// that last bit, a parent riding along would be stored as top-level and
/// overwrite its real link to the deliverable above it.
const ISSUE_WITH_PARENT: &str = "...IssueFields parent { ...IssueFields parent { id } }";

/// Issues in the org updated at or after the bound, oldest first, each
/// with its parent (for sub-issues) so the parent's SOW ref is known.
pub fn issue_search_query() -> String {
    format!(
        r#"query($q: String!, $cursor: String) {{
  search(type: ISSUE, query: $q, first: {PAGE_SIZE}, after: $cursor) {{
    pageInfo {{ hasNextPage endCursor }}
    nodes {{ ... on Issue {{ {ISSUE_WITH_PARENT} }} }}
  }}
}}
{ACTOR_FRAGMENT}{ISSUE_FRAGMENT}"#
    )
}

/// Specific issues by node id - ones a PR links to that the issue search
/// didn't return (not updated recently).
pub fn issues_by_id_query() -> String {
    format!(
        r#"query($ids: [ID!]!) {{
  nodes(ids: $ids) {{ ... on Issue {{ {ISSUE_WITH_PARENT} }} }}
}}
{ACTOR_FRAGMENT}{ISSUE_FRAGMENT}"#
    )
}

/// Every sub-issue of the given parents, so a deliverable's untouched
/// sub-issues show up too, not only the ones with recent activity.
pub fn sub_issues_query() -> String {
    format!(
        r#"query($ids: [ID!]!) {{
  nodes(ids: $ids) {{
    ... on Issue {{
      id
      subIssues(first: {SUB_ISSUES_PER_PARENT}) {{ pageInfo {{ hasNextPage }} nodes {{ ...IssueFields }} }}
    }}
  }}
}}
{ACTOR_FRAGMENT}{ISSUE_FRAGMENT}"#
    )
}

/// The search string for one org's PRs or issues updated since `since`
/// (RFC3339), oldest first so a high-water mark can advance page by page.
pub fn search_string(org: &str, kind: SearchKind, since: &str) -> String {
    let kind = match kind {
        SearchKind::Pr => "is:pr",
        SearchKind::Issue => "is:issue",
    };
    format!("org:{org} {kind} updated:>={since} sort:updated-asc")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Pr,
    Issue,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Actor {
    /// `None` for bots, Mannequins and deleted accounts ("ghost").
    pub id: Option<i64>,
    pub login: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub node_id: String,
    pub repo: String,
    pub number: i64,
    pub title: String,
    /// `"open"` | `"closed"`.
    pub state: String,
    /// `"completed"` | `"not_planned"` | `"reopened"` | `"duplicate"`.
    pub state_reason: Option<String>,
    pub author: Actor,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub assignee_ids: Vec<i64>,
    /// From the issue's own `sow:` label, else a `D.P.` token in its title
    /// (`sow::from_labels` / `sow::from_title`). Only meaningful on a
    /// top-level issue - a sub-issue takes its parent's.
    pub sow_ref: Option<SowRef>,
    pub parent_node_id: Option<String>,
    /// How this row was fetched - only a `SearchHit` was necessarily
    /// updated inside the search window.
    pub origin: Origin,
    /// Problems worth logging (an unparseable or ambiguous `sow:` label).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Returned by the issue search itself.
    SearchHit,
    /// The parent of another row, riding along with it.
    Parent,
    /// Fetched by id because a PR links to it.
    ById,
    /// Fetched as part of a deliverable's full sub-issue list.
    SubIssue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub node_id: String,
    pub author: Actor,
    /// `"approved"` | `"changes_requested"` | `"commented"` | `"dismissed"`.
    pub state: String,
    pub submitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub node_id: String,
    pub repo: String,
    pub number: i64,
    pub title: String,
    /// `"open"` | `"merged"` | `"closed"`.
    pub state: String,
    pub is_draft: bool,
    pub author: Actor,
    pub created_at: String,
    pub updated_at: String,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub closing_issue_ids: Vec<String>,
    pub reviews: Vec<Review>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    /// The newest `updatedAt` among the search hits themselves - not the
    /// parents riding along with them, which didn't come from the
    /// time-ordered search, so mustn't advance the sync's high-water mark.
    pub high_water: Option<String>,
    /// The last search hit's `updatedAt`, i.e. how far through the
    /// `sort:updated-asc` order this page got - the safe resume point when
    /// paging stops early (see `sync::fetch_search`).
    pub last_updated: Option<String>,
}

/// One parent's full sub-issue list from `sub_issues_query`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubIssueList {
    pub parent_id: String,
    pub children: Vec<Issue>,
    /// `false` when the parent has more than `SUB_ISSUES_PER_PARENT`
    /// children - then the list can't be used to unlink detached ones.
    pub complete: bool,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawActor {
    login: Option<String>,
    database_id: Option<i64>,
}

#[derive(Deserialize)]
struct RawRepo {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawId {
    database_id: Option<i64>,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNodeId {
    id: String,
}

/// `IssueFields`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIssueCore {
    id: String,
    number: i64,
    title: String,
    state: String,
    state_reason: Option<String>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    repository: RawRepo,
    author: Option<RawActor>,
    assignees: Nodes<RawId>,
    labels: Nodes<RawLabel>,
}

/// A parent issue as selected by `ISSUE_WITH_PARENT`: its fields plus its
/// own parent's id.
#[derive(Deserialize)]
struct RawParentIssue {
    #[serde(flatten)]
    core: RawIssueCore,
    #[serde(default)]
    parent: Option<RawNodeId>,
}

/// A search hit / by-id issue: its fields plus its parent.
#[derive(Deserialize)]
struct RawIssue {
    #[serde(flatten)]
    core: RawIssueCore,
    #[serde(default)]
    parent: Option<RawParentIssue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    id: String,
    state: String,
    submitted_at: Option<String>,
    author: Option<RawActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequest {
    id: String,
    number: i64,
    title: String,
    state: String,
    is_draft: bool,
    created_at: String,
    updated_at: String,
    merged_at: Option<String>,
    closed_at: Option<String>,
    repository: RawRepo,
    author: Option<RawActor>,
    closing_issues_references: Nodes<RawNodeId>,
    reviews: Nodes<RawReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSearch {
    page_info: RawPageInfo,
    nodes: Vec<Value>,
}

impl From<Option<RawActor>> for Actor {
    fn from(raw: Option<RawActor>) -> Actor {
        raw.map_or_else(Actor::default, |a| Actor {
            id: a.database_id,
            login: a.login,
        })
    }
}

fn lower(s: String) -> String {
    s.to_ascii_lowercase()
}

impl RawIssueCore {
    fn into_issue(self, parent_node_id: Option<String>, origin: Origin) -> Issue {
        let labels = self.labels.nodes.iter().map(|l| l.name.as_str());
        let (sow_ref, warnings) = match sow::from_labels(labels) {
            (Some(sow_ref), warnings) => (Some(sow_ref), warnings),
            (None, warnings) => (sow::from_title(&self.title), warnings),
        };
        Issue {
            node_id: self.id,
            repo: self.repository.name,
            number: self.number,
            title: self.title,
            state: lower(self.state),
            state_reason: self.state_reason.map(lower),
            author: self.author.into(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            closed_at: self.closed_at,
            assignee_ids: self
                .assignees
                .nodes
                .into_iter()
                .filter_map(|a| a.database_id)
                .collect(),
            sow_ref,
            parent_node_id,
            origin,
            warnings,
        }
    }
}

impl RawIssue {
    /// The issue itself, plus its parent (when it has one) as a second
    /// row - the parent must be stored too, since it's the deliverable.
    fn into_issues(self, origin: Origin) -> Vec<Issue> {
        let parent_id = self.parent.as_ref().map(|p| p.core.id.clone());
        let mut out = vec![self.core.into_issue(parent_id, origin)];
        if let Some(parent) = self.parent {
            let grandparent = parent.parent.map(|g| g.id);
            out.push(parent.core.into_issue(grandparent, Origin::Parent));
        }
        out
    }
}

impl From<RawPullRequest> for PullRequest {
    fn from(raw: RawPullRequest) -> PullRequest {
        PullRequest {
            node_id: raw.id,
            repo: raw.repository.name,
            number: raw.number,
            title: raw.title,
            state: lower(raw.state),
            is_draft: raw.is_draft,
            author: raw.author.into(),
            created_at: raw.created_at,
            updated_at: raw.updated_at,
            merged_at: raw.merged_at,
            closed_at: raw.closed_at,
            closing_issue_ids: raw
                .closing_issues_references
                .nodes
                .into_iter()
                .map(|n| n.id)
                .collect(),
            // A review still being written (PENDING) has no submittedAt and
            // isn't visible to anyone else yet - skip it.
            reviews: raw
                .reviews
                .nodes
                .into_iter()
                .filter_map(|r| {
                    Some(Review {
                        node_id: r.id,
                        submitted_at: r.submitted_at?,
                        state: lower(r.state),
                        author: r.author.into(),
                    })
                })
                .filter(|r| r.state != "pending")
                .collect(),
        }
    }
}

/// Search results can mix node types; a node that isn't the type asked for
/// comes back as `{}` from the inline fragment, and a `nodes(ids:)` id that
/// no longer resolves comes back as `null` (alongside a tolerated
/// NOT_FOUND error, see `client::parse_graphql`). Both are skipped here.
fn typed_nodes<T: serde::de::DeserializeOwned>(nodes: Vec<Value>) -> Result<Vec<T>> {
    nodes
        .into_iter()
        .filter(|n| n.get("id").is_some())
        .map(|n| serde_json::from_value(n).context("unexpected node shape in GitHub response"))
        .collect()
}

fn search_page(data: Value) -> Result<RawSearch> {
    serde_json::from_value(data.get("search").cloned().unwrap_or(Value::Null))
        .context("unexpected search response shape")
}

fn page<T>(
    items: Vec<T>,
    info: RawPageInfo,
    hit_updated: impl Iterator<Item = String> + Clone,
) -> Page<T> {
    Page {
        items,
        end_cursor: info.end_cursor,
        has_next_page: info.has_next_page,
        high_water: hit_updated.clone().max(),
        last_updated: hit_updated.last(),
    }
}

pub fn parse_pr_page(data: Value) -> Result<Page<PullRequest>> {
    let raw = search_page(data)?;
    let items: Vec<PullRequest> = typed_nodes::<RawPullRequest>(raw.nodes)?
        .into_iter()
        .map(PullRequest::from)
        .collect();
    let updated: Vec<String> = items.iter().map(|pr| pr.updated_at.clone()).collect();
    Ok(page(items, raw.page_info, updated.into_iter()))
}

/// Issues on one search page, each followed by its parent (if any), so a
/// parent can appear more than once - the store upserts, so that's fine.
pub fn parse_issue_page(data: Value) -> Result<Page<Issue>> {
    let raw = search_page(data)?;
    let hits = typed_nodes::<RawIssue>(raw.nodes)?;
    let updated: Vec<String> = hits.iter().map(|i| i.core.updated_at.clone()).collect();
    let items = hits
        .into_iter()
        .flat_map(|i| i.into_issues(Origin::SearchHit))
        .collect();
    Ok(page(items, raw.page_info, updated.into_iter()))
}

fn nodes_of(data: Value) -> Vec<Value> {
    match data.get("nodes") {
        Some(Value::Array(nodes)) => nodes.clone(),
        _ => Vec::new(),
    }
}

pub fn parse_issues_by_id(data: Value) -> Result<Vec<Issue>> {
    Ok(typed_nodes::<RawIssue>(nodes_of(data))?
        .into_iter()
        .flat_map(|i| i.into_issues(Origin::ById))
        .collect())
}

/// Each parent's sub-issues, with `parent_node_id` filled in (the
/// `subIssues` connection doesn't repeat the parent on each child).
pub fn parse_sub_issues(data: Value) -> Result<Vec<SubIssueList>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawChildren {
        page_info: RawPageInfo,
        nodes: Vec<RawIssueCore>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawParent {
        id: String,
        sub_issues: RawChildren,
    }
    let parents: Vec<RawParent> = typed_nodes(nodes_of(data))?;
    Ok(parents
        .into_iter()
        .map(|parent| SubIssueList {
            complete: !parent.sub_issues.page_info.has_next_page,
            children: parent
                .sub_issues
                .nodes
                .into_iter()
                .map(|child| child.into_issue(Some(parent.id.clone()), Origin::SubIssue))
                .collect(),
            parent_id: parent.id,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn actor(login: &str, id: i64) -> Value {
        json!({ "login": login, "databaseId": id })
    }

    fn raw_issue(id: &str, number: i64, title: &str, labels: &[&str]) -> Value {
        json!({
            "id": id, "number": number, "title": title,
            "state": "OPEN", "stateReason": null,
            "createdAt": "2026-10-01T03:00:00Z", "updatedAt": "2026-10-15T09:00:00Z",
            "closedAt": null,
            "repository": { "name": "api" },
            "author": actor("sari", 2),
            "assignees": { "nodes": [{ "databaseId": 2 }, { "databaseId": null }] },
            "labels": { "nodes": labels.iter().map(|l| json!({ "name": l })).collect::<Vec<_>>() }
        })
    }

    fn search(nodes: Vec<Value>, next: bool) -> Value {
        json!({ "search": {
            "pageInfo": { "hasNextPage": next, "endCursor": if next { json!("Y3Vyc29y") } else { Value::Null } },
            "nodes": nodes
        }})
    }

    #[test]
    fn search_string_is_scoped_incremental_and_oldest_first() {
        assert_eq!(
            search_string("oxgrad", SearchKind::Pr, "2026-10-01T00:00:00Z"),
            "org:oxgrad is:pr updated:>=2026-10-01T00:00:00Z sort:updated-asc"
        );
        assert!(search_string("oxgrad", SearchKind::Issue, "x").contains("is:issue"));
    }

    #[test]
    fn queries_define_exactly_the_fragments_they_use() {
        // Every fragment a query defines must also be used, or GitHub
        // rejects the whole query.
        for query in [
            pr_search_query(),
            issue_search_query(),
            issues_by_id_query(),
            sub_issues_query(),
        ] {
            for name in ["ActorFields", "IssueFields"] {
                let defined = query.contains(&format!("fragment {name} "));
                let used = query.contains(&format!("...{name}"));
                assert_eq!(defined, used, "{name} in {query}");
            }
            assert!(query.contains("fragment ActorFields"), "{query}");
        }
        assert!(!pr_search_query().contains("fragment IssueFields"));
    }

    #[test]
    fn sub_issue_batches_stay_under_githubs_node_limit() {
        // Worst case per parent: every child with every assignee and label.
        let per_parent = 1 + SUB_ISSUES_PER_PARENT as usize * (1 + 20 + 30);
        assert!(SUB_ISSUE_BATCH * per_parent < 500_000);
    }

    #[test]
    fn parse_pr_page_reads_prs_links_and_submitted_reviews() {
        let pr = json!({
            "id": "PR_1", "number": 210, "title": "Stripe webhook handler",
            "state": "MERGED", "isDraft": false,
            "createdAt": "2026-10-14T02:00:00Z", "updatedAt": "2026-10-15T08:00:00Z",
            "mergedAt": "2026-10-15T08:00:00Z", "closedAt": "2026-10-15T08:00:00Z",
            "repository": { "name": "api" },
            "author": actor("budi-dev", 1),
            "closingIssuesReferences": { "nodes": [{ "id": "I_201" }] },
            "reviews": { "nodes": [
                { "id": "R_1", "state": "APPROVED", "submittedAt": "2026-10-15T07:00:00Z",
                  "author": actor("rina", 3) },
                { "id": "R_2", "state": "PENDING", "submittedAt": null, "author": actor("sari", 2) },
                { "id": "R_3", "state": "COMMENTED", "submittedAt": "2026-10-15T06:00:00Z",
                  "author": { "login": "dependabot" } }
            ]}
        });
        // A search for PRs can return an issue node too; it's skipped.
        let page = parse_pr_page(search(vec![pr, json!({})], true)).unwrap();

        assert!(page.has_next_page);
        assert_eq!(page.end_cursor.as_deref(), Some("Y3Vyc29y"));
        assert_eq!(page.high_water.as_deref(), Some("2026-10-15T08:00:00Z"));
        assert_eq!(page.last_updated.as_deref(), Some("2026-10-15T08:00:00Z"));
        assert_eq!(page.items.len(), 1);
        let pr = &page.items[0];
        assert_eq!(pr.state, "merged");
        assert_eq!(
            pr.author,
            Actor {
                id: Some(1),
                login: Some("budi-dev".into())
            }
        );
        assert_eq!(pr.closing_issue_ids, vec!["I_201"]);
        assert_eq!(
            pr.reviews,
            vec![
                Review {
                    node_id: "R_1".into(),
                    author: Actor {
                        id: Some(3),
                        login: Some("rina".into())
                    },
                    state: "approved".into(),
                    submitted_at: "2026-10-15T07:00:00Z".into(),
                },
                Review {
                    node_id: "R_3".into(),
                    author: Actor {
                        id: None,
                        login: Some("dependabot".into())
                    },
                    state: "commented".into(),
                    submitted_at: "2026-10-15T06:00:00Z".into(),
                },
            ]
        );
    }

    #[test]
    fn parse_pr_page_tolerates_a_deleted_author() {
        let mut pr = json!({
            "id": "PR_2", "number": 1, "title": "x", "state": "OPEN", "isDraft": true,
            "createdAt": "t", "updatedAt": "t", "mergedAt": null, "closedAt": null,
            "repository": { "name": "web" },
            "closingIssuesReferences": { "nodes": [] }, "reviews": { "nodes": [] }
        });
        pr["author"] = Value::Null;
        let page = parse_pr_page(search(vec![pr], false)).unwrap();
        assert_eq!(page.items[0].author, Actor::default());
        assert!(page.items[0].is_draft);
        assert!(!page.has_next_page);
    }

    #[test]
    fn parse_issue_page_emits_the_parent_with_its_sow_ref() {
        let mut child = raw_issue("I_201", 201, "Stripe webhook handler", &["bug"]);
        child["state"] = json!("CLOSED");
        child["stateReason"] = json!("COMPLETED");
        child["closedAt"] = json!("2026-10-15T08:00:00Z");
        child["parent"] = raw_issue(
            "I_100",
            100,
            "Payment gateway integration",
            &["sow:D.P.1.2"],
        );

        let page = parse_issue_page(search(vec![child], false)).unwrap();

        assert_eq!(page.items.len(), 2);
        let (child, parent) = (&page.items[0], &page.items[1]);
        assert_eq!(child.node_id, "I_201");
        assert_eq!(child.origin, Origin::SearchHit);
        assert_eq!(child.state, "closed");
        assert_eq!(child.state_reason.as_deref(), Some("completed"));
        assert_eq!(child.parent_node_id.as_deref(), Some("I_100"));
        assert_eq!(child.assignee_ids, vec![2]);
        assert_eq!(child.sow_ref, None);
        assert_eq!(parent.node_id, "I_100");
        assert_eq!(parent.origin, Origin::Parent);
        assert_eq!(parent.parent_node_id, None);
        assert_eq!(parent.sow_ref.unwrap().to_string(), "D.P.1.2");
    }

    #[test]
    fn a_riding_along_parent_keeps_its_own_parent_link() {
        // deliverable D <- task T <- subtask S; only S was updated.
        let mut task = raw_issue("I_T", 2, "D.P.1.2 Stripe webhook", &[]);
        task["parent"] = json!({ "id": "I_D" });
        let mut subtask = raw_issue("I_S", 3, "Retry logic", &[]);
        subtask["parent"] = task;

        let page = parse_issue_page(search(vec![subtask], false)).unwrap();

        let task = page.items.iter().find(|i| i.node_id == "I_T").unwrap();
        assert_eq!(task.parent_node_id.as_deref(), Some("I_D"));
    }

    #[test]
    fn high_water_ignores_parents_riding_along() {
        let mut parent = raw_issue("I_100", 100, "Payment", &["sow:D.P.1.2"]);
        parent["updatedAt"] = json!("2026-10-16T00:00:00Z");
        let mut hit = raw_issue("I_201", 201, "Stripe", &[]);
        hit["parent"] = parent;
        let page = parse_issue_page(search(vec![hit], false)).unwrap();
        assert_eq!(page.high_water.as_deref(), Some("2026-10-15T09:00:00Z"));
    }

    #[test]
    fn last_updated_is_the_last_hit_in_sort_order_not_the_max() {
        let mut first = raw_issue("I_1", 1, "a", &[]);
        first["updatedAt"] = json!("2026-10-15T10:00:00Z");
        let mut second = raw_issue("I_2", 2, "b", &[]);
        second["updatedAt"] = json!("2026-10-15T09:00:00Z");
        let page = parse_issue_page(search(vec![first, second], true)).unwrap();
        assert_eq!(page.high_water.as_deref(), Some("2026-10-15T10:00:00Z"));
        assert_eq!(page.last_updated.as_deref(), Some("2026-10-15T09:00:00Z"));
    }

    #[test]
    fn issue_sow_ref_falls_back_to_the_title_and_keeps_label_warnings() {
        let issue = raw_issue("I_1", 1, "D.P.1.3 Admin dashboard", &["sow:TBD"]);
        let page = parse_issue_page(search(vec![issue], false)).unwrap();
        assert_eq!(page.items[0].sow_ref.unwrap().to_string(), "D.P.1.3");
        assert_eq!(page.items[0].warnings.len(), 1);
    }

    #[test]
    fn parse_issues_by_id_drops_unresolvable_ids() {
        let data = json!({ "nodes": [raw_issue("I_1", 1, "x", &[]), null] });
        let issues = parse_issues_by_id(data).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].node_id, "I_1");
        assert_eq!(issues[0].origin, Origin::ById);
    }

    #[test]
    fn parse_sub_issues_links_each_child_to_its_parent() {
        let data = json!({ "nodes": [
            { "id": "I_100", "subIssues": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [
                    raw_issue("I_201", 201, "Stripe webhook handler", &[]),
                    raw_issue("I_207", 207, "Invoice PDF", &[])
                ]}},
            { "id": "I_300", "subIssues": {
                "pageInfo": { "hasNextPage": true },
                "nodes": [] }},
            null
        ]});
        let lists = parse_sub_issues(data).unwrap();
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].parent_id, "I_100");
        assert!(lists[0].complete);
        assert_eq!(lists[0].children.len(), 2);
        assert!(
            lists[0]
                .children
                .iter()
                .all(|i| i.parent_node_id.as_deref() == Some("I_100"))
        );
        assert!(!lists[1].complete);
    }

    #[test]
    fn a_missing_search_object_is_an_error_not_an_empty_page() {
        assert!(parse_pr_page(json!({})).is_err());
    }
}

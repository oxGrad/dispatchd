//! The GitHub REST calls behind App authentication: who the App is, where
//! it's installed, minting an installation token, and how many repos that
//! token can see. The response parsing (`parse_*`, `describe_status`) is
//! pure and unit-tested; the `reqwest` calls themselves need a real App and
//! org, same "confirm end-to-end yourself" rule as `upgrade.rs`'s I/O.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde::Deserialize;

use super::auth::{AppKey, InstallationToken};
use crate::upgrade::{http_client, user_agent};

const API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppInfo {
    pub id: u64,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Installation {
    pub id: u64,
    /// `"all"` or `"selected"` - which repos the org admin granted.
    pub repository_selection: String,
}

#[derive(Deserialize)]
struct RawAccessToken {
    token: String,
    expires_at: String,
}

#[derive(Deserialize)]
struct RawRepoCount {
    total_count: u64,
}

/// Everything `github login` and `dispatchd status` report about the App.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCheck {
    pub app: AppInfo,
    pub installation: Installation,
    pub repo_count: u64,
    pub latency_ms: u128,
}

pub fn parse_app(body: &str) -> Result<AppInfo> {
    serde_json::from_str(body).context("unexpected response from GET /app")
}

pub fn parse_installation(body: &str) -> Result<Installation> {
    serde_json::from_str(body).context("unexpected response from GET /orgs/{org}/installation")
}

pub fn parse_access_token(body: &str) -> Result<InstallationToken> {
    let raw: RawAccessToken = serde_json::from_str(body)
        .context("unexpected response from POST /app/installations/{id}/access_tokens")?;
    let expires_at = DateTime::parse_from_rfc3339(&raw.expires_at)
        .with_context(|| format!("unexpected token expiry {:?}", raw.expires_at))?
        .timestamp();
    Ok(InstallationToken {
        token: raw.token,
        expires_at,
    })
}

pub fn parse_repo_count(body: &str) -> Result<u64> {
    let raw: RawRepoCount = serde_json::from_str(body)
        .context("unexpected response from GET /installation/repositories")?;
    Ok(raw.total_count)
}

/// Turns a non-2xx status from one of the calls above into an operator-
/// facing explanation - the raw "401 Unauthorized" alone doesn't say
/// whether the key, the App ID, or the clock is wrong.
pub fn describe_status(step: Step, status: u16, org: &str) -> String {
    match (step, status) {
        (Step::App, 401) => "GitHub rejected the App JWT (401) - check app_id in config.toml \
                             matches the App the key was generated for, and that this \
                             machine's clock is correct"
            .to_string(),
        (Step::Installation, 404) => format!(
            "the App is not installed on the {org:?} org (404) - install it from the App's \
             settings page (\"Install App\"), or fix `org` in config.toml"
        ),
        (Step::Token, 403) | (Step::Token, 404) => format!(
            "GitHub refused to issue an installation token ({status}) - the installation may \
             be suspended; check the App's installation on {org:?}"
        ),
        (Step::Repos, 403) => "the installation token can't list repositories (403) - make \
                               sure the App has the Metadata: read permission"
            .to_string(),
        (Step::Graphql, 401) => "GitHub rejected the installation token (401) - it may have \
                                 expired mid-sync; the next sync mints a fresh one"
            .to_string(),
        (Step::Graphql, 403) => "GitHub refused the GraphQL query (403) - usually a rate \
                                 limit; the next sync retries"
            .to_string(),
        (_, status) => format!("{} failed with HTTP {status}", step.endpoint()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    App,
    Installation,
    Token,
    Repos,
    Graphql,
    User,
}

impl Step {
    fn endpoint(self) -> &'static str {
        match self {
            Step::App => "GET /app",
            Step::Installation => "GET /orgs/{org}/installation",
            Step::Token => "POST /app/installations/{id}/access_tokens",
            Step::Repos => "GET /installation/repositories",
            Step::Graphql => "POST /graphql",
            Step::User => "GET /users/{login}",
        }
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Client {
        Client {
            http: http_client(Duration::from_secs(20)),
        }
    }

    /// Every GitHub call goes through here: the headers GitHub requires
    /// plus the bearer token, sent, with a transport failure tagged by
    /// `step`. The status is left to the caller.
    async fn request(
        &self,
        request: reqwest::RequestBuilder,
        bearer: &str,
        step: Step,
    ) -> Result<reqwest::Response> {
        request
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .bearer_auth(bearer)
            .send()
            .await
            .with_context(|| format!("{} failed - is GitHub reachable?", step.endpoint()))
    }

    async fn body(response: reqwest::Response, step: Step) -> Result<String> {
        response
            .text()
            .await
            .with_context(|| format!("failed to read the {} response", step.endpoint()))
    }

    /// `request` plus "any non-2xx is an error", returning the body.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        bearer: &str,
        step: Step,
        org: &str,
    ) -> Result<String> {
        let response = self.request(request, bearer, step).await?;
        let status = response.status();
        if !status.is_success() {
            bail!(describe_status(step, status.as_u16(), org));
        }
        Self::body(response, step).await
    }

    pub async fn app(&self, jwt: &str) -> Result<AppInfo> {
        let body = self
            .send(self.http.get(format!("{API}/app")), jwt, Step::App, "")
            .await?;
        parse_app(&body)
    }

    pub async fn org_installation(&self, jwt: &str, org: &str) -> Result<Installation> {
        let url = format!("{API}/orgs/{org}/installation");
        let body = self
            .send(self.http.get(url), jwt, Step::Installation, org)
            .await?;
        parse_installation(&body)
    }

    pub async fn installation_token(
        &self,
        jwt: &str,
        installation_id: u64,
        org: &str,
    ) -> Result<InstallationToken> {
        let url = format!("{API}/app/installations/{installation_id}/access_tokens");
        let body = self
            .send(self.http.post(url), jwt, Step::Token, org)
            .await?;
        parse_access_token(&body)
    }

    pub async fn repo_count(&self, token: &str, org: &str) -> Result<u64> {
        let url = format!("{API}/installation/repositories?per_page=1");
        let body = self
            .send(self.http.get(url), token, Step::Repos, org)
            .await?;
        parse_repo_count(&body)
    }

    /// Runs one GraphQL query with the installation `token` (see
    /// `parse_graphql` for which errors fail it).
    pub async fn graphql(
        &self,
        token: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<GraphqlResponse> {
        let request = self
            .http
            .post(format!("{API}/graphql"))
            .json(&serde_json::json!({ "query": query, "variables": variables }));
        let body = self.send(request, token, Step::Graphql, "").await?;
        parse_graphql(&body)
    }

    /// A GitHub username's numeric user id, `None` if no such user (renamed
    /// or deleted). Usernames are public, so any token works.
    pub async fn user_id(&self, token: &str, login: &str) -> Result<Option<i64>> {
        let request = self.http.get(format!("{API}/users/{login}"));
        let response = self.request(request, token, Step::User).await?;
        match response.status().as_u16() {
            404 => Ok(None),
            status if !(200..300).contains(&status) => {
                bail!(describe_status(Step::User, status, ""))
            }
            _ => Ok(Some(parse_user_id(
                &Self::body(response, Step::User).await?,
            )?)),
        }
    }
}

#[derive(Deserialize)]
struct RawGraphql {
    data: Option<serde_json::Value>,
    #[serde(default)]
    errors: Vec<RawGraphqlError>,
}

#[derive(Deserialize)]
struct RawGraphqlError {
    message: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    path: Vec<serde_json::Value>,
}

/// A GraphQL call's `data`, plus the node-level errors it was allowed to
/// carry (see `parse_graphql`), for the caller to log.
#[derive(Debug, PartialEq)]
pub struct GraphqlResponse {
    pub data: serde_json::Value,
    pub skipped: Vec<String>,
}

/// How many error messages an error string quotes before "(+N more)" - a
/// page of 100 failing nodes must not become a multi-KB error that blows
/// past Discord's message limit in `/admin status`.
const MAX_QUOTED_ERRORS: usize = 3;

fn summarize_errors(errors: &[RawGraphqlError]) -> String {
    let mut out = errors
        .iter()
        .take(MAX_QUOTED_ERRORS)
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    if errors.len() > MAX_QUOTED_ERRORS {
        out.push_str(&format!(" (+{} more)", errors.len() - MAX_QUOTED_ERRORS));
    }
    out
}

/// Parses a GraphQL response body. GitHub answers query errors with HTTP
/// 200 and an `errors` array, sometimes alongside partial `data`:
///
/// - A `FORBIDDEN` or `NOT_FOUND` error pinned to a field path (e.g. a
///   sub-issue whose parent lives in a repo the App isn't installed on, or a
///   `nodes(ids:)` id that was deleted) only nulls that one field. It's
///   tolerated - the rest of the page is good - and reported in `skipped`.
///   Failing the whole call instead would refetch the same item and fail
///   again on every run, stalling the sync for good.
/// - Any other error (a malformed query, a rate limit, no `data` at all)
///   fails the call, since the page can't be trusted.
pub fn parse_graphql(body: &str) -> Result<GraphqlResponse> {
    let raw: RawGraphql =
        serde_json::from_str(body).context("unexpected response from POST /graphql")?;
    let node_level = |e: &RawGraphqlError| {
        matches!(e.kind.as_deref(), Some("FORBIDDEN" | "NOT_FOUND")) && !e.path.is_empty()
    };
    let (skipped, fatal): (Vec<_>, Vec<_>) = raw.errors.into_iter().partition(node_level);
    if !fatal.is_empty() {
        bail!("GitHub GraphQL error: {}", summarize_errors(&fatal));
    }
    let data = raw
        .data
        .filter(|d| !d.is_null())
        .context("GitHub GraphQL response had no data")?;
    Ok(GraphqlResponse {
        data,
        skipped: skipped.into_iter().map(|e| e.message).collect(),
    })
}

pub fn parse_user_id(body: &str) -> Result<i64> {
    #[derive(Deserialize)]
    struct RawUser {
        id: i64,
    }
    let raw: RawUser =
        serde_json::from_str(body).context("unexpected response from GET /users/{login}")?;
    Ok(raw.id)
}

/// The full round-trip `github login` validates with and `dispatchd
/// status` reports: App JWT -> `/app` -> the org's installation -> an
/// installation token -> how many repos it sees. Proves the key, the App
/// ID, the org, the installation, and the Metadata permission in one go.
pub async fn check(app_id: u64, org: &str, key: &AppKey) -> Result<AppCheck> {
    let started = Instant::now();
    let client = Client::new();
    let jwt = key.app_jwt(app_id, chrono::Utc::now().timestamp())?;
    let app = client.app(&jwt).await?;
    let installation = client.org_installation(&jwt, org).await?;
    let token = client
        .installation_token(&jwt, installation.id, org)
        .await?;
    let repo_count = client.repo_count(&token.token, org).await?;
    Ok(AppCheck {
        app,
        installation,
        repo_count,
        latency_ms: started.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_app_reads_id_slug_and_name() {
        let body = r#"{"id": 123456, "slug": "dispatchd-oxgrad", "name": "dispatchd (oxgrad)",
                       "owner": {"login": "oxgrad"}, "permissions": {"issues": "read"}}"#;
        assert_eq!(
            parse_app(body).unwrap(),
            AppInfo {
                id: 123456,
                slug: "dispatchd-oxgrad".into(),
                name: "dispatchd (oxgrad)".into(),
            }
        );
    }

    #[test]
    fn parse_installation_reads_id_and_selection() {
        let body =
            r#"{"id": 987, "account": {"login": "oxgrad"}, "repository_selection": "selected"}"#;
        assert_eq!(
            parse_installation(body).unwrap(),
            Installation {
                id: 987,
                repository_selection: "selected".into(),
            }
        );
    }

    #[test]
    fn parse_access_token_converts_expiry_to_unix_seconds() {
        let body = r#"{"token": "ghs_abc", "expires_at": "2026-10-15T10:00:00Z",
                       "permissions": {"issues": "read"}}"#;
        assert_eq!(
            parse_access_token(body).unwrap(),
            InstallationToken {
                token: "ghs_abc".into(),
                expires_at: 1_792_058_400,
            }
        );
    }

    #[test]
    fn parse_access_token_rejects_a_bad_expiry() {
        let body = r#"{"token": "ghs_abc", "expires_at": "soon"}"#;
        assert!(parse_access_token(body).is_err());
    }

    #[test]
    fn parse_repo_count_reads_total_count() {
        let body = r#"{"total_count": 14, "repositories": [{"name": "api"}]}"#;
        assert_eq!(parse_repo_count(body).unwrap(), 14);
    }

    #[test]
    fn parse_graphql_returns_data() {
        let response = parse_graphql(r#"{"data": {"search": {"nodes": []}}}"#).unwrap();
        assert_eq!(response.data["search"]["nodes"], serde_json::json!([]));
        assert!(response.skipped.is_empty());
    }

    #[test]
    fn parse_graphql_fails_on_a_query_level_error_even_with_partial_data() {
        let body = r#"{"data": {"search": null},
                       "errors": [{"message": "Field 'subIssues' doesn't exist"},
                                  {"message": "second"}]}"#;
        let err = parse_graphql(body).unwrap_err().to_string();
        assert!(
            err.contains("Field 'subIssues' doesn't exist; second"),
            "{err}"
        );
    }

    #[test]
    fn parse_graphql_tolerates_node_level_forbidden_and_not_found() {
        let body = r#"{"data": {"nodes": [{"id": "I_1"}, null]},
                       "errors": [
                         {"type": "NOT_FOUND", "path": ["nodes", 1],
                          "message": "Could not resolve to a node with the global id of 'I_gone'"},
                         {"type": "FORBIDDEN", "path": ["search", "nodes", 0, "parent"],
                          "message": "Resource not accessible by integration"}]}"#;
        let response = parse_graphql(body).unwrap();
        assert_eq!(response.data["nodes"][0]["id"], "I_1");
        assert_eq!(response.skipped.len(), 2);
    }

    #[test]
    fn parse_graphql_fails_on_forbidden_without_a_path_or_without_data() {
        let no_path = r#"{"data": {"search": null},
                          "errors": [{"type": "FORBIDDEN", "message": "nope"}]}"#;
        assert!(parse_graphql(no_path).is_err());
        let no_data = r#"{"data": null,
                          "errors": [{"type": "NOT_FOUND", "path": ["x"], "message": "gone"}]}"#;
        assert!(parse_graphql(no_data).is_err());
    }

    #[test]
    fn parse_graphql_caps_how_many_errors_it_quotes() {
        let errors: Vec<String> = (0..50)
            .map(|i| format!(r#"{{"type": "INTERNAL", "message": "boom {i}"}}"#))
            .collect();
        let body = format!(r#"{{"data": null, "errors": [{}]}}"#, errors.join(","));
        let err = parse_graphql(&body).unwrap_err().to_string();
        assert!(err.ends_with("boom 0; boom 1; boom 2 (+47 more)"), "{err}");
    }

    #[test]
    fn parse_graphql_fails_without_data() {
        assert!(parse_graphql(r#"{"errors": []}"#).is_err());
    }

    #[test]
    fn parse_user_id_reads_the_numeric_id() {
        assert_eq!(
            parse_user_id(r#"{"login": "budi-dev", "id": 583231, "type": "User"}"#).unwrap(),
            583231
        );
    }

    #[test]
    fn describe_status_explains_the_common_failures() {
        assert!(describe_status(Step::App, 401, "oxgrad").contains("app_id"));
        assert!(describe_status(Step::Installation, 404, "oxgrad").contains("not installed"));
        assert!(describe_status(Step::Token, 403, "oxgrad").contains("suspended"));
        assert!(describe_status(Step::Repos, 403, "oxgrad").contains("Metadata"));
        assert_eq!(
            describe_status(Step::App, 500, "oxgrad"),
            "GET /app failed with HTTP 500"
        );
    }
}

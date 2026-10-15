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
        (_, status) => format!("{} failed with HTTP {status}", step.endpoint()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    App,
    Installation,
    Token,
    Repos,
}

impl Step {
    fn endpoint(self) -> &'static str {
        match self {
            Step::App => "GET /app",
            Step::Installation => "GET /orgs/{org}/installation",
            Step::Token => "POST /app/installations/{id}/access_tokens",
            Step::Repos => "GET /installation/repositories",
        }
    }
}

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Client {
        Client {
            http: http_client(Duration::from_secs(20)),
        }
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        bearer: &str,
        step: Step,
        org: &str,
    ) -> Result<String> {
        let response = request
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .bearer_auth(bearer)
            .send()
            .await
            .with_context(|| format!("{} failed - is GitHub reachable?", step.endpoint()))?;
        let status = response.status();
        if !status.is_success() {
            bail!(describe_status(step, status.as_u16(), org));
        }
        response
            .text()
            .await
            .with_context(|| format!("failed to read the {} response", step.endpoint()))
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

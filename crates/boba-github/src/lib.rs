//! # boba-github
//!
//! A small GitHub REST client for Boba Agent's repo sync (owned by the runner workstream):
//! list open issues (label filter) and pull requests (drafts optional), fetch one PR's head
//! branch, comment on issues/PRs, submit PR reviews, close issues, and verify webhooks.
//!
//! Auth: a token from `GH_TOKEN` / `GITHUB_TOKEN`, or `gh auth token` as a fallback
//! ([`GitHub::from_env`]). TLS is rustls; no OpenSSL needed.
//!
//! ```no_run
//! # async fn demo() -> Result<(), boba_github::Error> {
//! use boba_github::{GitHub, RepoRef, ReviewEvent};
//! let gh = GitHub::from_env().await?;
//! let repo = RepoRef::parse("Flotapponnier/boba-agent").unwrap();
//! for pr in gh.list_open_pulls(&repo, true).await? {
//!     println!("#{} {} ({})", pr.number, pr.title, pr.head_branch);
//! }
//! gh.submit_review(&repo, 9, ReviewEvent::Approve, "LGTM from Boba").await?;
//! # Ok(()) }
//! ```

mod types;
pub mod webhook;

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

pub use types::{CloseReason, Issue, Posted, PullRequest, RepoRef, ReviewEvent};
pub use webhook::{parse_event, verify_signature, WebhookEvent};

use types::{RawIssue, RawPosted, RawPull};

pub const DEFAULT_API: &str = "https://api.github.com";
/// Hard cap on pages fetched by list calls (100 items per page).
pub const MAX_PAGES: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no GitHub token: set GH_TOKEN or run `gh auth login`")]
    NoToken,
    #[error("GitHub rejected the token (401): {0}")]
    Unauthorized(String),
    #[error("GitHub rate limit reached, resets at {reset_at}")]
    RateLimited { reset_at: DateTime<Utc> },
    #[error("not found (404): {0}")]
    NotFound(String),
    #[error("GitHub API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected GitHub response: {0}")]
    Decode(String),
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_decode() {
            Error::Decode(e.to_string())
        } else {
            Error::Network(e.to_string())
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    api: String,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    pub fn new(token: &str) -> Result<Self> {
        Self::with_api(token, DEFAULT_API)
    }

    /// `api` = base URL, e.g. `https://ghe.example.com/api/v3` for GitHub Enterprise.
    pub fn with_api(token: &str, api: &str) -> Result<Self> {
        let token = token.trim();
        if token.is_empty() {
            return Err(Error::NoToken);
        }
        let mut headers = HeaderMap::new();
        let mut auth =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::NoToken)?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("boba-agent/", env!("CARGO_PKG_VERSION"))),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Self {
            http,
            api: api.trim_end_matches('/').to_string(),
        })
    }

    /// Token from `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token`.
    pub async fn from_env() -> Result<Self> {
        Self::new(&token_from_env().await.ok_or(Error::NoToken)?)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api)
    }

    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let mut req = self.http.request(method, url);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let headers = resp.headers().clone();
        let text = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| text.chars().take(300).collect());
        let remaining = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok());
        let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN
                && (remaining == Some("0") || message.to_lowercase().contains("rate limit")));
        Err(if rate_limited {
            let reset_at = headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
                .and_then(|t| Utc.timestamp_opt(t, 0).single())
                .or_else(|| {
                    headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<i64>().ok())
                        .map(|s| Utc::now() + chrono::Duration::seconds(s))
                })
                .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(60));
            Error::RateLimited { reset_at }
        } else {
            match status {
                StatusCode::UNAUTHORIZED => Error::Unauthorized(message),
                StatusCode::NOT_FOUND => Error::NotFound(message),
                s => Error::Api {
                    status: s.as_u16(),
                    message,
                },
            }
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .send(Method::GET, &self.url(path), None)
            .await?
            .json()
            .await?)
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Value,
    ) -> Result<T> {
        Ok(self
            .send(method, &self.url(path), Some(&body))
            .await?
            .json()
            .await?)
    }

    /// GET every page (following `Link: rel="next"`), up to [`MAX_PAGES`].
    async fn get_pages<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(self.url(path));
        let mut pages = 0;
        while let Some(url) = next.take() {
            let resp = self.send(Method::GET, &url, None).await?;
            next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let mut items: Vec<T> = resp.json().await?;
            out.append(&mut items);
            pages += 1;
            if pages >= MAX_PAGES {
                if next.is_some() {
                    tracing::warn!("{path}: stopped after {MAX_PAGES} pages");
                }
                break;
            }
        }
        Ok(out)
    }

    /// Open issues (pull requests excluded), oldest first. With `labels`, issues that carry
    /// at least one of them (GitHub's own `labels=` filter is AND, so this filters locally).
    pub async fn list_open_issues(&self, repo: &RepoRef, labels: &[String]) -> Result<Vec<Issue>> {
        let path = format!(
            "/repos/{}/{}/issues?state=open&sort=created&direction=asc&per_page=100",
            repo.owner, repo.name
        );
        let raw: Vec<RawIssue> = self.get_pages(&path).await?;
        let wanted: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
        Ok(raw
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .map(Issue::from)
            .filter(|i| {
                wanted.is_empty() || i.labels.iter().any(|l| wanted.contains(&l.to_lowercase()))
            })
            .collect())
    }

    /// Open pull requests (including forks), oldest first.
    pub async fn list_open_pulls(
        &self,
        repo: &RepoRef,
        skip_drafts: bool,
    ) -> Result<Vec<PullRequest>> {
        let path = format!(
            "/repos/{}/{}/pulls?state=open&sort=created&direction=asc&per_page=100",
            repo.owner, repo.name
        );
        let raw: Vec<RawPull> = self.get_pages(&path).await?;
        Ok(raw
            .into_iter()
            .map(PullRequest::from)
            .filter(|p| !(skip_drafts && p.draft))
            .collect())
    }

    pub async fn get_pull(&self, repo: &RepoRef, number: i64) -> Result<PullRequest> {
        let raw: RawPull = self
            .get_json(&format!(
                "/repos/{}/{}/pulls/{number}",
                repo.owner, repo.name
            ))
            .await?;
        Ok(raw.into())
    }

    pub async fn get_issue(&self, repo: &RepoRef, number: i64) -> Result<Issue> {
        let raw: RawIssue = self
            .get_json(&format!(
                "/repos/{}/{}/issues/{number}",
                repo.owner, repo.name
            ))
            .await?;
        Ok(raw.into())
    }

    /// Comment on an issue or a pull request (same endpoint).
    pub async fn comment(&self, repo: &RepoRef, number: i64, body: &str) -> Result<Posted> {
        let raw: RawPosted = self
            .post_json(
                Method::POST,
                &format!(
                    "/repos/{}/{}/issues/{number}/comments",
                    repo.owner, repo.name
                ),
                json!({ "body": body }),
            )
            .await?;
        Ok(Posted {
            id: raw.id,
            url: raw.html_url,
        })
    }

    /// Submit a PR review. GitHub refuses approving or requesting changes on your own PR
    /// (422); that comes back as `Error::Api { status: 422, .. }`.
    pub async fn submit_review(
        &self,
        repo: &RepoRef,
        number: i64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<Posted> {
        let raw: RawPosted = self
            .post_json(
                Method::POST,
                &format!("/repos/{}/{}/pulls/{number}/reviews", repo.owner, repo.name),
                json!({ "event": event.as_api(), "body": body }),
            )
            .await?;
        Ok(Posted {
            id: raw.id,
            url: raw.html_url,
        })
    }

    pub async fn close_issue(
        &self,
        repo: &RepoRef,
        number: i64,
        reason: CloseReason,
    ) -> Result<()> {
        let reason = match reason {
            CloseReason::Completed => "completed",
            CloseReason::NotPlanned => "not_planned",
        };
        let _: Value = self
            .post_json(
                Method::PATCH,
                &format!("/repos/{}/{}/issues/{number}", repo.owner, repo.name),
                json!({ "state": "closed", "state_reason": reason }),
            )
            .await?;
        Ok(())
    }
}

/// The `rel="next"` URL of a `Link` header.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let mut segs = part.split(';');
        let url = segs.next()?.trim().strip_prefix('<')?.strip_suffix('>')?;
        segs.any(|s| s.trim() == "rel=\"next\"")
            .then(|| url.to_string())
    })
}

/// `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token` (if `gh` is installed and logged in).
pub async fn token_from_env() -> Option<String> {
    for k in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    let out = tokio::process::Command::new("gh")
        .args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), out)
        .await
        .ok()?
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests;

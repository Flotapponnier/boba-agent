//! The parts of GitHub's REST payloads the repo sync needs, flattened.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `owner/name` of a GitHub repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// Accepts `owner/name`, `https://github.com/owner/name(.git)`,
    /// `git@github.com:owner/name.git` and `ssh://git@github.com/owner/name`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let rest = if let Some(r) = s.strip_prefix("git@github.com:") {
            r
        } else if let Some((_, r)) = s.split_once("github.com/") {
            r
        } else if !s.contains(':') && s.matches('/').count() == 1 {
            s
        } else {
            return None;
        };
        let rest = rest.trim_end_matches('/').trim_end_matches(".git");
        let mut parts = rest.split('/');
        let (owner, name) = (parts.next()?, parts.next()?);
        let ok = |p: &str| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        };
        (ok(owner) && ok(name)).then(|| Self::new(owner, name))
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub labels: Vec<String>,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub labels: Vec<String>,
    pub state: String,
    pub draft: bool,
    /// Branch name on the head repo (`head.ref`).
    pub head_branch: String,
    pub head_sha: String,
    /// `owner/name` of the head repo; differs from the base repo for forks. None when the
    /// fork was deleted.
    pub head_repo: Option<String>,
    pub is_fork: bool,
    pub base_branch: String,
    pub merged: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A created comment or review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Posted {
    pub id: i64,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    pub fn as_api(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseReason {
    Completed,
    NotPlanned,
}

// ------------------------------------------------------------------ raw API shapes

#[derive(Deserialize)]
pub(crate) struct RawUser {
    pub login: String,
}

#[derive(Deserialize)]
pub(crate) struct RawLabel {
    pub name: String,
}

#[derive(Deserialize)]
pub(crate) struct RawIssue {
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub html_url: String,
    pub user: Option<RawUser>,
    #[serde(default)]
    pub labels: Vec<RawLabel>,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Present when the "issue" is a pull request.
    pub pull_request: Option<serde_json::Value>,
}

impl From<RawIssue> for Issue {
    fn from(r: RawIssue) -> Self {
        Issue {
            number: r.number,
            title: r.title,
            body: r.body.unwrap_or_default(),
            url: r.html_url,
            author: r.user.map(|u| u.login).unwrap_or_else(|| "ghost".into()),
            labels: r.labels.into_iter().map(|l| l.name).collect(),
            state: r.state,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct RawRepo {
    pub full_name: String,
}

#[derive(Deserialize)]
pub(crate) struct RawRef {
    #[serde(rename = "ref")]
    pub branch: String,
    pub sha: String,
    pub repo: Option<RawRepo>,
}

#[derive(Deserialize)]
pub(crate) struct RawPull {
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub html_url: String,
    pub user: Option<RawUser>,
    #[serde(default)]
    pub labels: Vec<RawLabel>,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub head: RawRef,
    pub base: RawRef,
    pub merged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<RawPull> for PullRequest {
    fn from(r: RawPull) -> Self {
        let head_repo = r.head.repo.map(|x| x.full_name);
        let base_repo = r.base.repo.map(|x| x.full_name);
        PullRequest {
            number: r.number,
            title: r.title,
            body: r.body.unwrap_or_default(),
            url: r.html_url,
            author: r.user.map(|u| u.login).unwrap_or_else(|| "ghost".into()),
            labels: r.labels.into_iter().map(|l| l.name).collect(),
            state: r.state,
            draft: r.draft,
            head_branch: r.head.branch,
            head_sha: r.head.sha,
            is_fork: head_repo.is_none() || head_repo != base_repo,
            head_repo,
            base_branch: r.base.branch,
            merged: r.merged_at.is_some(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct RawPosted {
    pub id: i64,
    pub html_url: String,
}

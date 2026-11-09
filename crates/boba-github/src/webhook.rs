//! GitHub webhooks: signature check and the events the repo sync reacts to.

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::types::{Issue, PullRequest, RawIssue, RawPull, RepoRef};
use crate::Error;

/// Check `X-Hub-Signature-256` (`sha256=<hex>`) against the raw request body, in
/// constant time. Never parse the body before this returns true.
pub fn verify_signature(secret: &[u8], body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(sig) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

/// `sha256=<hex>` for `body`, e.g. to sign test deliveries.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum WebhookEvent {
    /// Sent once when the webhook is created.
    Ping { zen: String },
    /// `issues` event (opened, edited, closed, reopened, labeled, unlabeled, …).
    Issue {
        repo: RepoRef,
        action: String,
        issue: Issue,
    },
    /// `pull_request` event (opened, edited, closed, reopened, synchronize,
    /// ready_for_review, converted_to_draft, …).
    PullRequest {
        repo: RepoRef,
        action: String,
        pull: PullRequest,
    },
    /// Anything else: just trigger a sync if you care.
    Other {
        event: String,
        repo: Option<RepoRef>,
        action: Option<String>,
    },
}

#[derive(Deserialize)]
struct Envelope {
    action: Option<String>,
    repository: Option<RepoField>,
    zen: Option<String>,
    issue: Option<RawIssue>,
    pull_request: Option<RawPull>,
}

#[derive(Deserialize)]
struct RepoField {
    full_name: String,
}

/// Parse a delivery. `event` is the `X-GitHub-Event` header. Issue events about pull
/// requests (GitHub sends both) are reported as `Other`.
pub fn parse_event(event: &str, body: &[u8]) -> Result<WebhookEvent, Error> {
    let env: Envelope = serde_json::from_slice(body).map_err(|e| Error::Decode(e.to_string()))?;
    let repo = env
        .repository
        .as_ref()
        .and_then(|r| RepoRef::parse(&r.full_name));
    Ok(match (event, env.issue, env.pull_request, repo) {
        ("ping", ..) => WebhookEvent::Ping {
            zen: env.zen.unwrap_or_default(),
        },
        ("issues", Some(issue), _, Some(repo)) if issue.pull_request.is_none() => {
            WebhookEvent::Issue {
                repo,
                action: env.action.unwrap_or_default(),
                issue: issue.into(),
            }
        }
        ("pull_request", _, Some(pull), Some(repo)) => WebhookEvent::PullRequest {
            repo,
            action: env.action.unwrap_or_default(),
            pull: pull.into(),
        },
        (other, _, _, repo) => WebhookEvent::Other {
            event: other.to_string(),
            repo,
            action: env.action,
        },
    })
}

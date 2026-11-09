//! Teamwork: user profiles, comment threads, mentions, reactions, notifications,
//! and personal runners ("connect your Claude Code").

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::Id;

/// Square avatar sprites (web/public/sprites/avatar/<key>.webp) humans pick from.
/// Agents use the full-body characters; humans use these portraits.
pub const USER_AVATARS: &[&str] = &[
    "boba_avatar",
    "pinky_boba_avatar",
    "golden_boba_avatar",
    "black_boba_avatar",
    "diamond_boba_avatar",
    "boba_uncle",
    "uncle_song",
];

// ---------------------------------------------------------------- comments

/// Where a comment thread is attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "on", rename_all = "snake_case")]
#[ts(export)]
pub enum CommentAnchor {
    /// The ticket as a whole.
    Ticket,
    /// A passage of the ticket body (the quoted text is kept even if the body changes).
    TicketBody { quote: String },
    /// One line of a run transcript.
    RunLog {
        run_id: Id,
        #[ts(type = "number")]
        seq: u64,
    },
    /// A run as a whole (e.g. a review verdict, a job report).
    Run { run_id: Id },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CommentAuthorKind {
    User,
    /// Agents answer in threads (e.g. after being @mentioned).
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Reaction {
    /// A single emoji.
    pub emoji: String,
    pub user_ids: Vec<Id>,
}

/// A comment. A thread = a root comment (`parent_id = None`) + its replies.
/// Threads can be on a ticket, or on a job (`job_id`) for quest discussions.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Comment {
    pub id: Id,
    pub ticket_id: Option<Id>,
    pub job_id: Option<Id>,
    pub anchor: CommentAnchor,
    /// Root comment of the thread; None for the root itself.
    pub parent_id: Option<Id>,
    pub author_kind: CommentAuthorKind,
    pub author_id: Id,
    /// Markdown. Mentions are written `@[Name](user:<id>)` / `@[Name](agent:<id>)`.
    pub body: String,
    pub mentioned_user_ids: Vec<Id>,
    pub mentioned_agent_ids: Vec<Id>,
    /// Runs started because an agent was @mentioned in this comment.
    pub triggered_run_ids: Vec<Id>,
    pub reactions: Vec<Reaction>,
    /// Only meaningful on roots.
    pub resolved: bool,
    pub resolved_by: Option<Id>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    /// Soft delete: body is cleared, the thread structure stays.
    pub deleted: bool,
}

// ---------------------------------------------------------------- notifications

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NotificationKind {
    /// Someone @mentioned you.
    Mention,
    /// A reply in a thread you started or replied to.
    Reply,
    /// A run you started finished / failed / needs permission.
    RunFinished,
    RunFailed,
    RunWaiting,
    /// A review verdict on a ticket you created or ran.
    ReviewVerdict,
    /// A job you own produced a result.
    JobResult,
    /// You were invited / your role changed.
    Account,
}

/// Per-user inbox entry. Sent over WS only to its owner.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Notification {
    pub id: Id,
    pub user_id: Id,
    pub kind: NotificationKind,
    pub message: String,
    pub ticket_id: Option<Id>,
    pub comment_id: Option<Id>,
    pub run_id: Option<Id>,
    pub job_id: Option<Id>,
    pub actor_user_id: Option<Id>,
    pub actor_agent_id: Option<Id>,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------- personal runners

/// Who a runner works for. Anthropic's terms reserve Claude subscriptions for their owner's
/// individual use, so a runner signed in with a subscription is always `Personal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunnerScope {
    /// Runs anyone's work (needs an API key / Bedrock / Vertex). Jobs and auto reviews use these.
    Shared,
    /// Only runs work started by `owner_user_id`.
    Personal,
}

/// How the runner's Claude Code is authenticated (reported by the runner, never the secret).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunnerAuth {
    ApiKey,
    Bedrock,
    Vertex,
    /// Claude Pro/Max/Team login of the machine's user.
    Subscription,
    /// Demo runner, no Claude Code.
    Demo,
    Unknown,
}

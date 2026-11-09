//! Integrations (GitHub repo sync), connectors (MCP servers agents can use) and
//! scheduled jobs (the "Quest Board").

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::Id;

// ---------------------------------------------------------------- repo sync

/// How a repo's GitHub issues / PRs flow into the board and back.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RepoSync {
    /// Master switch. Sync runs by polling (`poll_minutes`) and, if configured, GitHub webhooks.
    pub enabled: bool,
    /// Tab that receives imported tickets.
    pub target_tab_id: Option<Id>,
    /// Import open issues as tickets in the tab's first Todo column.
    pub import_issues: bool,
    /// Only issues carrying one of these labels (empty = all).
    pub issue_labels: Vec<String>,
    /// Import open PRs (including PRs from outside contributors) into the tab's Review column,
    /// so the column's auto reviewer reviews them.
    pub import_prs: bool,
    /// Skip draft PRs.
    pub skip_draft_prs: bool,
    /// Post run summaries / review verdicts back as GitHub comments.
    pub comment_back: bool,
    /// Submit reviewer verdicts as real GitHub PR reviews (approve / request changes).
    pub post_reviews: bool,
    /// Close the linked issue when its ticket reaches a Done column.
    pub close_issues_on_done: bool,
    pub poll_minutes: u32,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

impl Default for RepoSync {
    fn default() -> Self {
        Self {
            enabled: false,
            target_tab_id: None,
            import_issues: true,
            issue_labels: vec![],
            import_prs: true,
            skip_draft_prs: true,
            comment_back: true,
            post_reviews: false,
            close_issues_on_done: false,
            poll_minutes: 5,
            last_synced_at: None,
            last_error: None,
        }
    }
}

/// Where a ticket came from. Imported tickets keep a link to their origin.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TicketSource {
    #[default]
    Manual,
    GithubIssue {
        repo_id: Id,
        #[ts(type = "number")]
        number: i64,
        url: String,
        author: String,
    },
    GithubPr {
        repo_id: Id,
        #[ts(type = "number")]
        number: i64,
        url: String,
        author: String,
        head_branch: String,
    },
    /// Created by an agent through the board tools during a job run.
    Job { job_id: Id, run_id: Id },
    /// Created by an agent through the board tools during a ticket run.
    Agent { agent_id: Id, run_id: Id },
}

// ---------------------------------------------------------------- connectors

/// Presets the UI offers; all of them are MCP servers under the hood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ConnectorKind {
    Github,
    GoogleWorkspace,
    GoogleSearchConsole,
    Linear,
    Slack,
    Sentry,
    Notion,
    WebSearch,
    Postgres,
    CustomMcp,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum McpTransport {
    /// Spawned by the runner: `command args…` with the connector's secrets as env vars.
    Stdio { command: String, args: Vec<String> },
    /// Remote MCP server (streamable HTTP). Secrets named in `header_secrets` become headers.
    Http {
        url: String,
        /// header name -> secret key, e.g. {"Authorization": "TOKEN"} sends "Bearer <TOKEN>".
        header_secrets: BTreeMap<String, String>,
    },
}

/// A tool source agents can be given. Secret VALUES never leave the server
/// (only their names are exposed).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Connector {
    pub id: Id,
    pub name: String,
    pub kind: ConnectorKind,
    pub transport: McpTransport,
    /// Names of the stored secrets (values are write-only).
    pub secret_keys: Vec<String>,
    /// Optional allow-list of tool names (empty = all tools of the server).
    pub allowed_tools: Vec<String>,
    pub created_by: Option<Id>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------- jobs

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum Schedule {
    /// Only via "Run now".
    Manual,
    /// Every N minutes (min 5).
    Interval { minutes: u32 },
    /// 5-field cron expression evaluated in `timezone` (IANA, e.g. "Europe/Paris").
    Cron { expr: String, timezone: String },
}

/// What a job is expected to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum JobOutput {
    /// The agent files tickets through the board tools (e.g. weekly SEO audit -> tickets).
    Tickets,
    /// The agent's final message is stored as a report (shown on the quest card).
    Report,
    /// The agent works in a worktree of `repo_id` and opens a PR (e.g. dependency bumps).
    PullRequest,
}

/// A scheduled (or on-demand) task for an agent: a quest on the Quest Board.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Job {
    pub id: Id,
    pub name: String,
    pub description: String,
    /// Sprite/emoji for the quest card.
    pub icon: String,
    pub agent_id: Id,
    /// The task prompt for each run.
    pub prompt: String,
    pub schedule: Schedule,
    pub output: JobOutput,
    pub enabled: bool,
    /// Repo the agent works in (required for PullRequest output, optional otherwise).
    pub repo_id: Option<Id>,
    /// Where tickets created by this job land.
    pub tab_id: Option<Id>,
    /// Skip a tick if the previous run is still going (default true).
    pub skip_if_running: bool,
    pub last_run_id: Option<Id>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    /// Last report (JobOutput::Report) or summary.
    pub last_result: Option<String>,
    pub runs_count: u32,
    pub created_by: Option<Id>,
    pub created_at: DateTime<Utc>,
}

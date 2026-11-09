//! Contract between the server (orchestration + persistence) and a runner (execution).
//!
//! The server never spawns processes itself: it builds a [`RunSpec`] and hands it
//! to a runner. The runner streams [`RunUpdate`]s back until exactly one
//! `RunUpdate::Finished`. The same messages travel over the wire for remote runners
//! (see [`RunnerWire`]), so the in-process local runner and a remote one are
//! interchangeable.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::*;

/// Everything a runner needs; no database access required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub run_id: Id,
    pub kind: RunKind,
    pub agent: AgentSpec,
    /// None for job runs.
    pub ticket: Option<TicketSpec>,
    /// Set for job runs.
    pub job: Option<JobSpec>,
    /// Set for command runs: the user's sentence + a JSON digest of the board (tabs, columns,
    /// agents, jobs, repos, connectors, skill ids/names) so the concierge can resolve names to ids.
    /// The concierge is read-only and must end with a ```boba-plan``` JSON block (a `CommandPlan`
    /// without run_id/required/allowed; the server fills those).
    pub command: Option<CommandSpec>,
    /// None = job without repo: the runner uses a scratch dir under its data dir.
    pub repo: Option<RepoSpec>,
    /// MCP servers from the agent's connectors, secrets resolved (runner side only, never logged).
    pub mcp_servers: Vec<McpServerSpec>,
    /// Board tools MCP endpoint, when the agent has `board_tools`.
    pub board_tools: Option<BoardToolsSpec>,
    /// Absolute paths of the skill folders to expose to Claude Code.
    pub skill_paths: Vec<String>,
    /// Resume this Claude Code session (follow-up / changes-requested round).
    pub resume_session_id: Option<String>,
    /// Extra user message for this run (assign instructions or follow-up).
    pub extra_instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub id: Id,
    pub name: String,
    pub role: String,
    pub system_prompt: String,
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub allowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketSpec {
    pub id: Id,
    pub number: i64,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    /// Set for review runs and for work runs that must update an existing PR.
    pub branch: Option<String>,
    pub pr_number: Option<i64>,
    pub pr_url: Option<String>,
    /// Reviewer feedback to address (changes-requested round).
    pub review_feedback: Option<String>,
    /// Number of review rounds already done on this ticket (0 on the first work run).
    pub review_rounds: u32,
    /// Origin; for imported PRs the runner reviews `head_branch` instead of a boba branch.
    pub source: crate::automation::TicketSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub text: String,
    pub voice: bool,
    pub user_name: String,
    pub board_digest: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub id: Id,
    pub name: String,
    pub prompt: String,
    pub output: crate::automation::JobOutput,
    /// Report from the previous run, so recurring jobs can avoid repeating themselves.
    pub previous_result: Option<String>,
}

/// One MCP server to pass to Claude Code (`--mcp-config`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSpec {
    /// Server name as seen by Claude Code (tools become `mcp__<name>__<tool>`).
    pub name: String,
    pub transport: crate::automation::McpTransport,
    /// Secret values by key. Stdio: exported as env vars. Http: used for `header_secrets`.
    pub secrets: std::collections::BTreeMap<String, String>,
    /// Empty = all tools allowed.
    pub allowed_tools: Vec<String>,
}

/// The server's own MCP endpoint exposing board tools to the agent (see docs/ARCHITECTURE.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardToolsSpec {
    /// Streamable HTTP MCP URL, e.g. http://127.0.0.1:7777/api/mcp
    pub url: String,
    /// Per-run bearer token; scoped to this run, revoked when it ends.
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSpec {
    pub path: String,
    pub default_branch: String,
    pub remote_url: Option<String>,
}

/// Streamed by the runner while a run executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunUpdate {
    Started {
        worktree_path: String,
        branch: String,
        session_id: Option<String>,
    },
    Log {
        line: RunLogLine,
    },
    /// Best-effort progress for the bar + short activity line ("Running tests").
    Progress {
        progress: f32,
        activity: String,
    },
    Usage {
        cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Claude Code asks for permission; the server surfaces it and answers via
    /// `Runner::answer_permission`.
    PermissionRequest {
        request_id: String,
        tool: String,
        input: serde_json::Value,
    },
    PrOpened {
        number: i64,
        url: String,
    },
    /// Always the last update of a run.
    Finished {
        outcome: RunOutcome,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunOutcome {
    pub status: RunStatus,
    pub summary: Option<String>,
    /// Review runs only.
    pub verdict: Option<ReviewVerdict>,
    /// Review runs: feedback to send back to the author when changes are requested.
    pub feedback: Option<String>,
    pub error: Option<String>,
    /// Command runs: the parsed ```boba-plan``` block.
    #[ts(type = "unknown")]
    pub plan: Option<serde_json::Value>,
}

/// Remote runner protocol over WebSocket `/api/runners/ws` (bearer runner token).
/// v1 ships only the local runner; this is here so the wire format is fixed early.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerWire {
    // runner -> server
    Hello {
        name: String,
        max_concurrent_runs: u32,
        version: String,
    },
    Update {
        run_id: Id,
        update: RunUpdate,
    },
    Skills {
        skills: Vec<Skill>,
    },
    // server -> runner
    Start {
        spec: Box<RunSpec>,
    },
    Cancel {
        run_id: Id,
    },
    Message {
        run_id: Id,
        text: String,
    },
    Permission {
        run_id: Id,
        request_id: String,
        allow: bool,
    },
}

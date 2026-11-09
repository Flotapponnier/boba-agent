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
    pub ticket: TicketSpec,
    pub repo: RepoSpec,
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

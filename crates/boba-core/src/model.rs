//! Persisted domain entities.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

pub type Id = Uuid;

// ---------------------------------------------------------------- users & RBAC

/// Org-wide role. Ordered: Owner > Admin > Member > Viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Role {
    Viewer,
    Member,
    Admin,
    Owner,
}

/// Fine-grained permissions checked by the server. `Role::grants` is the default mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Permission {
    /// See boards, agents, history, live logs.
    View,
    /// Create/edit/move tickets, comment.
    EditTickets,
    /// Drag an agent onto a ticket (starts a run that spends tokens), cancel runs.
    RunAgents,
    /// Create/edit/delete agents, their prompt and skills.
    ManageAgents,
    /// Create/edit skills.
    ManageSkills,
    /// Tabs, columns, repos, runners, theme.
    ManageWorkspace,
    /// Invite users, change roles.
    ManageUsers,
}

impl Role {
    pub fn grants(self, p: Permission) -> bool {
        use Permission::*;
        match self {
            // Owner vs Admin differ only on owner-only server actions (transfer ownership,
            // demote another admin) which the server checks explicitly.
            Role::Owner | Role::Admin => true,
            Role::Member => matches!(p, View | EditTickets | RunAgents | ManageSkills),
            Role::Viewer => matches!(p, View),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct User {
    pub id: Id,
    pub name: String,
    pub email: String,
    pub role: Role,
    /// Sprite key, see [`Sprite`].
    pub avatar: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------- workspace

/// A tab = one board with its own purpose (e.g. "Dev", "Review", "SEO") and its own columns.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Tab {
    pub id: Id,
    pub name: String,
    /// Short emoji or sprite key shown on the tab.
    pub icon: String,
    /// CSS colour, e.g. "#f39c12".
    pub color: String,
    pub position: i32,
    /// Default repo for tickets created in this tab.
    pub repo_id: Option<Id>,
    pub columns: Vec<Column>,
}

/// What a column *means*: drives automation and the colour of the ticket "pin".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ColumnKind {
    Backlog,
    Todo,
    /// Tickets with a running work run live here.
    Doing,
    /// PR column: entering it triggers `auto_reviewer_agent_id` if set.
    Review,
    Done,
    /// No automation.
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Column {
    pub id: Id,
    pub tab_id: Id,
    pub name: String,
    pub kind: ColumnKind,
    pub color: String,
    pub position: i32,
    /// Agent that automatically reviews every ticket entering this column (kind = Review).
    pub auto_reviewer_agent_id: Option<Id>,
    /// Column where approved tickets go after review (defaults to the tab's first Done column).
    pub on_approve_column_id: Option<Id>,
    /// Column where tickets go when the reviewer requests changes (defaults to first Todo column).
    pub on_changes_column_id: Option<Id>,
}

/// A git repository agents can work in. `path` is on the runner's machine.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Repo {
    pub id: Id,
    pub name: String,
    pub path: String,
    pub remote_url: Option<String>,
    pub default_branch: String,
    /// Which runner hosts this repo (multi-server). None = local runner.
    pub runner_id: Option<Id>,
}

/// A machine that executes runs. v1 ships the in-process "local" runner;
/// remote runners connect back over WebSocket (see docs/ARCHITECTURE.md).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunnerInfo {
    pub id: Id,
    pub name: String,
    pub online: bool,
    pub max_concurrent_runs: u32,
    pub active_runs: u32,
    pub last_seen_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------- agents & skills

/// The boba characters (sprites copied from Bobavim). The UI maps key -> image.
pub const SPRITES: &[&str] = &[
    "boba",
    "boba_uncle",
    "pinky_boba",
    "boba_diamond",
    "golden_boba",
    "black_boba",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentStatus {
    Idle,
    Working,
    Reviewing,
    /// Waiting for a human (permission prompt / question).
    Waiting,
    Error,
}

/// Claude Code permission mode used for the agent's runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
}

/// An agent = a Claude Code persona: system prompt + skills + model + look.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Agent {
    pub id: Id,
    pub name: String,
    /// Free text role shown as a badge: "Dev", "Reviewer", "SEO"…
    pub role: String,
    /// Sprite key, one of [`SPRITES`].
    pub sprite: String,
    /// Accent colour for the card / aura.
    pub color: String,
    /// One-line catchphrase shown in the tavern.
    pub tagline: String,
    /// Appended to Claude Code's system prompt (`--append-system-prompt`).
    pub system_prompt: String,
    /// Skill ids this agent may use.
    pub skill_ids: Vec<Id>,
    /// e.g. "claude-sonnet-5-5". None = Claude Code default.
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    /// Extra `--allowedTools` entries, e.g. "Bash(git diff:*)".
    pub allowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    // --- game stats (server-computed) ---
    /// XP inside the current level: progress bar = xp / xp_for_next_level(level).
    pub xp: u32,
    /// Starts at 1.
    pub level: u32,
    pub runs_completed: u32,
    pub prs_approved: u32,
    // --- live state ---
    pub status: AgentStatus,
    pub current_run_id: Option<Id>,
    pub created_by: Option<Id>,
    pub created_at: DateTime<Utc>,
}

/// XP needed to go from `level` to `level + 1`.
pub fn xp_for_next_level(level: u32) -> u32 {
    100 + 50 * level.saturating_sub(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SkillSource {
    /// Created in Boba Agent, stored in its data dir.
    Boba,
    /// Discovered in ~/.claude/skills.
    User,
    /// Discovered in <repo>/.claude/skills.
    Project,
    /// Provided by an installed Claude Code plugin.
    Plugin,
}

/// A Claude Code Agent Skill (a folder with SKILL.md).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Skill {
    pub id: Id,
    /// `name` from SKILL.md frontmatter.
    pub name: String,
    /// `description` from SKILL.md frontmatter.
    pub description: String,
    pub source: SkillSource,
    /// Absolute path of the skill directory on the runner.
    pub path: String,
    /// Only Boba-sourced skills are editable from the UI.
    pub editable: bool,
}

// ---------------------------------------------------------------- tickets & runs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

/// A "little paper" pinned on the board.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Ticket {
    pub id: Id,
    /// Human-friendly number, unique per workspace: T-42.
    #[ts(type = "number")]
    pub number: i64,
    pub tab_id: Id,
    pub column_id: Id,
    /// Fractional ordering inside a column.
    pub position: f64,
    pub title: String,
    /// Markdown. Becomes the agent's task prompt.
    pub body: String,
    pub tags: Vec<String>,
    pub priority: Priority,
    /// Optional paper colour override.
    pub color: Option<String>,
    pub repo_id: Option<Id>,
    /// Agent currently (or last) working on it.
    pub agent_id: Option<Id>,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    #[ts(type = "number | null")]
    pub pr_number: Option<i64>,
    /// Last reviewer feedback when changes were requested.
    pub review_feedback: Option<String>,
    pub review_rounds: u32,
    pub created_by: Option<Id>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunKind {
    /// Implement the ticket, commit on a branch, open/update a PR.
    Work,
    /// Read-only review of the ticket's PR; must end with a verdict.
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunStatus {
    Queued,
    Running,
    /// Blocked on a human (permission prompt).
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewVerdict {
    Approved,
    ChangesRequested,
}

/// One execution of an agent on a ticket.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Run {
    pub id: Id,
    pub ticket_id: Id,
    pub agent_id: Id,
    pub kind: RunKind,
    pub status: RunStatus,
    pub runner_id: Option<Id>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    /// Claude Code session id, for resume / follow-up messages.
    pub session_id: Option<String>,
    /// 0.0..=1.0, best-effort (runner heuristics) for the progress bar.
    pub progress: f32,
    /// Short human line: what the agent is doing right now.
    pub activity: Option<String>,
    pub summary: Option<String>,
    pub verdict: Option<ReviewVerdict>,
    pub cost_usd: f64,
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    pub started_by: Option<Id>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LogKind {
    /// Assistant prose.
    Text,
    /// Assistant thinking (collapsed by default in the UI).
    Thinking,
    /// A tool call: `data` = {"name": "...", "input": {...}}.
    ToolUse,
    /// A tool result: `data` = {"tool_use_id": "...", "output": "...", "is_error": bool}.
    ToolResult,
    /// Runner / system messages ("worktree created", "PR #12 opened").
    System,
    /// Stderr or failures.
    Error,
    /// A message typed by a human into the run.
    User,
}

/// One line of a run's transcript. Stored, and streamed live over the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunLogLine {
    pub run_id: Id,
    /// Monotonic per run, starts at 0.
    #[ts(type = "number")]
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub kind: LogKind,
    /// Plain text rendering (always present, used for terminals and search).
    pub text: String,
    /// Structured payload, shape depends on `kind`.
    #[ts(type = "unknown")]
    pub data: Option<serde_json::Value>,
}

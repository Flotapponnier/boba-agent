//! REST request/response bodies. Routes are documented in docs/API.md.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::HistoryEvent;
use crate::model::*;

/// Full board state, returned by `GET /api/snapshot` and as the first WS message.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    pub me: Option<User>,
    pub tabs: Vec<Tab>,
    pub agents: Vec<Agent>,
    pub tickets: Vec<Ticket>,
    /// Non-terminal runs + the latest run of every ticket.
    pub runs: Vec<Run>,
    pub skills: Vec<Skill>,
    pub repos: Vec<Repo>,
    pub users: Vec<User>,
    pub runners: Vec<RunnerInfo>,
    /// Latest 100 history events, newest first.
    pub history: Vec<HistoryEvent>,
    pub settings: WorkspaceSettings,
}

/// Workspace-wide customisation.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceSettings {
    pub name: String,
    /// "sky" | "sunset" | "night" | custom theme id.
    pub theme: String,
    pub music_enabled_by_default: bool,
    /// True when no Claude Code binary / API key is configured: runs are simulated.
    pub demo_mode: bool,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            name: "Boba Agent".into(),
            theme: "sky".into(),
            music_enabled_by_default: false,
            demo_mode: true,
        }
    }
}

// ---------------------------------------------------------------- auth

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// `POST /api/auth/setup` – only allowed while no user exists; creates the Owner.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetupRequest {
    pub workspace_name: String,
    pub name: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuthStatus {
    /// False until the first owner is created.
    pub setup_done: bool,
    pub me: Option<User>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InviteRequest {
    pub email: String,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InviteResponse {
    pub user: User,
    /// One-time link to set a password.
    pub invite_url: String,
}

/// `POST /api/auth/accept-invite`
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AcceptInvite {
    pub token: String,
    pub password: String,
}

/// `PATCH /api/users/:id`
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateUser {
    pub role: Role,
}

// ---------------------------------------------------------------- agents

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateAgent {
    pub name: String,
    pub role: String,
    pub sprite: String,
    pub color: Option<String>,
    pub tagline: Option<String>,
    pub system_prompt: String,
    pub skill_ids: Vec<Id>,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    pub allowed_tools: Option<Vec<String>>,
    pub max_turns: Option<u32>,
}

/// PATCH semantics: absent field = unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateAgent {
    pub name: Option<String>,
    pub role: Option<String>,
    pub sprite: Option<String>,
    pub color: Option<String>,
    pub tagline: Option<String>,
    pub system_prompt: Option<String>,
    pub skill_ids: Option<Vec<Id>>,
    pub model: Option<Option<String>>,
    pub permission_mode: Option<PermissionMode>,
    pub allowed_tools: Option<Vec<String>>,
    pub max_turns: Option<Option<u32>>,
}

// ---------------------------------------------------------------- tickets

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateTicket {
    pub tab_id: Id,
    /// Defaults to the tab's first Todo column.
    pub column_id: Option<Id>,
    pub title: String,
    pub body: Option<String>,
    pub tags: Option<Vec<String>>,
    pub priority: Option<Priority>,
    pub color: Option<String>,
    pub repo_id: Option<Id>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateTicket {
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Option<Vec<String>>,
    pub priority: Option<Priority>,
    pub color: Option<Option<String>>,
    pub repo_id: Option<Option<Id>>,
}

/// `POST /api/tickets/:id/move`. Moving into a Review column with an auto reviewer
/// starts a review run.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MoveTicket {
    pub column_id: Id,
    pub position: f64,
}

/// `POST /api/tickets/:id/assign` – the drag-an-agent-onto-a-ticket action.
/// Starts a Work run (or a Review run if the ticket sits in a Review column).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AssignAgent {
    pub agent_id: Id,
    /// Extra instructions appended to the ticket body for this run.
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CommentRequest {
    pub text: String,
}

// ---------------------------------------------------------------- runs

/// `POST /api/runs/:id/message` – send a follow-up message into a live or finished run
/// (resumes the Claude Code session).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunMessage {
    pub text: String,
}

/// `POST /api/runs/:id/permission` – answer a pending permission prompt.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionAnswer {
    pub request_id: String,
    pub allow: bool,
}

// ---------------------------------------------------------------- workspace

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateTab {
    pub name: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub repo_id: Option<Id>,
    /// None = default columns: Todo, Doing, Review (auto reviewer = first agent with role "Reviewer"), Done.
    pub columns: Option<Vec<ColumnSpec>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ColumnSpec {
    /// Present = update existing column, absent = create.
    pub id: Option<Id>,
    pub name: String,
    pub kind: ColumnKind,
    pub color: Option<String>,
    pub auto_reviewer_agent_id: Option<Id>,
    pub on_approve_column_id: Option<Id>,
    pub on_changes_column_id: Option<Id>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateTab {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub repo_id: Option<Option<Id>>,
    /// Full replacement of the column list (ids kept = columns kept).
    pub columns: Option<Vec<ColumnSpec>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateRepo {
    pub name: String,
    pub path: String,
    pub remote_url: Option<String>,
    pub default_branch: Option<String>,
    pub runner_id: Option<Id>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpsertSkill {
    pub name: String,
    pub description: String,
    /// Full SKILL.md body (without frontmatter).
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SkillDetail {
    pub skill: Skill,
    pub body: String,
    /// Other files in the skill folder (relative paths).
    pub files: Vec<String>,
}

/// `GET /api/history?ticket_id=&agent_id=&run_id=&before=&limit=`
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistoryQuery {
    pub ticket_id: Option<Id>,
    pub agent_id: Option<Id>,
    pub run_id: Option<Id>,
    pub tab_id: Option<Id>,
    /// ISO timestamp cursor (exclusive).
    pub before: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiError {
    pub error: String,
    pub code: String,
}

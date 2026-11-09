//! REST request/response bodies. Routes are documented in docs/API.md.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::automation::*;
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
    pub jobs: Vec<Job>,
    pub connectors: Vec<Connector>,
    /// The caller's unread + 50 latest notifications.
    pub notifications: Vec<crate::team::Notification>,
    /// Latest 100 history events, newest first.
    pub history: Vec<HistoryEvent>,
    pub settings: WorkspaceSettings,
    pub stats: WorkspaceStats,
}

/// Team-wide game counters (the pearl counter in the header). Also re-sent via `Fx::PearlsEarned`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceStats {
    /// Earned when a reviewer approves a PR (bonus on first-round approval).
    #[ts(type = "number")]
    pub pearls: u64,
    #[ts(type = "number")]
    pub runs_completed: u64,
    #[ts(type = "number")]
    pub prs_approved: u64,
    pub total_cost_usd: f64,
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
    /// Speech-to-text for voice commands.
    #[serde(default)]
    pub speech: SpeechProvider,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            name: "Boba Agent".into(),
            theme: "sky".into(),
            music_enabled_by_default: false,
            demo_mode: true,
            speech: SpeechProvider::default(),
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
    pub connector_ids: Option<Vec<Id>>,
    pub board_tools: Option<bool>,
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
    pub connector_ids: Option<Vec<Id>>,
    pub board_tools: Option<bool>,
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

// ---------------------------------------------------------------- team

/// `PATCH /api/me` – any signed-in user edits their own profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateMe {
    pub name: Option<String>,
    /// One of `team::USER_AVATARS`.
    pub avatar: Option<String>,
    /// Requires `current_password`.
    pub new_password: Option<String>,
    pub current_password: Option<String>,
}

/// `POST /api/comments` [EditTickets]. Mentioning an agent with `RunAgents` permission
/// starts a run of that agent on the ticket with the comment as instructions; the agent
/// replies in the thread when it finishes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateComment {
    pub ticket_id: Option<Id>,
    pub job_id: Option<Id>,
    pub anchor: crate::team::CommentAnchor,
    pub parent_id: Option<Id>,
    pub body: String,
}

/// `PATCH /api/comments/:id` – author edits body; anyone with EditTickets resolves/reopens a root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateComment {
    pub body: Option<String>,
    pub resolved: Option<bool>,
}

/// `POST /api/comments/:id/react` [View] – toggles the caller's reaction.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReactRequest {
    pub emoji: String,
}

/// `POST /api/notifications/read` – mark some (or all, when `ids` is None) as read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MarkRead {
    pub ids: Option<Vec<Id>>,
}

/// `POST /api/runners` – register a runner machine. Any member may add a Personal runner
/// for themselves; Shared runners need ManageWorkspace.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateRunner {
    pub name: String,
    pub scope: crate::team::RunnerScope,
    pub max_concurrent_runs: Option<u32>,
}

/// Returned once: the token is shown a single time.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunnerToken {
    pub runner: RunnerInfo,
    pub token: String,
    /// Ready-to-paste command, e.g. `boba-agent runner --server https://… --token …`.
    pub command: String,
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
    pub sync: Option<RepoSync>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateRepo {
    pub name: Option<String>,
    pub path: Option<String>,
    pub remote_url: Option<Option<String>>,
    pub default_branch: Option<String>,
    pub sync: Option<RepoSync>,
}

/// `POST /api/repos/:id/sync` result.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SyncReport {
    pub imported_issues: u32,
    pub imported_prs: u32,
    pub updated: u32,
    pub closed: u32,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------- connectors

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateConnector {
    pub name: String,
    pub kind: ConnectorKind,
    pub transport: McpTransport,
    /// Write-only secret values by key.
    pub secrets: std::collections::BTreeMap<String, String>,
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateConnector {
    pub name: Option<String>,
    pub transport: Option<McpTransport>,
    /// Keys present are set; a key mapped to "" is deleted; absent keys are kept.
    pub secrets: Option<std::collections::BTreeMap<String, String>>,
    pub allowed_tools: Option<Vec<String>>,
}

/// `POST /api/connectors/:id/test` – starts the MCP server and lists its tools.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConnectorTestResult {
    pub ok: bool,
    pub tools: Vec<String>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------- jobs

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateJob {
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub agent_id: Id,
    pub prompt: String,
    pub schedule: Schedule,
    pub output: JobOutput,
    pub enabled: Option<bool>,
    pub repo_id: Option<Id>,
    pub tab_id: Option<Id>,
    pub skip_if_running: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateJob {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub agent_id: Option<Id>,
    pub prompt: Option<String>,
    pub schedule: Option<Schedule>,
    pub output: Option<JobOutput>,
    pub enabled: Option<bool>,
    pub repo_id: Option<Option<Id>>,
    pub tab_id: Option<Option<Id>>,
    pub skip_if_running: Option<bool>,
}

/// `POST /api/jobs/preview-schedule` – validate a schedule and show the next fire times.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SchedulePreview {
    pub valid: bool,
    pub error: Option<String>,
    pub next: Vec<chrono::DateTime<chrono::Utc>>,
    /// Human sentence, e.g. "Every Monday at 09:00 (Europe/Paris)".
    pub description: String,
}

// ---------------------------------------------------------------- command bar & voice

/// Where voice is transcribed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum SpeechProvider {
    /// Browser Web Speech API (audio handled by the browser vendor). Text only reaches the server.
    #[default]
    Browser,
    /// Self-hosted Whisper-compatible endpoint (e.g. whisper.cpp server); the UI uploads audio to
    /// `POST /api/speech/transcribe`, which proxies to `url`. Audio never leaves your infra.
    Whisper {
        url: String,
    },
    Disabled,
}

/// `POST /api/speech/transcribe` response (body: audio/webm or audio/wav).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Transcript {
    pub text: String,
}

/// `POST /api/command` [View]: "create a job that…", "make Boba Dev review this"…
/// Starts a `RunKind::Command` run of the concierge; its result arrives as
/// `ServerMessage::CommandPlan` to the requesting user only.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CommandRequest {
    pub text: String,
    /// UI context so "this" / "here" resolve: current tab, selected ticket, selected agent.
    pub tab_id: Option<Id>,
    pub ticket_id: Option<Id>,
    pub agent_id: Option<Id>,
    /// True when the text came from voice (the concierge tolerates transcription errors).
    pub voice: bool,
}

/// One proposed action. Executed only after the user confirms, with the user's own permissions.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum CommandAction {
    CreateTicket { ticket: CreateTicket },
    UpdateTicket { ticket_id: Id, patch: UpdateTicket },
    MoveTicket { ticket_id: Id, column_id: Id },
    AssignAgent { ticket_id: Id, assign: AssignAgent },
    CreateAgent { agent: CreateAgent },
    UpdateAgent { agent_id: Id, patch: UpdateAgent },
    CreateJob { job: CreateJob },
    UpdateJob { job_id: Id, patch: UpdateJob },
    RunJob { job_id: Id },
    CreateTab { tab: CreateTab },
    SyncRepo { repo_id: Id },
    CancelRun { run_id: Id },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlannedAction {
    /// Human sentence for the confirmation card: "Create job 'Weekly SEO audit' (Mondays 09:00) for Pinky SEO".
    pub label: String,
    pub action: CommandAction,
    /// Permission the user needs; actions the user lacks are returned but flagged.
    pub required: Permission,
    pub allowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CommandPlan {
    pub run_id: Id,
    /// Short spoken/written answer ("Sure, here's what I'll do").
    pub reply: String,
    pub actions: Vec<PlannedAction>,
    /// Set when the concierge needs more info instead of acting.
    pub question: Option<String>,
}

/// `POST /api/command/execute` [per action]: run the confirmed (possibly edited) actions in order.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExecuteCommand {
    pub run_id: Id,
    pub actions: Vec<CommandAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActionResult {
    pub ok: bool,
    pub error: Option<String>,
    /// Created/updated entity, for the UI to focus it.
    pub entity: Option<crate::event::Entity>,
}

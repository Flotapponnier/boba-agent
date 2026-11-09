//! Realtime protocol (WebSocket `/api/ws`) and the persisted history log.
//!
//! Every state change the server makes is (1) persisted, (2) recorded as a
//! [`HistoryEvent`] when it is meaningful to humans, and (3) broadcast as a
//! [`ServerMessage`]. The UI keeps a normalised store and applies `Upsert`/`Delete`
//! messages; it never needs to refetch after connecting except on reconnect
//! (send `ClientMessage::Resync`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::automation::*;
use crate::model::*;

/// What happened, for the history timeline ("everything is visible").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HistoryKind {
    TicketCreated,
    TicketUpdated,
    TicketMoved,
    TicketDeleted,
    AgentCreated,
    AgentUpdated,
    AgentDeleted,
    AgentLevelUp,
    /// An agent was dropped on a ticket.
    RunStarted,
    RunWaiting,
    RunSucceeded,
    RunFailed,
    RunCancelled,
    PrOpened,
    ReviewApproved,
    ReviewChangesRequested,
    SkillCreated,
    SkillUpdated,
    TabCreated,
    TabUpdated,
    TabDeleted,
    UserJoined,
    CommandIssued,
    CommentResolved,
    RunnerConnected,
    RunnerDisconnected,
    ProfileUpdated,
    CommandExecuted,
    JobCreated,
    JobUpdated,
    JobDeleted,
    JobRunStarted,
    JobRunFinished,
    TicketImported,
    RepoSynced,
    ConnectorCreated,
    ConnectorUpdated,
    ConnectorDeleted,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ActorKind {
    User,
    Agent,
    System,
}

/// One entry of the history timeline. Append-only.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistoryEvent {
    pub id: Id,
    pub ts: DateTime<Utc>,
    pub kind: HistoryKind,
    pub actor_kind: ActorKind,
    /// User id or agent id depending on `actor_kind`.
    pub actor_id: Option<Id>,
    pub ticket_id: Option<Id>,
    pub agent_id: Option<Id>,
    pub run_id: Option<Id>,
    pub tab_id: Option<Id>,
    /// Ready-to-display sentence, e.g. "Uncle Review approved PR #12 (+30 XP)".
    pub message: String,
    /// Extra structured data (from/to column, xp gained, cost…).
    #[ts(type = "unknown")]
    pub data: Option<serde_json::Value>,
}

/// Any entity the UI caches.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "entity", content = "data", rename_all = "snake_case")]
#[ts(export)]
pub enum Entity {
    Tab(Tab),
    Agent(Agent),
    Ticket(Ticket),
    Run(Run),
    Skill(Skill),
    Repo(Repo),
    User(User),
    Runner(RunnerInfo),
    Job(Job),
    Connector(Connector),
    Comment(crate::team::Comment),
    /// Only ever sent to its owner.
    Notification(crate::team::Notification),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EntityKind {
    Tab,
    Agent,
    Ticket,
    Run,
    Skill,
    Repo,
    User,
    Runner,
    Job,
    Connector,
    Comment,
    Notification,
}

/// Purely cosmetic one-shot effects so every connected screen celebrates together.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "fx", rename_all = "snake_case")]
#[ts(export)]
pub enum Fx {
    LevelUp {
        agent_id: Id,
        level: u32,
    },
    XpGained {
        agent_id: Id,
        amount: u32,
    },
    WorkFinished {
        agent_id: Id,
        ticket_id: Id,
    },
    PrApproved {
        agent_id: Id,
        ticket_id: Id,
        reviewer_id: Id,
    },
    ChangesRequested {
        ticket_id: Id,
        reviewer_id: Id,
    },
    RunFailed {
        agent_id: Id,
        ticket_id: Id,
    },
    /// `total` is the new workspace pearl count.
    PearlsEarned {
        ticket_id: Id,
        amount: u32,
        #[ts(type = "number")]
        total: u64,
    },
}

/// Server -> client.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ServerMessage {
    /// First message after connect / resync: the full current state.
    Snapshot {
        state: crate::api::Snapshot,
    },
    Upsert {
        item: Entity,
    },
    Delete {
        kind: EntityKind,
        id: Id,
    },
    History {
        event: HistoryEvent,
    },
    /// Only sent for runs the client subscribed to (see `ClientMessage::Subscribe`).
    RunLog {
        line: RunLogLine,
    },
    Fx {
        fx: Fx,
    },
    /// Who is looking at the board right now.
    Presence {
        users: Vec<PresenceUser>,
    },
    /// Concierge result, sent only to the user who issued the command.
    CommandPlan {
        plan: crate::api::CommandPlan,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PresenceUser {
    pub user_id: Id,
    pub name: String,
    pub avatar: String,
    pub tab_id: Option<Id>,
}

/// Client -> server.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ClientMessage {
    Resync,
    /// Stream live log lines of these runs (the server first replays the last 200 lines).
    Subscribe {
        run_ids: Vec<Id>,
    },
    Unsubscribe {
        run_ids: Vec<Id>,
    },
    /// Presence: which tab the user is looking at.
    Viewing {
        tab_id: Option<Id>,
    },
    Ping,
}

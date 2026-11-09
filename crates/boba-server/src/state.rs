//! Shared application state and the "persist → history → broadcast" helpers every
//! mutation goes through.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use boba_core::api::{Snapshot, WorkspaceStats};
use boba_core::event::{
    ActorKind, Entity, EntityKind, Fx, HistoryEvent, HistoryKind, ServerMessage,
};
use boba_core::*;
use boba_runner::Runner;
use chrono::Utc;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::config::Config;
use crate::db::{new_id, Db};
use crate::hub::Hub;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub cfg: Config,
    pub db: Db,
    pub hub: Hub,
    pub runner: Arc<dyn Runner>,
    /// Serialises orchestration state transitions (assign, finish, review routing).
    pub orch_lock: Mutex<()>,
    /// Runs handed to the runner and not finished yet.
    pub live: StdMutex<HashSet<Id>>,
    /// Next log seq per active run (server-assigned, shared by runner and server lines).
    seqs: StdMutex<HashMap<Id, u64>>,
    logs: mpsc::Sender<LogCmd>,
}

/// Who did something, for history lines.
#[derive(Debug, Clone, Copy)]
pub enum Actor {
    User(Id),
    Agent(Id),
    System,
}

/// Entity references attached to a history line.
#[derive(Debug, Clone, Copy, Default)]
pub struct Refs {
    pub ticket: Option<Id>,
    pub agent: Option<Id>,
    pub run: Option<Id>,
    pub tab: Option<Id>,
}

impl Refs {
    pub fn ticket(t: &Ticket) -> Self {
        Self {
            ticket: Some(t.id),
            tab: Some(t.tab_id),
            ..Self::default()
        }
    }
    pub fn agent(mut self, a: Id) -> Self {
        self.agent = Some(a);
        self
    }
    pub fn run(mut self, r: Id) -> Self {
        self.run = Some(r);
        self
    }
}

enum LogCmd {
    Line(RunLogLine),
    Flush(oneshot::Sender<()>),
}

impl AppState {
    pub async fn new(cfg: Config, runner: Arc<dyn Runner>) -> anyhow::Result<SharedState> {
        let db = Db::open(&cfg.data_dir).await?;
        let (tx, rx) = mpsc::channel(8192);
        tokio::spawn(log_writer(db.clone(), rx));
        Ok(Arc::new(Self {
            cfg,
            db,
            hub: Hub::new(),
            runner,
            orch_lock: Mutex::new(()),
            live: StdMutex::new(HashSet::new()),
            seqs: StdMutex::new(HashMap::new()),
            logs: tx,
        }))
    }

    // ------------------------------------------------------------ broadcast helpers

    pub fn upsert(&self, item: Entity) {
        self.hub.broadcast(ServerMessage::Upsert { item });
    }

    pub fn deleted(&self, kind: EntityKind, id: Id) {
        self.hub.broadcast(ServerMessage::Delete { kind, id });
    }

    pub fn fx(&self, fx: Fx) {
        self.hub.broadcast(ServerMessage::Fx { fx });
    }

    /// Persist a history line and broadcast it. Failures are logged, never fatal.
    pub async fn record(
        &self,
        kind: HistoryKind,
        actor: Actor,
        refs: Refs,
        message: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Option<HistoryEvent> {
        let (actor_kind, actor_id) = match actor {
            Actor::User(id) => (ActorKind::User, Some(id)),
            Actor::Agent(id) => (ActorKind::Agent, Some(id)),
            Actor::System => (ActorKind::System, None),
        };
        let event = HistoryEvent {
            id: new_id(),
            ts: Utc::now(),
            kind,
            actor_kind,
            actor_id,
            ticket_id: refs.ticket,
            agent_id: refs.agent,
            run_id: refs.run,
            tab_id: refs.tab,
            message: message.into(),
            data,
        };
        if let Err(e) = self.db.insert_history(&event).await {
            tracing::error!(error = ?e, "failed to persist history");
            return None;
        }
        self.hub.broadcast(ServerMessage::History {
            event: event.clone(),
        });
        Some(event)
    }

    // ------------------------------------------------------------ run logs

    /// Make sure the run has a seq counter (resumes after the last stored line).
    pub async fn init_seq(&self, run_id: Id) -> anyhow::Result<()> {
        let exists = self
            .seqs
            .lock()
            .map(|m| m.contains_key(&run_id))
            .unwrap_or(false);
        if !exists {
            let next = self.db.next_log_seq(run_id).await?;
            if let Ok(mut m) = self.seqs.lock() {
                m.entry(run_id).or_insert(next);
            }
        }
        Ok(())
    }

    pub fn drop_seq(&self, run_id: Id) {
        if let Ok(mut m) = self.seqs.lock() {
            m.remove(&run_id);
        }
    }

    fn take_seq(&self, run_id: Id) -> u64 {
        let Ok(mut m) = self.seqs.lock() else {
            return 0;
        };
        let e = m.entry(run_id).or_insert(0);
        let v = *e;
        *e += 1;
        v
    }

    /// Append a line to a run's transcript with a server-assigned seq.
    pub async fn append_log(&self, mut line: RunLogLine) {
        line.seq = self.take_seq(line.run_id);
        self.log(line).await;
    }

    /// Convenience: a server-authored line.
    pub async fn server_log(
        &self,
        run_id: Id,
        kind: LogKind,
        text: impl Into<String>,
        data: Option<serde_json::Value>,
    ) {
        if let Err(e) = self.init_seq(run_id).await {
            tracing::warn!(error = ?e, "init log seq");
        }
        self.append_log(RunLogLine {
            run_id,
            seq: 0,
            ts: Utc::now(),
            kind,
            text: text.into(),
            data,
        })
        .await;
    }

    /// Queue a log line for a batched insert and fan it out live.
    async fn log(&self, line: RunLogLine) {
        self.hub.push_log(line.clone());
        if self.logs.send(LogCmd::Line(line)).await.is_err() {
            tracing::error!("log writer stopped");
        }
    }

    /// Wait until every queued line is in the database.
    pub async fn flush_logs(&self) {
        let (tx, rx) = oneshot::channel();
        if self.logs.send(LogCmd::Flush(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    // ------------------------------------------------------------ stats

    pub async fn stats(&self) -> anyhow::Result<WorkspaceStats> {
        Ok(match self.db.setting("stats").await? {
            Some(v) => serde_json::from_str(&v).unwrap_or_default(),
            None => WorkspaceStats::default(),
        })
    }

    pub async fn update_stats(
        &self,
        f: impl FnOnce(&mut WorkspaceStats),
    ) -> anyhow::Result<WorkspaceStats> {
        let mut s = self.stats().await?;
        f(&mut s);
        self.db
            .set_setting("stats", &serde_json::to_string(&s)?)
            .await?;
        Ok(s)
    }

    // ------------------------------------------------------------ snapshot

    pub async fn runner_info(&self) -> anyhow::Result<RunnerInfo> {
        let active = self
            .db
            .active_runs()
            .await?
            .iter()
            .filter(|r| r.status != RunStatus::Queued)
            .count() as u32;
        Ok(RunnerInfo {
            id: self.runner.id(),
            name: self.runner.name().to_string(),
            online: true,
            max_concurrent_runs: self.runner.max_concurrent_runs(),
            active_runs: active,
            last_seen_at: Some(Utc::now()),
        })
    }

    pub async fn snapshot(&self, me: Option<User>) -> anyhow::Result<Snapshot> {
        let db = &self.db;
        let (tabs, agents, tickets, runs, skills, repos, users) = tokio::try_join!(
            db.tabs(),
            db.agents(),
            db.tickets(),
            db.snapshot_runs(),
            db.skills(),
            db.repos(),
            db.users(),
        )?;
        let history = db
            .history(&boba_core::api::HistoryQuery {
                limit: Some(100),
                ..Default::default()
            })
            .await?;
        Ok(Snapshot {
            me,
            tabs,
            agents,
            tickets,
            runs,
            skills,
            repos,
            users,
            runners: vec![self.runner_info().await?],
            history,
            settings: db.workspace_settings(self.cfg.demo_mode).await?,
            stats: self.stats().await?,
        })
    }
}

/// Batches log inserts: flush every 150 ms or 500 lines, whichever first.
async fn log_writer(db: Db, mut rx: mpsc::Receiver<LogCmd>) {
    let mut buf: Vec<RunLogLine> = Vec::with_capacity(512);
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(LogCmd::Line(l)) => {
                    buf.push(l);
                    if buf.len() >= 500 {
                        write(&db, &mut buf).await;
                    }
                }
                Some(LogCmd::Flush(done)) => {
                    write(&db, &mut buf).await;
                    let _ = done.send(());
                }
                None => {
                    write(&db, &mut buf).await;
                    return;
                }
            },
            _ = tick.tick() => {
                if !buf.is_empty() {
                    write(&db, &mut buf).await;
                }
            }
        }
    }
}

async fn write(db: &Db, buf: &mut Vec<RunLogLine>) {
    if buf.is_empty() {
        return;
    }
    if let Err(e) = db.insert_logs(buf).await {
        tracing::error!(error = ?e, lines = buf.len(), "failed to persist run logs");
    }
    buf.clear();
}

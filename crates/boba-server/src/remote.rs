//! Remote runners ("connect your Claude Code"): a machine that dials in over
//! `WS /api/runners/ws` and speaks `RunnerWire`.
//!
//! [`RemoteRunner`] is the server-side stand-in implementing [`Runner`]: the orchestrator
//! talks to it like to the built-in runner, and it relays over the socket. Runs survive a
//! short disconnect: the runner buffers its updates and lists its live runs in `Resume`
//! when it reconnects; runs it does not resume within [`RESUME_GRACE`] are failed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use boba_core::runner::{RunOutcome, RunSpec, RunUpdate, RunnerWire};
use boba_core::{Id, RunStatus, Skill};
use boba_runner::{Runner, UpdateTx};
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

/// How long a disconnected runner has to come back with its runs.
pub const RESUME_GRACE: Duration = Duration::from_secs(120);

pub struct RemoteRunner {
    id: Id,
    name: String,
    /// What the runner was registered with (the server never asks for more).
    registered_max: u32,
    max_runs: AtomicU32,
    /// (connection generation, outgoing frames) while connected.
    conn: Mutex<Option<(u64, mpsc::Sender<RunnerWire>)>>,
    generation: AtomicU64,
    /// Update channel of every run handed to this runner and not finished.
    runs: Mutex<HashMap<Id, UpdateTx>>,
    skills: Mutex<Vec<Skill>>,
    last_seen: Mutex<Option<DateTime<Utc>>>,
}

impl RemoteRunner {
    pub fn new(id: Id, name: String, max_runs: u32, last_seen: Option<DateTime<Utc>>) -> Self {
        Self {
            id,
            name,
            registered_max: max_runs.max(1),
            max_runs: AtomicU32::new(max_runs.max(1)),
            conn: Mutex::new(None),
            generation: AtomicU64::new(0),
            runs: Mutex::new(HashMap::new()),
            skills: Mutex::new(Vec::new()),
            last_seen: Mutex::new(last_seen),
        }
    }

    pub fn is_online(&self) -> bool {
        self.conn.lock().map(|c| c.is_some()).unwrap_or(false)
    }

    pub fn last_seen(&self) -> Option<DateTime<Utc>> {
        self.last_seen.lock().ok().and_then(|l| *l)
    }

    pub fn touch(&self) {
        if let Ok(mut l) = self.last_seen.lock() {
            *l = Some(Utc::now());
        }
    }

    /// The machine's own limit from Hello: the effective one is the smaller of the two.
    pub fn set_max_runs(&self, hello: u32) {
        self.max_runs.store(
            hello.clamp(1, 64).min(self.registered_max),
            Ordering::Relaxed,
        );
    }

    /// After a server restart: take back a run this runner was executing, so its buffered
    /// updates (sent after Resume) reach the orchestrator again.
    pub fn adopt(&self, run_id: Id, updates: UpdateTx) {
        if let Ok(mut r) = self.runs.lock() {
            r.insert(run_id, updates);
        }
    }

    pub fn set_skills(&self, skills: Vec<Skill>) {
        if let Ok(mut s) = self.skills.lock() {
            *s = skills;
        }
    }

    pub fn active_run_ids(&self) -> Vec<Id> {
        self.runs
            .lock()
            .map(|r| r.keys().copied().collect())
            .unwrap_or_default()
    }

    /// A new socket took over (an older one, if any, is dropped and ends). Returns its
    /// generation, used by [`detach`](Self::detach).
    pub fn attach(&self, tx: mpsc::Sender<RunnerWire>) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut c) = self.conn.lock() {
            *c = Some((generation, tx));
        }
        self.touch();
        generation
    }

    /// The socket of `generation` closed. False if a newer one already replaced it.
    pub fn detach(&self, generation: u64) -> bool {
        let Ok(mut c) = self.conn.lock() else {
            return false;
        };
        if c.as_ref().is_some_and(|(g, _)| *g == generation) {
            *c = None;
            drop(c);
            self.touch();
            true
        } else {
            false
        }
    }

    /// Drop the connection (runner deleted): its socket loop ends.
    pub fn kick(&self) {
        if let Ok(mut c) = self.conn.lock() {
            *c = None;
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn sender(&self) -> Option<mpsc::Sender<RunnerWire>> {
        self.conn
            .lock()
            .ok()
            .and_then(|c| c.as_ref().map(|(_, tx)| tx.clone()))
    }

    async fn send(&self, msg: RunnerWire) -> anyhow::Result<()> {
        let tx = self
            .sender()
            .ok_or_else(|| anyhow::anyhow!("runner {} is offline", self.name))?;
        tx.send(msg)
            .await
            .map_err(|_| anyhow::anyhow!("runner {} disconnected", self.name))
    }

    /// An update from the machine: forwarded to the orchestrator.
    pub async fn deliver(&self, run_id: Id, update: RunUpdate) {
        let finished = matches!(update, RunUpdate::Finished { .. });
        let tx = self.runs.lock().ok().and_then(|r| r.get(&run_id).cloned());
        let Some(tx) = tx else {
            return; // unknown or already finished run: ignore
        };
        let _ = tx.send(update).await;
        if finished {
            if let Ok(mut r) = self.runs.lock() {
                r.remove(&run_id);
            }
        }
    }

    /// The runner reconnected and still executes `alive`: every other run it had is lost.
    pub async fn resume(&self, alive: &[Id]) {
        let lost: Vec<Id> = self
            .active_run_ids()
            .into_iter()
            .filter(|id| !alive.contains(id))
            .collect();
        for id in lost {
            self.fail(id, "the runner restarted and lost this run")
                .await;
        }
    }

    /// Fail every run (runner gone for good, or did not come back in time).
    pub async fn fail_all(&self, reason: &str) {
        for id in self.active_run_ids() {
            self.fail(id, reason).await;
        }
    }

    async fn fail(&self, run_id: Id, reason: &str) {
        self.finish(run_id, RunStatus::Failed, reason).await;
    }

    async fn finish(&self, run_id: Id, status: RunStatus, reason: &str) {
        let tx = self.runs.lock().ok().and_then(|mut r| r.remove(&run_id));
        if let Some(tx) = tx {
            let _ = tx
                .send(RunUpdate::Finished {
                    outcome: RunOutcome {
                        status,
                        summary: None,
                        verdict: None,
                        feedback: None,
                        error: Some(reason.to_string()),
                        plan: None,
                    },
                })
                .await;
        }
    }
}

#[async_trait]
impl Runner for RemoteRunner {
    fn id(&self) -> Id {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn max_concurrent_runs(&self) -> u32 {
        self.max_runs.load(Ordering::Relaxed)
    }

    async fn start(&self, spec: RunSpec, updates: UpdateTx) -> anyhow::Result<()> {
        let run_id = spec.run_id;
        if let Ok(mut r) = self.runs.lock() {
            r.insert(run_id, updates);
        }
        if let Err(e) = self
            .send(RunnerWire::Start {
                spec: Box::new(spec),
            })
            .await
        {
            if let Ok(mut r) = self.runs.lock() {
                r.remove(&run_id);
            }
            return Err(e);
        }
        Ok(())
    }

    async fn cancel(&self, run_id: Id) -> anyhow::Result<()> {
        if self.sender().is_none() {
            // Nobody to tell: the run is over as far as the server is concerned.
            self.finish(run_id, RunStatus::Cancelled, "cancelled").await;
            return Ok(());
        }
        self.send(RunnerWire::Cancel { run_id }).await
    }

    async fn send_message(&self, run_id: Id, text: String) -> anyhow::Result<()> {
        self.send(RunnerWire::Message { run_id, text }).await
    }

    async fn answer_permission(
        &self,
        run_id: Id,
        request_id: String,
        allow: bool,
    ) -> anyhow::Result<()> {
        self.send(RunnerWire::Permission {
            run_id,
            request_id,
            allow,
        })
        .await
    }

    async fn discover_skills(
        &self,
        _repo_paths: &[String],
        _boba_skills_dir: &str,
    ) -> anyhow::Result<Vec<Skill>> {
        Ok(self.skills.lock().map(|s| s.clone()).unwrap_or_default())
    }
}

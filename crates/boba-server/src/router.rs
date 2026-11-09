//! Runner routing: which runner executes a run.
//!
//! Anthropic's terms reserve a Claude subscription for its owner's individual use, so a
//! runner signed in with a subscription is **Personal**: it only runs work its owner started.
//! Everyone else's work goes to a **Shared** runner (API key / Bedrock / Vertex), or to the
//! demo runner, which simulates the run and says so.
//!
//! Slots are the built-in runners plus remote ones ("connect your Claude Code"), which come
//! and go: the table is dynamic and only online runners are routed to.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use boba_core::team::{RunnerAuth, RunnerScope};
use boba_core::{Id, RunnerInfo};
use boba_runner::Runner;
use chrono::Utc;

use crate::db::Db;
use crate::remote::RemoteRunner;

/// One runner the server can hand runs to.
#[derive(Clone)]
pub struct RunnerSlot {
    pub runner: Arc<dyn Runner>,
    pub scope: RunnerScope,
    pub auth: RunnerAuth,
    /// Personal built-in runner: email of the user it works for (resolved lazily: the owner
    /// may sign up after the server starts).
    pub owner_email: Option<String>,
    /// Personal remote runner: the user who registered it.
    pub owner_id: Option<Id>,
    pub claude_version: Option<String>,
    pub host: Option<String>,
    /// Set for remote runners (online state, last seen).
    pub remote: Option<Arc<RemoteRunner>>,
}

impl RunnerSlot {
    pub fn shared(runner: Arc<dyn Runner>, auth: RunnerAuth) -> Self {
        Self {
            runner,
            scope: RunnerScope::Shared,
            auth,
            owner_email: None,
            owner_id: None,
            claude_version: None,
            host: None,
            remote: None,
        }
    }

    pub fn id(&self) -> Id {
        self.runner.id()
    }

    /// Simulated runs (no Claude Code behind them).
    pub fn simulated(&self) -> bool {
        self.auth == RunnerAuth::Demo
    }

    /// Built-in runners are always online; remote ones while their socket is up.
    pub fn online(&self) -> bool {
        self.remote.as_ref().is_none_or(|r| r.is_online())
    }
}

pub struct Runners {
    slots: RwLock<Vec<Arc<RunnerSlot>>>,
    owners: Mutex<HashMap<String, Id>>,
}

impl From<Arc<dyn Runner>> for Runners {
    /// A single shared runner (tests, plain demo mode, API-key servers).
    fn from(r: Arc<dyn Runner>) -> Self {
        Self::new(vec![RunnerSlot::shared(r, RunnerAuth::Demo)])
    }
}

impl Runners {
    /// The first slot is the primary one (skill discovery runs there). Remote runners are
    /// added later and never become primary.
    pub fn new(slots: Vec<RunnerSlot>) -> Self {
        assert!(!slots.is_empty(), "at least one runner");
        Self {
            slots: RwLock::new(slots.into_iter().map(Arc::new).collect()),
            owners: Mutex::new(HashMap::new()),
        }
    }

    fn read(&self) -> Vec<Arc<RunnerSlot>> {
        self.slots.read().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn primary(&self) -> Arc<dyn Runner> {
        self.read()
            .first()
            .map(|s| s.runner.clone())
            .expect("at least one runner")
    }

    pub fn slots(&self) -> Vec<Arc<RunnerSlot>> {
        self.read()
    }

    /// The slot a run was routed to. `None` id (runs from before routing existed) means the
    /// first shared built-in runner: a personal slot is never a fallback.
    pub fn by_id(&self, id: Option<Id>) -> Option<Arc<RunnerSlot>> {
        let slots = self.read();
        match id {
            Some(id) => slots.into_iter().find(|s| s.id() == id),
            None => slots
                .into_iter()
                .find(|s| s.scope == RunnerScope::Shared && s.remote.is_none()),
        }
    }

    pub fn contains(&self, id: Id) -> bool {
        self.read().iter().any(|s| s.id() == id)
    }

    /// True when at least one slot runs real Claude Code.
    pub fn any_real(&self) -> bool {
        self.read().iter().any(|s| !s.simulated())
    }

    /// Add or replace a slot (remote runners on register / hello).
    pub fn upsert(&self, slot: RunnerSlot) {
        if let Ok(mut slots) = self.slots.write() {
            let slot = Arc::new(slot);
            match slots.iter_mut().find(|s| s.id() == slot.id()) {
                Some(existing) => *existing = slot,
                None => slots.push(slot),
            }
        }
    }

    /// Remove a remote runner (built-in ones stay).
    pub fn remove(&self, id: Id) {
        if let Ok(mut slots) = self.slots.write() {
            slots.retain(|s| s.id() != id || s.remote.is_none());
        }
    }

    async fn owner_of(&self, db: &Db, slot: &RunnerSlot) -> Option<Id> {
        if let Some(id) = slot.owner_id {
            return Some(id);
        }
        let email = slot.owner_email.as_ref()?.trim().to_lowercase();
        if let Some(id) = self.owners.lock().ok().and_then(|m| m.get(&email).copied()) {
            return Some(id);
        }
        let (user, _) = db.user_by_email(&email).await.ok()??;
        if let Ok(mut m) = self.owners.lock() {
            m.insert(email, user.id);
        }
        Some(user.id)
    }

    /// Pick an online runner for work a human (`for_user`) is responsible for:
    /// 1. that human's personal runner (a connected remote one first),
    /// 2. a real shared runner,
    /// 3. a simulated shared runner,
    /// 4. none: nothing may run it (the caller refuses the run).
    ///
    /// A personal runner is never used for anyone but its owner.
    pub async fn route(&self, db: &Db, for_user: Option<Id>) -> Option<Arc<RunnerSlot>> {
        let slots: Vec<Arc<RunnerSlot>> = self.read().into_iter().filter(|s| s.online()).collect();
        if let Some(u) = for_user {
            let mut mine = Vec::new();
            for s in &slots {
                if s.scope == RunnerScope::Personal && self.owner_of(db, s).await == Some(u) {
                    mine.push(s.clone());
                }
            }
            mine.sort_by_key(|s| (s.remote.is_none(), s.simulated()));
            if let Some(s) = mine.into_iter().next() {
                return Some(s);
            }
        }
        let shared = || slots.iter().filter(|s| s.scope == RunnerScope::Shared);
        shared()
            .find(|s| !s.simulated())
            .or_else(|| shared().next())
            .cloned()
    }

    /// Runner cards for the snapshot. `active` = running/waiting runs per runner id.
    pub async fn infos(&self, db: &Db, active: &HashMap<Id, u32>) -> Vec<RunnerInfo> {
        let slots = self.read();
        let mut out = Vec::with_capacity(slots.len());
        for s in &slots {
            let online = s.online();
            out.push(RunnerInfo {
                id: s.id(),
                name: s.runner.name().to_string(),
                online,
                max_concurrent_runs: s.runner.max_concurrent_runs(),
                active_runs: active.get(&s.id()).copied().unwrap_or(0),
                last_seen_at: match &s.remote {
                    Some(r) if !online => r.last_seen(),
                    _ => Some(Utc::now()),
                },
                scope: s.scope,
                owner_user_id: self.owner_of(db, s).await,
                auth: s.auth,
                host: s.host.clone(),
                claude_version: s.claude_version.clone(),
            });
        }
        out
    }
}

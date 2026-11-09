//! Runner routing: which runner executes a run.
//!
//! Anthropic's terms reserve a Claude subscription for its owner's individual use, so a
//! runner signed in with a subscription is **Personal**: it only runs work its owner started.
//! Everyone else's work goes to a **Shared** runner (API key / Bedrock / Vertex), or to the
//! demo runner, which simulates the run and says so.
//!
//! The same rules will apply to remote runners ("connect your Claude Code").

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use boba_core::team::{RunnerAuth, RunnerScope};
use boba_core::{Id, RunnerInfo};
use boba_runner::Runner;
use chrono::Utc;

use crate::db::Db;

/// One runner the server can hand runs to.
pub struct RunnerSlot {
    pub runner: Arc<dyn Runner>,
    pub scope: RunnerScope,
    pub auth: RunnerAuth,
    /// Personal runners: email of the user they work for (resolved lazily: the owner may
    /// sign up after the server starts).
    pub owner_email: Option<String>,
    pub claude_version: Option<String>,
    pub host: Option<String>,
}

impl RunnerSlot {
    pub fn shared(runner: Arc<dyn Runner>, auth: RunnerAuth) -> Self {
        Self {
            runner,
            scope: RunnerScope::Shared,
            auth,
            owner_email: None,
            claude_version: None,
            host: None,
        }
    }

    pub fn id(&self) -> Id {
        self.runner.id()
    }

    /// Simulated runs (no Claude Code behind them).
    pub fn simulated(&self) -> bool {
        self.auth == RunnerAuth::Demo
    }
}

pub struct Runners {
    slots: Vec<RunnerSlot>,
    owners: Mutex<HashMap<String, Id>>,
}

impl From<Arc<dyn Runner>> for Runners {
    /// A single shared runner (tests, plain demo mode, API-key servers).
    fn from(r: Arc<dyn Runner>) -> Self {
        Self::new(vec![RunnerSlot::shared(r, RunnerAuth::Demo)])
    }
}

impl Runners {
    /// The first slot is the primary one (skill discovery runs there).
    pub fn new(slots: Vec<RunnerSlot>) -> Self {
        assert!(!slots.is_empty(), "at least one runner");
        Self {
            slots,
            owners: Mutex::new(HashMap::new()),
        }
    }

    pub fn primary(&self) -> &Arc<dyn Runner> {
        &self.slots[0].runner
    }

    pub fn slots(&self) -> &[RunnerSlot] {
        &self.slots
    }

    /// The slot a run was routed to. `None` id (runs from before routing existed) means the
    /// first shared runner: a personal slot is never a fallback.
    pub fn by_id(&self, id: Option<Id>) -> Option<&RunnerSlot> {
        match id {
            Some(id) => self.slots.iter().find(|s| s.id() == id),
            None => self.slots.iter().find(|s| s.scope == RunnerScope::Shared),
        }
    }

    pub fn contains(&self, id: Id) -> bool {
        self.slots.iter().any(|s| s.id() == id)
    }

    /// True when at least one slot runs real Claude Code.
    pub fn any_real(&self) -> bool {
        self.slots.iter().any(|s| !s.simulated())
    }

    async fn owner_id(&self, db: &Db, slot: &RunnerSlot) -> Option<Id> {
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

    /// Pick the runner for work a human (`for_user`) is responsible for:
    /// 1. that human's personal runner,
    /// 2. a real shared runner,
    /// 3. a simulated shared runner,
    /// 4. none: nothing may run it (the caller refuses the run).
    ///
    /// A personal runner is never used for anyone but its owner.
    pub async fn route(&self, db: &Db, for_user: Option<Id>) -> Option<&RunnerSlot> {
        if let Some(u) = for_user {
            for s in &self.slots {
                if s.scope == RunnerScope::Personal && self.owner_id(db, s).await == Some(u) {
                    return Some(s);
                }
            }
        }
        let shared = || self.slots.iter().filter(|s| s.scope == RunnerScope::Shared);
        shared()
            .find(|s| !s.simulated())
            .or_else(|| shared().next())
    }

    /// Runner cards for the snapshot. `active` = running/waiting runs per runner id.
    pub async fn infos(&self, db: &Db, active: &HashMap<Id, u32>) -> Vec<RunnerInfo> {
        let mut out = Vec::with_capacity(self.slots.len());
        for s in &self.slots {
            out.push(RunnerInfo {
                id: s.id(),
                name: s.runner.name().to_string(),
                online: true,
                max_concurrent_runs: s.runner.max_concurrent_runs(),
                active_runs: active.get(&s.id()).copied().unwrap_or(0),
                last_seen_at: Some(Utc::now()),
                scope: s.scope,
                owner_user_id: self.owner_id(db, s).await,
                auth: s.auth,
                host: s.host.clone(),
                claude_version: s.claude_version.clone(),
            });
        }
        out
    }
}

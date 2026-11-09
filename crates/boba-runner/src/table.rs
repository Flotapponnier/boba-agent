//! Shared run bookkeeping for runners: control channels, the concurrency limit, and the
//! task wrapper that guarantees exactly one `Finished` per run.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail};
use boba_core::runner::RunOutcome;
use boba_core::{Id, RunStatus};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

use crate::emit::{self, Closed, Emitter};

/// Server → run control messages.
#[derive(Debug)]
pub enum Control {
    Cancel,
    Message(String),
    Permission { request_id: String, allow: bool },
}

pub type ControlRx = mpsc::UnboundedReceiver<Control>;

/// Why a run script stopped early.
#[derive(Debug)]
pub enum Stop {
    Cancelled,
    /// The server dropped its receiver: nobody listens, finish silently.
    Closed,
}

impl From<Closed> for Stop {
    fn from(_: Closed) -> Self {
        Stop::Closed
    }
}

pub struct RunTable {
    runs: Mutex<HashMap<Id, mpsc::UnboundedSender<Control>>>,
    slots: Arc<Semaphore>,
}

impl RunTable {
    pub fn new(max_concurrent_runs: u32) -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(max_concurrent_runs.max(1) as usize)),
        }
    }

    /// Runs queued or executing.
    pub fn len(&self) -> usize {
        self.runs.lock().map(|r| r.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn register(&self, run_id: Id) -> anyhow::Result<ControlRx> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("runner state poisoned"))?;
        if runs.contains_key(&run_id) {
            bail!("run {run_id} is already active");
        }
        let (tx, rx) = mpsc::unbounded_channel();
        runs.insert(run_id, tx);
        Ok(rx)
    }

    fn remove(&self, run_id: Id) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&run_id);
        }
    }

    pub fn send(&self, run_id: Id, c: Control) -> anyhow::Result<()> {
        let runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("runner state poisoned"))?;
        let tx = runs
            .get(&run_id)
            .ok_or_else(|| anyhow!("run {run_id} is not active"))?;
        tx.send(c)
            .map_err(|_| anyhow!("run {run_id} already finished"))
    }

    /// Wait for a slot; `None` if cancelled while queued. Other control messages sent
    /// while queued are dropped (there is no process to deliver them to yet).
    async fn acquire(&self, run_id: Id, rx: &mut ControlRx) -> Option<OwnedSemaphorePermit> {
        loop {
            tokio::select! {
                p = self.slots.clone().acquire_owned() => return p.ok(),
                c = rx.recv() => match c {
                    None | Some(Control::Cancel) => return None,
                    Some(other) => tracing::debug!(%run_id, ?other, "dropping control message for a queued run"),
                },
            }
        }
    }

    /// Register `run_id` and run `script` in the background once a slot is free.
    ///
    /// Guarantees: exactly one `Finished` (unless the receiver is gone), also on panic
    /// (`Failed`) and on cancel (`Cancelled`, even while queued, then without `Started`).
    /// The slot is held until `Finished` has been sent.
    pub fn spawn<F, Fut>(self: &Arc<Self>, run_id: Id, em: Emitter, script: F) -> anyhow::Result<()>
    where
        F: FnOnce(Emitter, ControlRx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<RunOutcome, Stop>> + Send + 'static,
    {
        let mut rx = self.register(run_id)?;
        let table = self.clone();
        tokio::spawn(async move {
            let permit = table.acquire(run_id, &mut rx).await;
            let joined = match &permit {
                None => Ok(Err(Stop::Cancelled)),
                Some(_) => tokio::spawn(script(em.clone(), rx)).await,
            };
            let outcome = match joined {
                Ok(Ok(outcome)) => Some(outcome),
                Ok(Err(Stop::Cancelled)) => {
                    let _ = em.system("Run cancelled").await;
                    Some(emit::outcome(RunStatus::Cancelled))
                }
                Ok(Err(Stop::Closed)) => None,
                Err(e) => {
                    tracing::error!(%run_id, "run task crashed: {e}");
                    Some(emit::failed(format!("runner crashed: {e}")))
                }
            };
            table.remove(run_id);
            if let Some(outcome) = outcome {
                let _ = em.finished(outcome).await;
            }
            drop(permit);
        });
        Ok(())
    }
}

//! Realtime fan-out: one broadcast channel for board events, plus per-run log channels
//! that only reach the sockets that subscribed to that run.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use boba_core::event::{PresenceUser, ServerMessage};
use boba_core::{Id, RunLogLine};
use tokio::sync::{broadcast, mpsc};

/// Lines kept in memory per live run, replayed to late subscribers.
pub const REPLAY_LINES: usize = 200;

pub type ConnId = u64;
pub type ConnTx = mpsc::Sender<ServerMessage>;

#[derive(Default)]
struct RunChannel {
    tail: VecDeque<RunLogLine>,
    subscribers: HashMap<ConnId, ConnTx>,
}

struct Presence {
    user: PresenceUser,
}

pub struct Hub {
    tx: broadcast::Sender<ServerMessage>,
    runs: Mutex<HashMap<Id, RunChannel>>,
    presence: Mutex<HashMap<ConnId, Presence>>,
    next_conn: std::sync::atomic::AtomicU64,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            tx,
            runs: Mutex::new(HashMap::new()),
            presence: Mutex::new(HashMap::new()),
            next_conn: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.tx.subscribe()
    }

    /// Send to every connected socket. No receivers is fine.
    pub fn broadcast(&self, msg: ServerMessage) {
        let _ = self.tx.send(msg);
    }

    pub fn new_conn_id(&self) -> ConnId {
        self.next_conn
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Fan a live log line out to the run's subscribers and keep it in the replay tail.
    pub fn push_log(&self, line: RunLogLine) {
        let Ok(mut runs) = self.runs.lock() else {
            return;
        };
        let Some(ch) = runs.get_mut(&line.run_id) else {
            return;
        };
        ch.subscribers.retain(|_, tx| {
            !matches!(
                tx.try_send(ServerMessage::RunLog { line: line.clone() }),
                Err(mpsc::error::TrySendError::Closed(_))
            )
        });
        ch.tail.push_back(line);
        while ch.tail.len() > REPLAY_LINES {
            ch.tail.pop_front();
        }
    }

    /// Create the live channel of a run (called when the run starts).
    pub fn open_run(&self, run_id: Id) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.entry(run_id).or_default();
        }
    }

    /// Subscribe a connection to a live run. Returns the in-memory replay (taken atomically
    /// with registration, so no line is lost or duplicated), or `None` when the run is not
    /// live and the caller must replay from the database.
    pub fn subscribe_run(&self, run_id: Id, conn: ConnId, tx: ConnTx) -> Option<Vec<RunLogLine>> {
        let mut runs = self.runs.lock().ok()?;
        let ch = runs.get_mut(&run_id)?;
        ch.subscribers.insert(conn, tx);
        Some(ch.tail.iter().cloned().collect())
    }

    pub fn unsubscribe_run(&self, run_id: Id, conn: ConnId) {
        if let Ok(mut runs) = self.runs.lock() {
            if let Some(ch) = runs.get_mut(&run_id) {
                ch.subscribers.remove(&conn);
            }
        }
    }

    /// Drop the in-memory channel of a finished run (the DB has every line).
    pub fn close_run(&self, run_id: Id) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&run_id);
        }
    }

    pub fn drop_conn(&self, conn: ConnId) {
        if let Ok(mut runs) = self.runs.lock() {
            for ch in runs.values_mut() {
                ch.subscribers.remove(&conn);
            }
        }
        if let Ok(mut p) = self.presence.lock() {
            p.remove(&conn);
        }
        self.broadcast_presence();
    }

    pub fn set_presence(&self, conn: ConnId, user: PresenceUser) {
        if let Ok(mut p) = self.presence.lock() {
            p.insert(conn, Presence { user });
        }
        self.broadcast_presence();
    }

    /// One entry per user (several tabs open = one avatar, last viewed tab wins).
    pub fn presence_users(&self) -> Vec<PresenceUser> {
        let Ok(p) = self.presence.lock() else {
            return Vec::new();
        };
        let mut conns: Vec<(&ConnId, &Presence)> = p.iter().collect();
        conns.sort_by_key(|(c, _)| **c);
        let mut out: Vec<PresenceUser> = Vec::new();
        for (_, pr) in conns {
            if let Some(existing) = out.iter_mut().find(|u| u.user_id == pr.user.user_id) {
                *existing = pr.user.clone();
            } else {
                out.push(pr.user.clone());
            }
        }
        out
    }

    fn broadcast_presence(&self) {
        let users = self.presence_users();
        self.broadcast(ServerMessage::Presence { users });
    }
}

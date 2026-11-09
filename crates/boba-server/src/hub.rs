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
    /// Hashes of sessions that were logged out: their sockets close.
    revoked: broadcast::Sender<String>,
    runs: Mutex<HashMap<Id, RunChannel>>,
    presence: Mutex<HashMap<ConnId, Presence>>,
    /// Every open socket and its user, for messages meant for one user only.
    conns: Mutex<HashMap<ConnId, (Id, ConnTx)>>,
    /// "Send yourself a fresh snapshot" (state that is not an Entity changed, e.g. settings).
    resync: broadcast::Sender<()>,
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
        let (revoked, _) = broadcast::channel(64);
        let (resync, _) = broadcast::channel(16);
        Self {
            tx,
            revoked,
            runs: Mutex::new(HashMap::new()),
            presence: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
            resync,
            next_conn: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.tx.subscribe()
    }

    pub fn register_conn(&self, conn: ConnId, user: Id, tx: ConnTx) {
        if let Ok(mut c) = self.conns.lock() {
            c.insert(conn, (user, tx));
        }
    }

    /// Deliver to every socket of `user` (and nobody else).
    pub fn send_to_user(&self, user: Id, msg: ServerMessage) {
        if let Ok(c) = self.conns.lock() {
            for (uid, tx) in c.values() {
                if *uid == user {
                    let _ = tx.try_send(msg.clone());
                }
            }
        }
    }

    /// Ask every socket to send its own user a fresh snapshot.
    pub fn request_resync(&self) {
        let _ = self.resync.send(());
    }

    pub fn resyncs(&self) -> broadcast::Receiver<()> {
        self.resync.subscribe()
    }

    pub fn revoke_session(&self, token_hash: &str) {
        let _ = self.revoked.send(token_hash.to_string());
    }

    pub fn revocations(&self) -> broadcast::Receiver<String> {
        self.revoked.subscribe()
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
        if let Ok(mut c) = self.conns.lock() {
            c.remove(&conn);
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

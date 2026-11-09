//! WebSocket `/api/ws`: snapshot on connect, board broadcasts, per-run log streams,
//! presence.

use std::collections::HashSet;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use boba_core::event::{ClientMessage, PresenceUser, ServerMessage};
use boba_core::{Id, Permission, User};
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};

use crate::auth::SessionAuth;
use crate::error::{AppError, AppResult};
use crate::hub::{ConnId, REPLAY_LINES};
use crate::state::SharedState;

const PING_EVERY: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_SUBSCRIPTIONS: usize = 32;

pub async fn upgrade(
    State(state): State<SharedState>,
    me: SessionAuth,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    if !me.user.role.grants(Permission::View) {
        return Err(AppError::forbidden("your role cannot view this workspace"));
    }
    let SessionAuth { user, token_hash } = me;
    Ok(ws
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| async move { session(state, user, token_hash, socket).await }))
}

async fn session(state: SharedState, user: User, token_hash: String, socket: WebSocket) {
    let conn = state.hub.new_conn_id();
    let (mut sink, mut stream) = socket.split();
    // Direct messages to this socket (snapshot, run logs). Bounded: a socket that cannot
    // keep up drops log lines rather than stalling the run.
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(1024);
    let mut board = state.hub.subscribe();
    let mut revoked = state.hub.revocations();
    let mut resyncs = state.hub.resyncs();
    state.hub.register_conn(conn, user.id, tx.clone());
    let mut subs: HashSet<Id> = HashSet::new();

    // The snapshot goes straight to the socket so it is always the first message.
    send_snapshot(&state, &user, &tx).await;
    if let Ok(first) = rx.try_recv() {
        if write(&mut sink, &first).await.is_err() {
            return;
        }
    }
    state.hub.set_presence(
        conn,
        PresenceUser {
            user_id: user.id,
            name: user.name.clone(),
            avatar: user.avatar.clone(),
            tab_id: None,
        },
    );

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut last_seen = tokio::time::Instant::now();

    loop {
        tokio::select! {
            msg = board.recv() => match msg {
                Ok(m) => {
                    if write(&mut sink, &m).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(conn, skipped = n, "socket lagged, resyncing");
                    send_snapshot(&state, &user, &tx).await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(m) = rx.recv() => {
                if write(&mut sink, &m).await.is_err() { break; }
            }
            incoming = stream.next() => {
                let Some(Ok(frame)) = incoming else { break };
                last_seen = tokio::time::Instant::now();
                match frame {
                    Message::Text(t) => {
                        match serde_json::from_str::<ClientMessage>(t.as_str()) {
                            Ok(cm) => handle(&state, &user, conn, &tx, &mut subs, cm).await,
                            Err(_) => {
                                let _ = tx.try_send(ServerMessage::Error { message: "unknown message".into() });
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            Ok(()) = resyncs.recv() => send_snapshot(&state, &user, &tx).await,
            r = revoked.recv() => match r {
                Ok(h) if h == token_hash => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
                _ => {}
            },
            _ = ping.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT {
                    break;
                }
                // Session expired or was removed: stop streaming.
                if !matches!(state.db.session_user(&token_hash).await, Ok(Some(_))) {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
                if sink.send(Message::Ping(Default::default())).await.is_err() { break; }
            }
        }
    }
    state.hub.drop_conn(conn);
}

async fn write(
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    m: &ServerMessage,
) -> Result<(), axum::Error> {
    match serde_json::to_string(m) {
        Ok(s) => sink.send(Message::Text(s.into())).await,
        Err(e) => {
            tracing::error!(error = ?e, "serialising ws message");
            Ok(())
        }
    }
}

async fn send_snapshot(state: &SharedState, user: &User, tx: &mpsc::Sender<ServerMessage>) {
    // Re-read the user: their role may have changed since connecting.
    let me = state
        .db
        .user(user.id)
        .await
        .ok()
        .flatten()
        .or_else(|| Some(user.clone()));
    match state.snapshot(me).await {
        Ok(snap) => {
            let _ = tx.send(ServerMessage::Snapshot { state: snap }).await;
        }
        Err(e) => {
            tracing::error!(error = ?e, "building snapshot");
            let _ = tx.try_send(ServerMessage::Error {
                message: "could not load the board".into(),
            });
        }
    }
}

async fn handle(
    state: &SharedState,
    user: &User,
    conn: ConnId,
    tx: &mpsc::Sender<ServerMessage>,
    subs: &mut HashSet<Id>,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Ping => {}
        ClientMessage::Resync => send_snapshot(state, user, tx).await,
        ClientMessage::Viewing { tab_id } => state.hub.set_presence(
            conn,
            PresenceUser {
                user_id: user.id,
                name: user.name.clone(),
                avatar: user.avatar.clone(),
                tab_id,
            },
        ),
        ClientMessage::Subscribe { run_ids } => {
            for run_id in run_ids {
                if subs.len() >= MAX_SUBSCRIPTIONS && !subs.contains(&run_id) {
                    let _ = tx.try_send(ServerMessage::Error {
                        message: format!("too many log subscriptions (max {MAX_SUBSCRIPTIONS})"),
                    });
                    break;
                }
                subs.insert(run_id);
                let replay = match state.hub.subscribe_run(run_id, conn, tx.clone()) {
                    Some(lines) => lines,
                    None => state
                        .db
                        .tail_logs(run_id, REPLAY_LINES as u32)
                        .await
                        .unwrap_or_default(),
                };
                for line in replay {
                    if tx.send(ServerMessage::RunLog { line }).await.is_err() {
                        return;
                    }
                }
            }
        }
        ClientMessage::Unsubscribe { run_ids } => {
            for run_id in run_ids {
                subs.remove(&run_id);
                state.hub.unsubscribe_run(run_id, conn);
            }
        }
    }
}

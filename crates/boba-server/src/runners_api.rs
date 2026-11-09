//! Registering remote runners and their WebSocket (`/api/runners/ws`).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use boba_core::api::{CreateRunner, RunnerToken};
use boba_core::event::{Entity, EntityKind, HistoryKind};
use boba_core::runner::RunnerWire;
use boba_core::team::{RunnerAuth, RunnerScope};
use boba_core::*;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::auth::{self, Auth};
use crate::db::{new_id, RemoteRunnerRow};
use crate::error::{AppError, AppResult};
use crate::remote::{RemoteRunner, RESUME_GRACE};
use crate::router::RunnerSlot;
use crate::state::{Actor, Refs, SharedState};

const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const PING_EVERY: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

fn slot(row: &RemoteRunnerRow, remote: &Arc<RemoteRunner>) -> RunnerSlot {
    RunnerSlot {
        runner: remote.clone(),
        scope: row.scope,
        auth: row.auth,
        owner_email: None,
        owner_id: if row.scope == RunnerScope::Personal {
            row.owner_user_id
        } else {
            None
        },
        claude_version: row.claude_version.clone(),
        host: row.host.clone(),
        remote: Some(remote.clone()),
    }
}

/// Startup: every registered runner is listed, offline until it dials in.
pub async fn load(state: &SharedState) -> anyhow::Result<()> {
    for row in state.db.remote_runners().await? {
        let remote = Arc::new(RemoteRunner::new(
            row.id,
            row.name.clone(),
            row.max_concurrent_runs,
            row.last_seen_at,
        ));
        state.runners.upsert(slot(&row, &remote));
        if let Ok(mut m) = state.remotes.lock() {
            m.insert(row.id, remote);
        }
    }
    Ok(())
}

async fn broadcast_runner(state: &SharedState, id: Id) {
    if let Ok(infos) = state.runner_infos().await {
        if let Some(info) = infos.into_iter().find(|i| i.id == id) {
            state.upsert(Entity::Runner(info));
        }
    }
}

// ---------------------------------------------------------------- REST

pub async fn create(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateRunner>,
) -> AppResult<Json<RunnerToken>> {
    // Personal: any member, for themselves (they bring their own Claude). Shared: admins.
    let me = match req.scope {
        RunnerScope::Personal => me.require(Permission::RunAgents)?.clone(),
        RunnerScope::Shared => me.require(Permission::ManageWorkspace)?.clone(),
    };
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::bad("runner names must be 1 to 60 characters"));
    }
    let token = auth::new_token();
    let row = RemoteRunnerRow {
        id: new_id(),
        name,
        scope: req.scope,
        owner_user_id: (req.scope == RunnerScope::Personal).then_some(me.id),
        auth: RunnerAuth::Unknown,
        host: None,
        claude_version: None,
        max_concurrent_runs: req.max_concurrent_runs.unwrap_or(2).clamp(1, 16),
        last_seen_at: None,
    };
    state
        .db
        .insert_remote_runner(&row, &auth::hash_token(&token), me.id)
        .await?;
    let remote = Arc::new(RemoteRunner::new(
        row.id,
        row.name.clone(),
        row.max_concurrent_runs,
        None,
    ));
    state.runners.upsert(slot(&row, &remote));
    if let Ok(mut m) = state.remotes.lock() {
        m.insert(row.id, remote);
    }
    let info = state
        .runner_infos()
        .await?
        .into_iter()
        .find(|i| i.id == row.id)
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("runner vanished")))?;
    state.upsert(Entity::Runner(info.clone()));
    Ok(Json(RunnerToken {
        runner: info,
        command: format!(
            "boba-agent runner --server {} --token {token}",
            state.cfg.public_url
        ),
        token,
    }))
}

pub async fn delete(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::View)?.clone();
    let row = state
        .db
        .remote_runner(id)
        .await?
        .ok_or(AppError::NotFound("runner"))?;
    let owner = row.owner_user_id == Some(me.id) && row.scope == RunnerScope::Personal;
    if !owner && !me.role.grants(Permission::ManageWorkspace) {
        return Err(AppError::forbidden(
            "only its owner or an admin can remove a runner",
        ));
    }
    state.db.delete_remote_runner(id).await?;
    let remote = state.remotes.lock().ok().and_then(|mut m| m.remove(&id));
    state.runners.remove(id);
    if let Some(r) = remote {
        r.kick();
        r.fail_all("the runner was removed").await;
    }
    state.deleted(EntityKind::Runner, id);
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- WebSocket

pub async fn upgrade(
    State(state): State<SharedState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(AppError::Unauthorized)?;
    let row = state
        .db
        .remote_runner_by_token(&auth::hash_token(token))
        .await?
        .ok_or(AppError::Unauthorized)?;
    Ok(ws
        .max_message_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| async move { session(state, row, socket).await }))
}

async fn close(mut socket: WebSocket, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 4003,
            reason: reason.chars().take(120).collect::<String>().into(),
        })))
        .await;
}

/// Parsed incoming frame. `resume` is matched by hand so the server already accepts it
/// before the contract carries it.
enum Incoming {
    Wire(RunnerWire),
    Resume(Vec<Id>),
}

fn parse(text: &str) -> Option<Incoming> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("type").and_then(|t| t.as_str()) == Some("resume") {
        let ids = v
            .get("run_ids")?
            .as_array()?
            .iter()
            .filter_map(|i| i.as_str())
            .filter_map(|s| Id::parse_str(s).ok())
            .collect();
        return Some(Incoming::Resume(ids));
    }
    serde_json::from_value(v).ok().map(Incoming::Wire)
}

async fn session(state: SharedState, row: RemoteRunnerRow, mut socket: WebSocket) {
    // 1. Hello first.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, async {
        while let Some(Ok(m)) = socket.recv().await {
            if let Message::Text(t) = m {
                return parse(t.as_str());
            }
        }
        None
    })
    .await;
    let Ok(Some(Incoming::Wire(RunnerWire::Hello {
        max_concurrent_runs,
        auth,
        host,
        claude_version,
        ..
    }))) = hello
    else {
        close(socket, "expected a hello message first").await;
        return;
    };
    // Anthropic's terms: a Claude subscription only ever runs its owner's work.
    // `auth` is self-reported by the runner and cannot be verified from here: this rule
    // relies on honest runners (shared ones are registered by admins).
    if row.scope == RunnerScope::Shared && auth == RunnerAuth::Subscription {
        close(
            socket,
            "shared runners must use an API key, Bedrock or Vertex; register a personal runner to use your Claude subscription",
        )
        .await;
        return;
    }
    let Some(remote) = state
        .remotes
        .lock()
        .ok()
        .and_then(|m| m.get(&row.id).cloned())
    else {
        close(socket, "this runner was removed").await;
        return;
    };
    remote.set_max_runs(max_concurrent_runs);
    if let Err(e) = state
        .db
        .update_remote_hello(row.id, auth, &host, claude_version.as_deref())
        .await
    {
        tracing::warn!(error = ?e, "saving runner hello");
    }
    let mut updated = row.clone();
    updated.auth = auth;
    updated.host = Some(host.clone());
    updated.claude_version = claude_version.clone();
    state.runners.upsert(slot(&updated, &remote));

    let (tx, mut rx) = mpsc::channel::<RunnerWire>(256);
    let generation = remote.attach(tx);
    broadcast_runner(&state, row.id).await;
    let who = match row.owner_user_id {
        Some(u) => state
            .db
            .user(u)
            .await
            .ok()
            .flatten()
            .map(|u| format!("{}'s runner", u.name)),
        None => None,
    }
    .unwrap_or_else(|| "Shared runner".into());
    state
        .record(
            HistoryKind::RunnerConnected,
            Actor::System,
            Refs::default(),
            format!("{who} \"{}\" connected from {host}", row.name),
            Some(json!({ "runner_id": row.id, "auth": auth })),
        )
        .await;

    // 2. Relay.
    let (mut sink, mut stream) = socket.split();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut last_seen = tokio::time::Instant::now();
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(msg) = out else { break }; // kicked (removed) or replaced
                let Ok(text) = serde_json::to_string(&msg) else { continue };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = stream.next() => {
                let Some(Ok(frame)) = incoming else { break };
                last_seen = tokio::time::Instant::now();
                match frame {
                    Message::Text(t) => match parse(t.as_str()) {
                        Some(Incoming::Wire(RunnerWire::Update { run_id, update })) => remote.deliver(run_id, update).await,
                        Some(Incoming::Wire(RunnerWire::Skills { skills })) => remote.set_skills(skills),
                        Some(Incoming::Resume(ids)) => remote.resume(&ids).await,
                        _ => {}
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT {
                    break;
                }
                remote.touch();
                let _ = state.db.touch_remote_runner(row.id).await;
                if sink.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
        }
    }

    // 3. Gone: offline now, its runs get a grace period to be resumed.
    if !remote.detach(generation) {
        return; // a newer connection took over
    }
    let _ = state.db.touch_remote_runner(row.id).await;
    broadcast_runner(&state, row.id).await;
    state
        .record(
            HistoryKind::RunnerDisconnected,
            Actor::System,
            Refs::default(),
            format!("{who} \"{}\" disconnected", row.name),
            Some(json!({ "runner_id": row.id })),
        )
        .await;
    let gen_at_close = remote.generation();
    tokio::spawn(async move {
        tokio::time::sleep(RESUME_GRACE).await;
        if !remote.is_online() && remote.generation() == gen_at_close {
            remote
                .fail_all("the runner disconnected and did not come back")
                .await;
        }
    });
}

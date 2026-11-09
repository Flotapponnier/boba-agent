//! Snapshot, history, settings, tabs/columns, repos.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use boba_core::api::*;
use boba_core::event::{Entity, EntityKind, HistoryEvent, HistoryKind};
use boba_core::*;
use serde::Deserialize;

use crate::auth::Auth;
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::orchestrator;
use crate::seed::{column_color, default_columns};
use crate::state::{Actor, Refs, SharedState};

pub async fn snapshot(State(state): State<SharedState>, me: Auth) -> AppResult<Json<Snapshot>> {
    me.require(Permission::View)?;
    Ok(Json(state.snapshot(Some(me.0)).await?))
}

pub async fn history(
    State(state): State<SharedState>,
    me: Auth,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<Vec<HistoryEvent>>> {
    me.require(Permission::View)?;
    Ok(Json(state.db.history(&q).await?))
}

/// Partial settings. `demo_mode` is runtime state and is ignored.
#[derive(Debug, Deserialize)]
pub struct SettingsPatch {
    name: Option<String>,
    theme: Option<String>,
    music_enabled_by_default: Option<bool>,
}

pub async fn update_settings(
    State(state): State<SharedState>,
    me: Auth,
    Json(p): Json<SettingsPatch>,
) -> AppResult<Json<WorkspaceSettings>> {
    me.require(Permission::ManageWorkspace)?;
    let mut s = state.db.workspace_settings(state.cfg.demo_mode).await?;
    if let Some(n) = p.name {
        let n = n.trim();
        if n.is_empty() {
            return Err(AppError::bad("workspace name cannot be empty"));
        }
        s.name = n.chars().take(80).collect();
    }
    if let Some(t) = p.theme {
        s.theme = t.chars().take(40).collect();
    }
    if let Some(m) = p.music_enabled_by_default {
        s.music_enabled_by_default = m;
    }
    state.db.save_workspace_settings(&s).await?;
    // Settings are not an Entity: clients pick them up via a fresh snapshot on resync;
    // a full snapshot push keeps every screen in sync right away.
    if let Ok(snap) = state.snapshot(None).await {
        state
            .hub
            .broadcast(boba_core::event::ServerMessage::Snapshot { state: snap });
    }
    Ok(Json(s))
}

// ---------------------------------------------------------------- tabs

fn check_color(c: &Option<String>) -> AppResult<()> {
    if let Some(c) = c {
        if c.len() > 32 {
            return Err(AppError::bad("colour is too long"));
        }
    }
    Ok(())
}

/// Turn a column spec list into columns for `tab_id`, keeping ids of existing ones.
fn build_columns(
    tab_id: Id,
    existing: &[Column],
    specs: Vec<ColumnSpec>,
    agents: &[Agent],
) -> AppResult<Vec<Column>> {
    if specs.is_empty() {
        return Err(AppError::bad("a tab needs at least one column"));
    }
    if specs.len() > 20 {
        return Err(AppError::bad("a tab can have at most 20 columns"));
    }
    let mut cols = Vec::with_capacity(specs.len());
    for (i, s) in specs.into_iter().enumerate() {
        let name = s.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 40 {
            return Err(AppError::bad("column names must be 1 to 40 characters"));
        }
        check_color(&s.color)?;
        let id = match s.id {
            Some(id) if existing.iter().any(|c| c.id == id) => id,
            Some(_) => return Err(AppError::bad("unknown column id in this tab")),
            None => new_id(),
        };
        if let Some(r) = s.auto_reviewer_agent_id {
            if !agents.iter().any(|a| a.id == r) {
                return Err(AppError::bad("auto reviewer agent does not exist"));
            }
        }
        cols.push(Column {
            id,
            tab_id,
            name,
            kind: s.kind,
            color: s.color.unwrap_or_else(|| column_color(s.kind).into()),
            position: i as i32,
            auto_reviewer_agent_id: if s.kind == ColumnKind::Review {
                s.auto_reviewer_agent_id
            } else {
                None
            },
            on_approve_column_id: s.on_approve_column_id,
            on_changes_column_id: s.on_changes_column_id,
        });
    }
    let ids: Vec<Id> = cols.iter().map(|c| c.id).collect();
    for c in &mut cols {
        c.on_approve_column_id = c.on_approve_column_id.filter(|id| ids.contains(id));
        c.on_changes_column_id = c.on_changes_column_id.filter(|id| ids.contains(id));
    }
    Ok(cols)
}

pub async fn create_tab(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateTab>,
) -> AppResult<Json<Tab>> {
    let me = me.require(Permission::ManageWorkspace)?.clone();
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 40 {
        return Err(AppError::bad("tab names must be 1 to 40 characters"));
    }
    check_color(&req.color)?;
    if let Some(r) = req.repo_id {
        state.db.repo(r).await?.ok_or(AppError::NotFound("repo"))?;
    }
    let agents = state.db.agents().await?;
    let id = new_id();
    let columns = match req.columns {
        Some(specs) => build_columns(id, &[], specs, &agents)?,
        None => {
            let reviewer = agents
                .iter()
                .find(|a| a.role.eq_ignore_ascii_case("reviewer"))
                .map(|a| a.id);
            default_columns(id, reviewer)
        }
    };
    let position = state
        .db
        .tabs()
        .await?
        .iter()
        .map(|t| t.position + 1)
        .max()
        .unwrap_or(0);
    let tab = Tab {
        id,
        name,
        icon: req.icon.unwrap_or_else(|| "🧋".into()),
        color: req.color.unwrap_or_else(|| "#c98b5a".into()),
        position,
        repo_id: req.repo_id,
        columns,
    };
    state.db.save_tab(&tab).await?;
    state.upsert(Entity::Tab(tab.clone()));
    state
        .record(
            HistoryKind::TabCreated,
            Actor::User(me.id),
            Refs {
                tab: Some(tab.id),
                ..Default::default()
            },
            format!("{} created the {} tab", me.name, tab.name),
            None,
        )
        .await;
    Ok(Json(tab))
}

pub async fn update_tab(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpdateTab>,
) -> AppResult<Json<Tab>> {
    let me = me.require(Permission::ManageWorkspace)?.clone();
    let _g = state.orch_lock.lock().await;
    let mut tab = state.db.tab(id).await?.ok_or(AppError::NotFound("tab"))?;
    if let Some(n) = req.name {
        let n = n.trim().to_string();
        if n.is_empty() || n.chars().count() > 40 {
            return Err(AppError::bad("tab names must be 1 to 40 characters"));
        }
        tab.name = n;
    }
    if let Some(i) = req.icon {
        tab.icon = i.chars().take(16).collect();
    }
    if let Some(c) = req.color {
        check_color(&Some(c.clone()))?;
        tab.color = c;
    }
    if let Some(p) = req.position {
        tab.position = p;
    }
    if let Some(r) = req.repo_id {
        if let Some(r) = r {
            state.db.repo(r).await?.ok_or(AppError::NotFound("repo"))?;
        }
        tab.repo_id = r;
    }
    let mut moved = Vec::new();
    if let Some(specs) = req.columns {
        let agents = state.db.agents().await?;
        let cols = build_columns(tab.id, &tab.columns, specs, &agents)?;
        // Tickets in removed columns go to the first remaining column of the same kind
        // (or the first column).
        let removed: Vec<&Column> = tab
            .columns
            .iter()
            .filter(|c| !cols.iter().any(|n| n.id == c.id))
            .collect();
        if !removed.is_empty() {
            // Appended at the bottom of their new column, in their previous order.
            let mut bottoms: std::collections::HashMap<Id, f64> = std::collections::HashMap::new();
            for t in state.db.tickets_in_tab(tab.id).await? {
                if let Some(old) = removed.iter().find(|c| c.id == t.column_id) {
                    let dest = cols.iter().find(|c| c.kind == old.kind).unwrap_or(&cols[0]);
                    let bottom = match bottoms.get(&dest.id) {
                        Some(b) => *b,
                        None => state.db.max_position(dest.id).await?,
                    } + 1024.0;
                    bottoms.insert(dest.id, bottom);
                    moved.push((t.id, dest.id, bottom));
                }
            }
        }
        tab.columns = cols;
    }
    state.db.save_tab_moving(&tab, &moved).await?;
    for (tid, _, _) in &moved {
        if let Some(t) = state.db.ticket(*tid).await? {
            state.upsert(Entity::Ticket(t));
        }
    }
    state.upsert(Entity::Tab(tab.clone()));
    state
        .record(
            HistoryKind::TabUpdated,
            Actor::User(me.id),
            Refs {
                tab: Some(tab.id),
                ..Default::default()
            },
            format!("{} updated the {} tab", me.name, tab.name),
            None,
        )
        .await;
    // New or changed auto reviewers may have tickets waiting.
    orchestrator::drain_all_locked(&state).await;
    Ok(Json(tab))
}

pub async fn delete_tab(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::ManageWorkspace)?.clone();
    let _g = state.orch_lock.lock().await;
    let tab = state.db.tab(id).await?.ok_or(AppError::NotFound("tab"))?;
    let active = state.db.active_runs().await?;
    let tickets = state.db.tickets_in_tab(id).await?;
    if active
        .iter()
        .any(|r| tickets.iter().any(|t| Some(t.id) == r.ticket_id))
    {
        return Err(AppError::conflict(
            "agents are still working in this tab; cancel their runs first",
        ));
    }
    state.db.delete_tab(id).await?;
    for t in &tickets {
        state.deleted(EntityKind::Ticket, t.id);
    }
    state.deleted(EntityKind::Tab, id);
    state
        .record(
            HistoryKind::TabDeleted,
            Actor::User(me.id),
            Refs::default(),
            format!(
                "{} deleted the {} tab ({} tickets)",
                me.name,
                tab.name,
                tickets.len()
            ),
            Some(serde_json::json!({ "tab_id": id })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- repos

pub async fn create_repo(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateRepo>,
) -> AppResult<Json<Repo>> {
    me.require(Permission::ManageWorkspace)?;
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad("repository name is required"));
    }
    let path = req.path.trim().to_string();
    let local = req.runner_id.is_none() || req.runner_id == Some(state.runner.id());
    if local && !state.cfg.demo_mode {
        let p = std::path::Path::new(&path);
        if !p.is_absolute() {
            return Err(AppError::bad("repository path must be absolute"));
        }
        if !p.join(".git").exists() {
            return Err(AppError::bad(format!("{path} is not a git repository")));
        }
    }
    let repo = Repo {
        id: new_id(),
        name,
        path,
        remote_url: req.remote_url.filter(|s| !s.trim().is_empty()),
        default_branch: req
            .default_branch
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "main".into()),
        runner_id: req.runner_id,
        sync: req.sync.unwrap_or_default(),
    };
    state.db.save_repo(&repo).await?;
    state.upsert(Entity::Repo(repo.clone()));
    // Project skills of the new repo become available.
    let s2 = state.clone();
    tokio::spawn(async move {
        if let Ok(skills) = crate::skills::rescan(&s2).await {
            for sk in skills {
                s2.upsert(Entity::Skill(sk));
            }
        }
    });
    Ok(Json(repo))
}

pub async fn delete_repo(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    me.require(Permission::ManageWorkspace)?;
    if !state.db.delete_repo(id).await? {
        return Err(AppError::NotFound("repo"));
    }
    state.deleted(EntityKind::Repo, id);
    // Tabs and tickets that pointed at it changed.
    for t in state.db.tabs().await? {
        state.upsert(Entity::Tab(t));
    }
    Ok(StatusCode::NO_CONTENT)
}

//! Agents, tickets, runs, skills.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use boba_core::api::*;
use boba_core::automation::TicketSource;
use boba_core::event::{Entity, EntityKind, HistoryEvent, HistoryKind};
use boba_core::*;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use crate::auth::Auth;
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::orchestrator::{self, tref, NewRun, Trigger};
use crate::skills;
use crate::state::{Actor, Refs, SharedState};

const MAX_PROMPT: usize = 20_000;
const MAX_BODY: usize = 50_000;

fn text(field: &str, v: &str, max: usize, required: bool) -> AppResult<String> {
    let t = v.trim();
    if required && t.is_empty() {
        return Err(AppError::bad(format!("{field} is required")));
    }
    if t.chars().count() > max {
        return Err(AppError::bad(format!(
            "{field} is too long (max {max} characters)"
        )));
    }
    Ok(t.to_string())
}

// ---------------------------------------------------------------- agents

fn check_mode(state: &SharedState, me: &User, mode: PermissionMode) -> AppResult<()> {
    if mode == PermissionMode::BypassPermissions {
        if me.role < Role::Admin {
            return Err(AppError::forbidden(
                "only admins can give an agent bypassPermissions",
            ));
        }
        if !state.cfg.allow_bypass {
            return Err(AppError::bad(
                "bypassPermissions is disabled: run the server in a container with BOBA_ALLOW_BYPASS=1",
            ));
        }
    }
    Ok(())
}

async fn check_skills(state: &SharedState, ids: &[Id]) -> AppResult<()> {
    let all = state.db.skills().await?;
    if let Some(bad) = ids.iter().find(|id| !all.iter().any(|s| s.id == **id)) {
        return Err(AppError::bad(format!("unknown skill {bad}")));
    }
    Ok(())
}

fn check_sprite(s: &str) -> AppResult<()> {
    if s.is_empty() || s.len() > 64 {
        return Err(AppError::bad("sprite is required"));
    }
    Ok(())
}

pub async fn create_agent(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateAgent>,
) -> AppResult<Json<Agent>> {
    let me = me.require(Permission::ManageAgents)?.clone();
    let mode = req.permission_mode.unwrap_or(PermissionMode::AcceptEdits);
    check_mode(&state, &me, mode)?;
    check_sprite(&req.sprite)?;
    check_skills(&state, &req.skill_ids).await?;
    let agent = Agent {
        id: new_id(),
        name: text("name", &req.name, 40, true)?,
        role: text("role", &req.role, 40, true)?,
        sprite: req.sprite,
        color: req.color.unwrap_or_else(|| "#c98b5a".into()),
        tagline: text("tagline", req.tagline.as_deref().unwrap_or(""), 120, false)?,
        system_prompt: text("system prompt", &req.system_prompt, MAX_PROMPT, false)?,
        skill_ids: req.skill_ids,
        model: req.model.filter(|m| !m.trim().is_empty()),
        permission_mode: mode,
        allowed_tools: req.allowed_tools.unwrap_or_default(),
        max_turns: req.max_turns,
        xp: 0,
        level: 1,
        runs_completed: 0,
        prs_approved: 0,
        status: AgentStatus::Idle,
        current_run_id: None,
        connector_ids: req.connector_ids.unwrap_or_default(),
        board_tools: req.board_tools.unwrap_or(false),
        created_by: Some(me.id),
        created_at: Utc::now(),
    };
    state.db.save_agent(&agent).await?;
    let agent = state.db.agent(agent.id).await?.unwrap_or(agent);
    state.upsert(Entity::Agent(agent.clone()));
    state
        .record(
            HistoryKind::AgentCreated,
            Actor::User(me.id),
            Refs {
                agent: Some(agent.id),
                ..Default::default()
            },
            format!("{} hired {} ({})", me.name, agent.name, agent.role),
            None,
        )
        .await;
    Ok(Json(agent))
}

pub async fn update_agent(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpdateAgent>,
) -> AppResult<Json<Agent>> {
    let me = me.require(Permission::ManageAgents)?.clone();
    let _g = state.orch_lock.lock().await;
    let mut a = state
        .db
        .agent(id)
        .await?
        .ok_or(AppError::NotFound("agent"))?;
    if let Some(v) = req.name {
        a.name = text("name", &v, 40, true)?;
    }
    if let Some(v) = req.role {
        a.role = text("role", &v, 40, true)?;
    }
    if let Some(v) = req.sprite {
        check_sprite(&v)?;
        a.sprite = v;
    }
    if let Some(v) = req.color {
        a.color = v;
    }
    if let Some(v) = req.tagline {
        a.tagline = text("tagline", &v, 120, false)?;
    }
    if let Some(v) = req.system_prompt {
        a.system_prompt = text("system prompt", &v, MAX_PROMPT, false)?;
    }
    if let Some(v) = req.skill_ids {
        check_skills(&state, &v).await?;
        a.skill_ids = v;
    }
    if let Some(v) = req.model {
        a.model = v.filter(|m| !m.trim().is_empty());
    }
    if let Some(v) = req.permission_mode {
        check_mode(&state, &me, v)?;
        a.permission_mode = v;
    }
    if let Some(v) = req.allowed_tools {
        a.allowed_tools = v;
    }
    if let Some(v) = req.max_turns {
        a.max_turns = v;
    }
    if let Some(v) = req.connector_ids {
        a.connector_ids = v;
    }
    if let Some(v) = req.board_tools {
        a.board_tools = v;
    }
    state.db.save_agent(&a).await?;
    state.upsert(Entity::Agent(a.clone()));
    state
        .record(
            HistoryKind::AgentUpdated,
            Actor::User(me.id),
            Refs {
                agent: Some(a.id),
                ..Default::default()
            },
            format!("{} updated {}", me.name, a.name),
            None,
        )
        .await;
    Ok(Json(a))
}

pub async fn delete_agent(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::ManageAgents)?.clone();
    let _g = state.orch_lock.lock().await;
    let a = state
        .db
        .agent(id)
        .await?
        .ok_or(AppError::NotFound("agent"))?;
    if a.status != AgentStatus::Idle || a.current_run_id.is_some() {
        return Err(AppError::conflict(format!(
            "{} is working; cancel the run first",
            a.name
        )));
    }
    let tabs = state.db.clear_reviewer(id).await?;
    state.db.delete_agent(id).await?;
    for t in tabs {
        if let Some(t) = state.db.tab(t).await? {
            state.upsert(Entity::Tab(t));
        }
    }
    state.deleted(EntityKind::Agent, id);
    state
        .record(
            HistoryKind::AgentDeleted,
            Actor::User(me.id),
            Refs::default(),
            format!("{} said goodbye to {}", me.name, a.name),
            Some(json!({ "agent_id": id, "name": a.name })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- tickets

pub async fn create_ticket(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateTicket>,
) -> AppResult<Json<Ticket>> {
    let me = me.require(Permission::EditTickets)?.clone();
    let tab = state
        .db
        .tab(req.tab_id)
        .await?
        .ok_or(AppError::NotFound("tab"))?;
    let column = match req.column_id {
        Some(c) => tab
            .columns
            .iter()
            .find(|col| col.id == c)
            .ok_or_else(|| AppError::bad("column does not belong to this tab"))?,
        None => tab
            .columns
            .iter()
            .find(|c| c.kind == ColumnKind::Todo)
            .or_else(|| tab.columns.first())
            .ok_or_else(|| AppError::bad("this tab has no columns"))?,
    }
    .clone();
    if let Some(r) = req.repo_id {
        state.db.repo(r).await?.ok_or(AppError::NotFound("repo"))?;
    }
    let now = Utc::now();
    let _g = state.orch_lock.lock().await;
    let ticket = Ticket {
        id: new_id(),
        number: state.db.next_ticket_number().await?,
        tab_id: tab.id,
        column_id: column.id,
        position: state.db.max_position(column.id).await? + 1024.0,
        title: text("title", &req.title, 200, true)?,
        body: text(
            "description",
            req.body.as_deref().unwrap_or(""),
            MAX_BODY,
            false,
        )?,
        tags: clean_tags(req.tags.unwrap_or_default()),
        priority: req.priority.unwrap_or(Priority::Normal),
        color: req.color,
        repo_id: req.repo_id,
        agent_id: None,
        branch: None,
        pr_url: None,
        pr_number: None,
        review_feedback: None,
        review_rounds: 0,
        created_by: Some(me.id),
        created_at: now,
        updated_at: now,
        source: TicketSource::Manual,
    };
    state.db.save_ticket(&ticket).await?;
    state.upsert(Entity::Ticket(ticket.clone()));
    state
        .record(
            HistoryKind::TicketCreated,
            Actor::User(me.id),
            Refs::ticket(&ticket),
            format!("{} pinned {}: {}", me.name, tref(&ticket), ticket.title),
            None,
        )
        .await;
    if column.kind == ColumnKind::Review {
        orchestrator::maybe_auto_review_locked(&state, ticket.id).await;
    }
    Ok(Json(ticket))
}

fn clean_tags(tags: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t: String = t.trim().chars().take(32).collect();
        if !t.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
        if out.len() >= 12 {
            break;
        }
    }
    out
}

pub async fn update_ticket(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpdateTicket>,
) -> AppResult<Json<Ticket>> {
    let me = me.require(Permission::EditTickets)?.clone();
    let _g = state.orch_lock.lock().await;
    let mut t = state
        .db
        .ticket(id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    let mut changed = Vec::new();
    if let Some(v) = req.title {
        t.title = text("title", &v, 200, true)?;
        changed.push("title");
    }
    if let Some(v) = req.body {
        t.body = text("description", &v, MAX_BODY, false)?;
        changed.push("description");
    }
    if let Some(v) = req.tags {
        t.tags = clean_tags(v);
        changed.push("tags");
    }
    if let Some(v) = req.priority {
        t.priority = v;
        changed.push("priority");
    }
    if let Some(v) = req.color {
        t.color = v;
        changed.push("colour");
    }
    if let Some(v) = req.repo_id {
        if let Some(r) = v {
            state.db.repo(r).await?.ok_or(AppError::NotFound("repo"))?;
        }
        t.repo_id = v;
        changed.push("repository");
    }
    t.updated_at = Utc::now();
    state.db.save_ticket(&t).await?;
    state.upsert(Entity::Ticket(t.clone()));
    if !changed.is_empty() {
        state
            .record(
                HistoryKind::TicketUpdated,
                Actor::User(me.id),
                Refs::ticket(&t),
                format!(
                    "{} edited the {} of {}",
                    me.name,
                    changed.join(", "),
                    tref(&t)
                ),
                Some(json!({ "fields": changed })),
            )
            .await;
    }
    Ok(Json(t))
}

pub async fn move_ticket(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<MoveTicket>,
) -> AppResult<Json<Ticket>> {
    let me = me.require(Permission::EditTickets)?.clone();
    if !req.position.is_finite() {
        return Err(AppError::bad("position must be a number"));
    }
    let _g = state.orch_lock.lock().await;
    let mut t = state
        .db
        .ticket(id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    let tab = state
        .db
        .tab(t.tab_id)
        .await?
        .ok_or(AppError::NotFound("tab"))?;
    let to = tab
        .columns
        .iter()
        .find(|c| c.id == req.column_id)
        .ok_or_else(|| AppError::bad("tickets can only move between columns of their tab"))?;
    let from = tab
        .columns
        .iter()
        .find(|c| c.id == t.column_id)
        .map(|c| c.name.clone());
    let changed_column = t.column_id != to.id;
    t.column_id = to.id;
    t.position = req.position;
    t.updated_at = Utc::now();
    state.db.save_ticket(&t).await?;
    state.upsert(Entity::Ticket(t.clone()));
    if changed_column {
        state
            .record(
                HistoryKind::TicketMoved,
                Actor::User(me.id),
                Refs::ticket(&t),
                format!("{} moved {} to {}", me.name, tref(&t), to.name),
                Some(json!({ "from": from, "to": to.name, "to_column_id": to.id })),
            )
            .await;
        if to.kind == ColumnKind::Review {
            orchestrator::maybe_auto_review_locked(&state, t.id).await;
        }
    }
    Ok(Json(state.db.ticket(id).await?.unwrap_or(t)))
}

pub async fn assign(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<AssignAgent>,
) -> AppResult<Json<Run>> {
    let me = me.require(Permission::RunAgents)?.clone();
    let instructions = req
        .instructions
        .map(|s| text("instructions", &s, MAX_BODY, false))
        .transpose()?
        .filter(|s| !s.is_empty());
    let run = orchestrator::assign(
        &state,
        NewRun {
            ticket_id: id,
            agent_id: req.agent_id,
            kind: None,
            instructions,
            resume_session_id: None,
            trigger: Trigger::User(me.id),
        },
    )
    .await?;
    Ok(Json(run))
}

pub async fn comment(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<CommentRequest>,
) -> AppResult<Json<HistoryEvent>> {
    let me = me.require(Permission::EditTickets)?.clone();
    let t = state
        .db
        .ticket(id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    let body = text("comment", &req.text, 5_000, true)?;
    let ev = state
        .record(
            HistoryKind::Comment,
            Actor::User(me.id),
            Refs::ticket(&t),
            body.clone(),
            Some(json!({ "author": me.name })),
        )
        .await
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("could not save comment")))?;
    Ok(Json(ev))
}

pub async fn delete_ticket(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::EditTickets)?.clone();
    let _g = state.orch_lock.lock().await;
    let t = state
        .db
        .ticket(id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    orchestrator::abort_ticket_runs_locked(&state, id).await?;
    let runs = state.db.runs_for_ticket(id).await?;
    state.db.delete_ticket(id).await?;
    for r in runs {
        state.deleted(EntityKind::Run, r.id);
    }
    state.deleted(EntityKind::Ticket, id);
    state
        .record(
            HistoryKind::TicketDeleted,
            Actor::User(me.id),
            Refs {
                tab: Some(t.tab_id),
                ..Default::default()
            },
            format!("{} threw away {}: {}", me.name, tref(&t), t.title),
            Some(json!({ "ticket_id": id, "number": t.number })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- runs

pub async fn get_run(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<Run>> {
    me.require(Permission::View)?;
    Ok(Json(
        state.db.run(id).await?.ok_or(AppError::NotFound("run"))?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    after_seq: Option<u64>,
    limit: Option<u32>,
}

pub async fn run_logs(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Query(q): Query<LogQuery>,
) -> AppResult<Json<Vec<RunLogLine>>> {
    me.require(Permission::View)?;
    state.db.run(id).await?.ok_or(AppError::NotFound("run"))?;
    state.flush_logs().await;
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    Ok(Json(state.db.logs(id, q.after_seq, limit).await?))
}

pub async fn ticket_runs(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<Vec<Run>>> {
    me.require(Permission::View)?;
    state
        .db
        .ticket(id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    Ok(Json(state.db.runs_for_ticket(id).await?))
}

pub async fn cancel_run(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<Run>> {
    let me = me.require(Permission::RunAgents)?.clone();
    Ok(Json(orchestrator::cancel(&state, id, me.id).await?))
}

pub async fn message_run(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<RunMessage>,
) -> AppResult<Json<Run>> {
    let me = me.require(Permission::RunAgents)?.clone();
    let t = text("message", &req.text, MAX_BODY, true)?;
    Ok(Json(orchestrator::message(&state, id, t, me.id).await?))
}

pub async fn permission_run(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<PermissionAnswer>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::RunAgents)?.clone();
    orchestrator::answer_permission(&state, id, req.request_id, req.allow, me.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- skills

pub async fn get_skill(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<SkillDetail>> {
    me.require(Permission::View)?;
    let skill = state
        .db
        .skill(id)
        .await?
        .ok_or(AppError::NotFound("skill"))?;
    let dir = std::path::PathBuf::from(&skill.path);
    let (body, files) = tokio::task::spawn_blocking(move || {
        let body = std::fs::read_to_string(dir.join("SKILL.md"))
            .map(|t| skills::parse_skill_md(&t).2)
            .unwrap_or_default();
        (body, skills::list_files(&dir))
    })
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(SkillDetail { skill, body, files }))
}

fn validate_skill(req: &UpsertSkill) -> AppResult<(String, String, String)> {
    Ok((
        text("name", &req.name, 64, true)?,
        text("description", &req.description, 1024, true)?,
        text("skill body", &req.body, 100_000, false)?,
    ))
}

pub async fn create_skill(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<UpsertSkill>,
) -> AppResult<Json<Skill>> {
    let me = me.require(Permission::ManageSkills)?.clone();
    let (name, description, body) = validate_skill(&req)?;
    let dir = state.cfg.skills_dir();
    let slug = skills::slug(&name);
    if dir.join(&slug).exists() {
        return Err(AppError::conflict(format!(
            "a skill named \"{slug}\" already exists"
        )));
    }
    let path = skills::write_boba_skill(&dir, &name, &description, &body)?;
    let skill = state
        .db
        .upsert_skill(&Skill {
            id: new_id(),
            name,
            description,
            source: SkillSource::Boba,
            path: path.to_string_lossy().to_string(),
            editable: true,
        })
        .await?;
    state.upsert(Entity::Skill(skill.clone()));
    state
        .record(
            HistoryKind::SkillCreated,
            Actor::User(me.id),
            Refs::default(),
            format!("{} wrote the {} skill", me.name, skill.name),
            Some(json!({ "skill_id": skill.id })),
        )
        .await;
    Ok(Json(skill))
}

pub async fn update_skill(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpsertSkill>,
) -> AppResult<Json<Skill>> {
    let me = me.require(Permission::ManageSkills)?.clone();
    let (name, description, body) = validate_skill(&req)?;
    let mut skill = state
        .db
        .skill(id)
        .await?
        .ok_or(AppError::NotFound("skill"))?;
    if !skill.editable || skill.source != SkillSource::Boba {
        return Err(AppError::forbidden(
            "only skills created in Boba Agent can be edited",
        ));
    }
    // Keep the folder (agents reference it by path); only the content changes.
    let folder = std::path::PathBuf::from(&skill.path);
    std::fs::create_dir_all(&folder).map_err(|e| AppError::Internal(e.into()))?;
    std::fs::write(
        folder.join("SKILL.md"),
        skills::render_skill_md(&name, &description, &body),
    )
    .map_err(|e| AppError::Internal(e.into()))?;
    skill.name = name;
    skill.description = description;
    let skill = state.db.upsert_skill(&skill).await?;
    state.upsert(Entity::Skill(skill.clone()));
    state
        .record(
            HistoryKind::SkillUpdated,
            Actor::User(me.id),
            Refs::default(),
            format!("{} improved the {} skill", me.name, skill.name),
            Some(json!({ "skill_id": skill.id })),
        )
        .await;
    Ok(Json(skill))
}

pub async fn rescan_skills(
    State(state): State<SharedState>,
    me: Auth,
) -> AppResult<Json<Vec<Skill>>> {
    me.require(Permission::ManageSkills)?;
    let before: Vec<Id> = state.db.skills().await?.iter().map(|s| s.id).collect();
    let skills = skills::rescan(&state).await?;
    for id in before
        .iter()
        .filter(|id| !skills.iter().any(|s| s.id == **id))
    {
        state.deleted(EntityKind::Skill, *id);
    }
    for s in &skills {
        state.upsert(Entity::Skill(s.clone()));
    }
    // Agents that lost a skill changed too.
    for a in state.db.agents().await? {
        state.upsert(Entity::Agent(a));
    }
    Ok(Json(skills))
}

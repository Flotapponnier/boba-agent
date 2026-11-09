//! Jobs (the Quest Board): CRUD, "Run now", scheduled runs and what happens when they end.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use boba_core::api::{CreateJob, SchedulePreview, UpdateJob};
use boba_core::automation::{Job, JobOutput, Schedule};
use boba_core::event::{Entity, EntityKind, HistoryKind};
use boba_core::runner::RepoSpec;
use boba_core::team::NotificationKind;
use boba_core::*;
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::auth::Auth;
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::notify::Target;
use crate::orchestrator::{self, grant_xp};
use crate::scheduler;
use crate::state::{Actor, Refs, SharedState};

pub const XP_JOB: u32 = 25;
const MAX_PROMPT: usize = 20_000;

fn refs(job: &Job, agent: Option<Id>, run: Option<Id>) -> Refs {
    Refs {
        agent,
        run,
        tab: job.tab_id,
        ..Default::default()
    }
}

/// The repo a job works in. Pull-request jobs need one (unless simulated); others may run in
/// a scratch directory.
pub async fn job_repo(
    state: &SharedState,
    job: &Job,
    simulated: bool,
) -> AppResult<Option<RepoSpec>> {
    let repo = match job.repo_id {
        Some(id) => state.db.repo(id).await?,
        None => None,
    };
    match repo {
        Some(r) => Ok(Some(RepoSpec {
            path: r.path,
            default_branch: r.default_branch,
            remote_url: r.remote_url,
            ai_attribution: r.ai_attribution,
        })),
        None if job.output == JobOutput::PullRequest && !simulated => Err(AppError::bad(format!(
            "the quest \"{}\" opens pull requests: pick a repository for it",
            job.name
        ))),
        None => Ok(None),
    }
}

async fn validate(state: &SharedState, job: &Job) -> AppResult<()> {
    let name_len = job.name.chars().count();
    if job.name.trim().is_empty() || name_len > 80 {
        return Err(AppError::bad("quest names must be 1 to 80 characters"));
    }
    if job.prompt.trim().is_empty() || job.prompt.chars().count() > MAX_PROMPT {
        return Err(AppError::bad(format!(
            "the task must be 1 to {MAX_PROMPT} characters"
        )));
    }
    if job.description.chars().count() > 2_000 {
        return Err(AppError::bad("the description is too long"));
    }
    scheduler::validate(&job.schedule).map_err(AppError::bad)?;
    state
        .db
        .agent(job.agent_id)
        .await?
        .ok_or_else(|| AppError::bad("that agent does not exist"))?;
    if let Some(r) = job.repo_id {
        state.db.repo(r).await?.ok_or(AppError::NotFound("repo"))?;
    }
    if let Some(t) = job.tab_id {
        state.db.tab(t).await?.ok_or(AppError::NotFound("tab"))?;
    }
    if job.output == JobOutput::PullRequest && job.repo_id.is_none() && !state.cfg.demo_mode {
        return Err(AppError::bad("pull-request quests need a repository"));
    }
    Ok(())
}

fn next_fire(schedule: &Schedule, enabled: bool, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if !enabled {
        return None;
    }
    scheduler::next_after(schedule, from).ok().flatten()
}

// ---------------------------------------------------------------- routes

pub async fn create(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateJob>,
) -> AppResult<Json<Job>> {
    let me = me.require(Permission::ManageAutomations)?.clone();
    let enabled = req.enabled.unwrap_or(true);
    let now = Utc::now();
    let mut job = Job {
        id: new_id(),
        name: req.name.trim().to_string(),
        description: req.description.unwrap_or_default().trim().to_string(),
        icon: req
            .icon
            .filter(|i| !i.trim().is_empty())
            .unwrap_or_else(|| "📜".into()),
        agent_id: req.agent_id,
        prompt: req.prompt.trim().to_string(),
        schedule: req.schedule,
        output: req.output,
        enabled,
        repo_id: req.repo_id,
        tab_id: req.tab_id,
        skip_if_running: req.skip_if_running.unwrap_or(true),
        last_run_id: None,
        last_run_at: None,
        next_run_at: None,
        last_result: None,
        runs_count: 0,
        created_by: Some(me.id),
        created_at: now,
    };
    validate(&state, &job).await?;
    job.next_run_at = next_fire(&job.schedule, enabled, now);
    state.db.save_job(&job).await?;
    state.upsert(Entity::Job(job.clone()));
    state
        .record(
            HistoryKind::JobCreated,
            Actor::User(me.id),
            refs(&job, Some(job.agent_id), None),
            format!(
                "{} posted the quest \"{}\" ({})",
                me.name,
                job.name,
                scheduler::describe(&job.schedule)
            ),
            Some(json!({ "job_id": job.id })),
        )
        .await;
    Ok(Json(job))
}

pub async fn update(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpdateJob>,
) -> AppResult<Json<Job>> {
    let me = me.require(Permission::ManageAutomations)?.clone();
    let mut job = state.db.job(id).await?.ok_or(AppError::NotFound("job"))?;
    let mut reschedule = false;
    if let Some(v) = req.name {
        job.name = v.trim().to_string();
    }
    if let Some(v) = req.description {
        job.description = v.trim().to_string();
    }
    if let Some(v) = req.icon {
        job.icon = v;
    }
    if let Some(v) = req.agent_id {
        job.agent_id = v;
    }
    if let Some(v) = req.prompt {
        job.prompt = v.trim().to_string();
    }
    if let Some(v) = req.schedule {
        job.schedule = v;
        reschedule = true;
    }
    if let Some(v) = req.output {
        job.output = v;
    }
    if let Some(v) = req.enabled {
        reschedule |= v != job.enabled;
        job.enabled = v;
    }
    if let Some(v) = req.repo_id {
        job.repo_id = v;
    }
    if let Some(v) = req.tab_id {
        job.tab_id = v;
    }
    if let Some(v) = req.skip_if_running {
        job.skip_if_running = v;
    }
    validate(&state, &job).await?;
    if reschedule {
        job.next_run_at = next_fire(&job.schedule, job.enabled, Utc::now());
    }
    state.db.save_job(&job).await?;
    state.upsert(Entity::Job(job.clone()));
    state
        .record(
            HistoryKind::JobUpdated,
            Actor::User(me.id),
            refs(&job, Some(job.agent_id), None),
            format!("{} updated the quest \"{}\"", me.name, job.name),
            Some(json!({ "job_id": job.id })),
        )
        .await;
    Ok(Json(job))
}

pub async fn delete(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::ManageAutomations)?.clone();
    let job = state.db.job(id).await?.ok_or(AppError::NotFound("job"))?;
    if state
        .db
        .job_runs(id)
        .await?
        .iter()
        .any(|r| !r.status.is_terminal())
    {
        return Err(AppError::conflict(
            "this quest is running; cancel its run first",
        ));
    }
    state.db.delete_job(id).await?;
    state.deleted(EntityKind::Job, id);
    state
        .record(
            HistoryKind::JobDeleted,
            Actor::User(me.id),
            refs(&job, None, None),
            format!("{} took down the quest \"{}\"", me.name, job.name),
            Some(json!({ "job_id": id })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn run_now(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<Run>> {
    let me = me.require(Permission::RunAgents)?.clone();
    let _g = state.orch_lock.lock().await;
    let job = state.db.job(id).await?.ok_or(AppError::NotFound("job"))?;
    Ok(Json(start_job_run_locked(&state, &job, Some(me.id)).await?))
}

pub async fn runs(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<Vec<Run>>> {
    me.require(Permission::View)?;
    state.db.job(id).await?.ok_or(AppError::NotFound("job"))?;
    Ok(Json(state.db.job_runs(id).await?))
}

pub async fn preview(me: Auth, Json(s): Json<Schedule>) -> AppResult<Json<SchedulePreview>> {
    me.require(Permission::View)?;
    Ok(Json(scheduler::preview(&s, Utc::now())))
}

// ---------------------------------------------------------------- runs

/// Start one run of `job`. `by` = the human who pressed Run now (scheduled runs: None, and
/// the job's creator is the human responsible for routing).
pub async fn start_job_run_locked(
    state: &SharedState,
    job: &Job,
    by: Option<Id>,
) -> AppResult<Run> {
    let db = &state.db;
    if job.skip_if_running
        && db
            .job_runs(job.id)
            .await?
            .iter()
            .any(|r| !r.status.is_terminal())
    {
        return Err(AppError::conflict(format!(
            "\"{}\" is already running",
            job.name
        )));
    }
    let mut agent = db
        .agent(job.agent_id)
        .await?
        .ok_or(AppError::NotFound("agent"))?;
    if agent.status != AgentStatus::Idle {
        return Err(AppError::conflict(format!(
            "{} is busy right now",
            agent.name
        )));
    }
    let responsible = by.or(job.created_by);
    let slot = state.runners.route(db, responsible).await.ok_or_else(|| {
        AppError::conflict(
            "no runner is available for you: connect your own Claude Code to run agents",
        )
    })?;
    let simulated = slot.simulated();
    job_repo(state, job, simulated).await?;

    let now = Utc::now();
    let run = Run {
        id: new_id(),
        ticket_id: None,
        job_id: Some(job.id),
        agent_id: agent.id,
        kind: RunKind::Job,
        status: RunStatus::Queued,
        runner_id: Some(slot.id()),
        branch: None,
        worktree_path: None,
        session_id: None,
        progress: 0.0,
        activity: Some("Waiting for a free runner".into()),
        summary: None,
        verdict: None,
        cost_usd: 0.0,
        input_tokens: 0,
        output_tokens: 0,
        started_by: by,
        created_at: now,
        started_at: None,
        ended_at: None,
        error: None,
    };
    db.insert_run(&run, None, None, None).await?;
    state.upsert(Entity::Run(run.clone()));
    agent.status = orchestrator::status_for(RunKind::Job);
    agent.current_run_id = Some(run.id);
    orchestrator::save_agent(state, &agent).await?;

    let mut job = job.clone();
    job.last_run_id = Some(run.id);
    job.last_run_at = Some(now);
    job.runs_count += 1;
    db.save_job(&job).await?;
    state.upsert(Entity::Job(job.clone()));

    let actor = by.map(Actor::User).unwrap_or(Actor::Agent(agent.id));
    state
        .record(
            HistoryKind::JobRunStarted,
            actor,
            refs(&job, Some(agent.id), Some(run.id)),
            format!("{} set off on the quest \"{}\"", agent.name, job.name),
            Some(json!({ "job_id": job.id })),
        )
        .await;
    state.init_seq(run.id).await?;
    if simulated && state.runners.any_real() {
        state
            .server_log(
                run.id,
                LogKind::System,
                "Simulated: this server's Claude Code only runs its owner's work. Connect your own Claude Code to run this for real.",
                Some(json!({ "simulated": true })),
            )
            .await;
    }
    orchestrator::dispatch_queued_locked(state).await;
    Ok(db.run(run.id).await?.unwrap_or(run))
}

/// Called by the orchestrator when a job run ends (lock held).
pub async fn on_job_run_finished_locked(
    state: &SharedState,
    run: &Run,
    agent: &mut Agent,
) -> anyhow::Result<()> {
    let Some(job_id) = run.job_id else {
        return Ok(());
    };
    let Some(mut job) = state.db.job(job_id).await? else {
        return Ok(());
    };
    let ok = run.status == RunStatus::Succeeded;
    if ok {
        if let Some(summary) = run.summary.clone().filter(|s| !s.trim().is_empty()) {
            job.last_result = Some(summary);
        }
    }
    state.db.save_job(&job).await?;
    state.upsert(Entity::Job(job.clone()));
    let r = refs(&job, Some(agent.id), Some(run.id));
    let message = match run.status {
        RunStatus::Succeeded => format!(
            "{} completed the quest \"{}\" (+{XP_JOB} XP)",
            agent.name, job.name
        ),
        RunStatus::Cancelled => format!("{}'s quest \"{}\" was cancelled", agent.name, job.name),
        _ => format!(
            "{}'s quest \"{}\" failed: {}",
            agent.name,
            job.name,
            run.error.clone().unwrap_or_else(|| "unknown error".into())
        ),
    };
    state
        .record(
            HistoryKind::JobRunFinished,
            Actor::Agent(agent.id),
            r,
            message.clone(),
            Some(json!({ "job_id": job.id, "status": run.status, "cost_usd": run.cost_usd })),
        )
        .await;
    if ok {
        let ups = grant_xp(agent, XP_JOB);
        orchestrator::xp_effects(state, agent, XP_JOB, &ups, r).await;
    }
    let mut users: Vec<Id> = job.created_by.into_iter().collect();
    users.extend(run.started_by);
    state
        .notify(
            users,
            NotificationKind::JobResult,
            &message,
            Target {
                run: Some(run.id),
                job: Some(job.id),
                actor_agent: Some(agent.id),
                ..Default::default()
            },
        )
        .await;
    Ok(())
}

/// Scheduler tick: start every due job, then move its next fire time forward.
/// Jobs whose agent is busy stay due and are retried next tick.
pub async fn run_due(state: &SharedState, now: DateTime<Utc>) -> anyhow::Result<()> {
    let due = state.db.due_jobs(now).await?;
    if due.is_empty() {
        return Ok(());
    }
    let _g = state.orch_lock.lock().await;
    for job in due {
        let res = start_job_run_locked(state, &job, None).await;
        let advance = match &res {
            Ok(_) => true,
            // The previous run is still going: skip this occurrence.
            Err(AppError::Conflict(m)) if m.contains("already running") => true,
            // Agent busy / no runner right now: try again on the next tick.
            Err(AppError::Conflict(_)) => false,
            // Misconfigured (repo gone…): skip, and say why in the logs.
            Err(e) => {
                tracing::warn!(job = %job.name, error = %e, "scheduled quest could not start");
                true
            }
        };
        if advance {
            if let Some(mut j) = state.db.job(job.id).await? {
                // From now, not from the missed time: a server that was down fires once.
                j.next_run_at = next_fire(&j.schedule, j.enabled, now);
                state.db.save_job(&j).await?;
                state.upsert(Entity::Job(j));
            }
        }
    }
    Ok(())
}

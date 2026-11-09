//! The run state machine: assign → dispatch → consume the runner's update stream →
//! finish (XP, routing, auto review, queue draining).
//!
//! Every transition that touches more than one entity runs under `AppState::orch_lock`.
//! Functions suffixed `_locked` expect the caller to hold it.

use std::time::Duration;

use boba_core::event::{Entity, Fx, HistoryKind};
use boba_core::runner::{AgentSpec, RepoSpec, RunOutcome, RunSpec, RunUpdate, TicketSpec};
use boba_core::*;
use chrono::Utc;
use futures::future::BoxFuture;
use serde_json::json;
use tokio::sync::mpsc;

use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::state::{Actor, Refs, SharedState};

pub const XP_WORK: u32 = 40;
pub const XP_WORK_SKILL_MATCH: u32 = 60;
pub const XP_REVIEW: u32 = 30;
pub const XP_FIRST_ROUND_BONUS: u32 = 50;
const PROGRESS_FLUSH: Duration = Duration::from_millis(300);
const CANCEL_GRACE: Duration = Duration::from_secs(15);

/// Add XP, level up as many times as needed. Returns the levels reached.
pub fn grant_xp(agent: &mut Agent, amount: u32) -> Vec<u32> {
    let mut ups = Vec::new();
    agent.level = agent.level.max(1);
    agent.xp += amount;
    while agent.xp >= xp_for_next_level(agent.level) {
        agent.xp -= xp_for_next_level(agent.level);
        agent.level += 1;
        ups.push(agent.level);
    }
    ups
}

fn first_of(tab: &Tab, kind: ColumnKind) -> Option<&Column> {
    tab.columns.iter().find(|c| c.kind == kind)
}

fn column_named(tab: &Tab, id: Option<Id>) -> Option<&Column> {
    id.and_then(|id| tab.columns.iter().find(|c| c.id == id))
}

fn status_for(kind: RunKind) -> AgentStatus {
    match kind {
        RunKind::Work => AgentStatus::Working,
        RunKind::Review => AgentStatus::Reviewing,
        RunKind::Job | RunKind::Command => AgentStatus::Working,
    }
}

/// Tools a read-only run (review, command) may use, whatever the agent is configured with.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(gh pr diff:*)",
    "Bash(gh pr view:*)",
];

/// The read-only allowlist plus the agent's MCP tools whose name says they only read.
pub fn read_only_tools(agent_tools: &[String]) -> Vec<String> {
    const READ_VERBS: &[&str] = &[
        "get", "list", "search", "read", "view", "fetch", "find", "query",
    ];
    let mut out: Vec<String> = READ_ONLY_TOOLS.iter().map(|s| s.to_string()).collect();
    for t in agent_tools {
        let Some(rest) = t.strip_prefix("mcp__") else {
            continue;
        };
        let tool = rest.rsplit("__").next().unwrap_or("").to_ascii_lowercase();
        let reads = READ_VERBS
            .iter()
            .any(|v| tool == *v || tool.starts_with(&format!("{v}_")) || tool.starts_with(v));
        if reads && !t.contains('*') && !out.contains(t) {
            out.push(t.clone());
        }
    }
    out
}

async fn run_ticket(db: &crate::db::Db, run: &Run) -> anyhow::Result<Option<Ticket>> {
    match run.ticket_id {
        Some(id) => db.ticket(id).await,
        None => Ok(None),
    }
}

/// `T-12`
pub fn tref(t: &Ticket) -> String {
    format!("T-{}", t.number)
}

fn pr_ref(t: &Ticket) -> String {
    match t.pr_number {
        Some(n) => format!("PR #{n}"),
        None => "the PR".into(),
    }
}

// ---------------------------------------------------------------- broadcast helpers

async fn save_ticket(state: &SharedState, t: &Ticket) -> anyhow::Result<()> {
    state.db.save_ticket(t).await?;
    state.upsert(Entity::Ticket(t.clone()));
    Ok(())
}

async fn save_agent(state: &SharedState, a: &Agent) -> anyhow::Result<()> {
    state.db.save_agent(a).await?;
    state.upsert(Entity::Agent(a.clone()));
    Ok(())
}

async fn save_run(state: &SharedState, r: &Run) -> anyhow::Result<()> {
    state.db.save_run(r).await?;
    state.upsert(Entity::Run(r.clone()));
    Ok(())
}

async fn broadcast_run(state: &SharedState, run_id: Id) {
    if let Ok(Some(r)) = state.db.run(run_id).await {
        state.upsert(Entity::Run(r));
    }
}

async fn broadcast_runner(state: &SharedState) {
    if let Ok(info) = state.runner_info().await {
        state.upsert(Entity::Runner(info));
    }
}

/// Move a ticket to the bottom of `column`.
async fn place(state: &SharedState, t: &mut Ticket, column: &Column) -> anyhow::Result<()> {
    if t.column_id != column.id {
        t.position = state.db.max_position(column.id).await? + 1024.0;
        t.column_id = column.id;
    }
    t.updated_at = Utc::now();
    Ok(())
}

async fn xp_effects(state: &SharedState, agent: &Agent, amount: u32, ups: &[u32], refs: Refs) {
    state.fx(Fx::XpGained {
        agent_id: agent.id,
        amount,
    });
    for &level in ups {
        state.fx(Fx::LevelUp {
            agent_id: agent.id,
            level,
        });
        state
            .record(
                HistoryKind::AgentLevelUp,
                Actor::Agent(agent.id),
                Refs {
                    agent: Some(agent.id),
                    ..refs
                },
                format!("{} reached level {level}!", agent.name),
                Some(json!({ "level": level })),
            )
            .await;
    }
}

// ---------------------------------------------------------------- assign / create

/// Who asked for a run.
#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    User(Id),
    /// Auto review when a ticket enters a review column.
    Auto,
}

pub struct NewRun {
    pub ticket_id: Id,
    pub agent_id: Id,
    pub kind: Option<RunKind>,
    pub instructions: Option<String>,
    pub resume_session_id: Option<String>,
    pub trigger: Trigger,
}

/// `POST /tickets/:id/assign`: drag an agent onto a ticket.
pub async fn assign(state: &SharedState, req: NewRun) -> AppResult<Run> {
    let _g = state.orch_lock.lock().await;
    create_run_locked(state, req).await
}

pub async fn create_run_locked(state: &SharedState, req: NewRun) -> AppResult<Run> {
    let db = &state.db;
    let mut ticket = db
        .ticket(req.ticket_id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    let mut agent = db
        .agent(req.agent_id)
        .await?
        .ok_or(AppError::NotFound("agent"))?;
    let tab = db
        .tab(ticket.tab_id)
        .await?
        .ok_or(AppError::NotFound("tab"))?;

    if db
        .runs_for_ticket(ticket.id)
        .await?
        .iter()
        .any(|r| !r.status.is_terminal())
    {
        return Err(AppError::conflict(format!(
            "{} already has a run in progress",
            tref(&ticket)
        )));
    }
    if agent.status != AgentStatus::Idle {
        return Err(AppError::conflict(format!(
            "{} is busy right now",
            agent.name
        )));
    }

    let in_review = column_named(&tab, Some(ticket.column_id))
        .map(|c| c.kind == ColumnKind::Review)
        .unwrap_or(false);
    let kind = req.kind.unwrap_or(if in_review {
        RunKind::Review
    } else {
        RunKind::Work
    });
    if kind == RunKind::Review
        && ticket.pr_number.is_none()
        && ticket.branch.is_none()
        && !state.cfg.demo_mode
    {
        return Err(AppError::bad(format!(
            "{} has no branch or pull request to review yet",
            tref(&ticket)
        )));
    }
    // Validate that a repo exists before spending anything.
    resolve_repo(state, &ticket, &tab).await?;

    let now = Utc::now();
    let started_by = match req.trigger {
        Trigger::User(u) => Some(u),
        Trigger::Auto => None,
    };
    let resume = match (&req.resume_session_id, kind) {
        (Some(s), _) => Some(s.clone()),
        (None, RunKind::Work) if ticket.review_rounds > 0 || ticket.branch.is_some() => {
            db.last_work_session(ticket.id).await?
        }
        _ => None,
    };
    let run = Run {
        id: new_id(),
        ticket_id: Some(ticket.id),
        job_id: None,
        agent_id: agent.id,
        kind,
        status: RunStatus::Queued,
        runner_id: Some(state.runner.id()),
        branch: ticket.branch.clone(),
        worktree_path: None,
        session_id: None,
        progress: 0.0,
        activity: Some("Waiting for a free runner".into()),
        summary: None,
        verdict: None,
        cost_usd: 0.0,
        input_tokens: 0,
        output_tokens: 0,
        started_by,
        created_at: now,
        started_at: None,
        ended_at: None,
        error: None,
    };
    db.insert_run(&run, req.instructions.as_deref(), resume.as_deref())
        .await?;
    state.upsert(Entity::Run(run.clone()));

    if kind == RunKind::Work {
        ticket.agent_id = Some(agent.id);
        if let Some(doing) = first_of(&tab, ColumnKind::Doing) {
            place(state, &mut ticket, doing).await?;
        }
        ticket.updated_at = now;
        save_ticket(state, &ticket).await?;
    }
    agent.status = status_for(kind);
    agent.current_run_id = Some(run.id);
    save_agent(state, &agent).await?;

    let actor = match req.trigger {
        Trigger::User(u) => Actor::User(u),
        Trigger::Auto => Actor::Agent(agent.id),
    };
    let message = match (kind, req.trigger) {
        (RunKind::Work | RunKind::Job | RunKind::Command, _) => format!(
            "{} started working on {}: {}",
            agent.name,
            tref(&ticket),
            ticket.title
        ),
        (RunKind::Review, Trigger::Auto) => format!(
            "{} picked up {} of {} for review",
            agent.name,
            pr_ref(&ticket),
            tref(&ticket)
        ),
        (RunKind::Review, Trigger::User(_)) => format!(
            "{} started reviewing {} of {}",
            agent.name,
            pr_ref(&ticket),
            tref(&ticket)
        ),
    };
    state
        .record(
            HistoryKind::RunStarted,
            actor,
            Refs::ticket(&ticket).agent(agent.id).run(run.id),
            message,
            Some(json!({ "kind": kind })),
        )
        .await;

    state.init_seq(run.id).await?;
    if let Some(text) = &req.instructions {
        state
            .server_log(
                run.id,
                LogKind::User,
                text.clone(),
                started_by.map(|u| json!({ "user_id": u })),
            )
            .await;
    }

    dispatch_queued_locked(state).await;
    Ok(db.run(run.id).await?.unwrap_or(run))
}

/// The repo a ticket run works in. `None` only in demo mode (simulated runs need no checkout).
async fn resolve_repo(
    state: &SharedState,
    ticket: &Ticket,
    tab: &Tab,
) -> AppResult<Option<RepoSpec>> {
    let repo_id = ticket.repo_id.or(tab.repo_id);
    let repo = match repo_id {
        Some(id) => state.db.repo(id).await?,
        None => None,
    };
    match repo {
        Some(r) => Ok(Some(RepoSpec {
            path: r.path,
            default_branch: r.default_branch,
            remote_url: r.remote_url,
        })),
        None if state.cfg.demo_mode => Ok(None),
        None => Err(AppError::bad(format!(
            "{} has no repository: pick one on the ticket or on the \"{}\" tab",
            tref(ticket),
            tab.name
        ))),
    }
}

async fn build_spec(state: &SharedState, run: &Run) -> AppResult<RunSpec> {
    let db = &state.db;
    let ticket = run_ticket(db, run)
        .await?
        .ok_or_else(|| AppError::bad("this run has no ticket"))?;
    let agent = db
        .agent(run.agent_id)
        .await?
        .ok_or(AppError::NotFound("agent"))?;
    let tab = db
        .tab(ticket.tab_id)
        .await?
        .ok_or(AppError::NotFound("tab"))?;
    let repo = resolve_repo(state, &ticket, &tab).await?;
    let skills = db.skills().await?;
    let skill_paths = agent
        .skill_ids
        .iter()
        .filter_map(|id| skills.iter().find(|s| s.id == *id).map(|s| s.path.clone()))
        .collect();
    let (extra, resume) = db.run_inputs(run.id).await?;
    // Reviews and commands are read-only, whatever the agent is configured with:
    // plan mode and a server-enforced read-only tool allowlist.
    let read_only = matches!(run.kind, RunKind::Review | RunKind::Command);
    let (permission_mode, allowed_tools) = if read_only {
        (PermissionMode::Plan, read_only_tools(&agent.allowed_tools))
    } else {
        (agent.permission_mode, agent.allowed_tools.clone())
    };
    Ok(RunSpec {
        run_id: run.id,
        kind: run.kind,
        agent: AgentSpec {
            id: agent.id,
            name: agent.name,
            role: agent.role,
            system_prompt: agent.system_prompt,
            model: agent.model,
            permission_mode,
            allowed_tools,
            max_turns: agent.max_turns,
        },
        ticket: Some(TicketSpec {
            id: ticket.id,
            number: ticket.number,
            title: ticket.title,
            body: ticket.body,
            tags: ticket.tags,
            branch: ticket.branch,
            pr_number: ticket.pr_number,
            pr_url: ticket.pr_url,
            review_feedback: ticket.review_feedback,
            review_rounds: ticket.review_rounds,
            source: ticket.source.clone(),
        }),
        job: None,
        command: None,
        repo,
        // Connectors and board tools are wired in follow-up PRs.
        mcp_servers: Vec::new(),
        board_tools: None,
        skill_paths,
        resume_session_id: resume,
        extra_instructions: extra,
    })
}

/// Hand every not-yet-dispatched queued run to the runner (oldest first).
pub async fn dispatch_queued_locked(state: &SharedState) {
    let queued = match state.db.active_runs().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = ?e, "listing queued runs");
            return;
        }
    };
    for run in queued.into_iter().filter(|r| r.status == RunStatus::Queued) {
        // Runners queue internally past their capacity and send `Started` when a slot
        // frees up, so every queued run is handed over right away and stays Queued.
        let handed = state
            .live
            .lock()
            .map(|l| l.contains(&run.id))
            .unwrap_or(true);
        if !handed {
            dispatch_boxed(state, run).await;
        }
    }
    broadcast_runner(state).await;
}

fn dispatch_boxed(state: &SharedState, run: Run) -> BoxFuture<'_, ()> {
    Box::pin(dispatch_locked(state, run))
}

async fn dispatch_locked(state: &SharedState, run: Run) {
    let spec = match build_spec(state, &run).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            fail_now_locked(state, run.id, msg).await;
            return;
        }
    };
    if let Ok(mut live) = state.live.lock() {
        live.insert(run.id);
    }
    state.hub.open_run(run.id);
    let (tx, rx) = mpsc::channel::<RunUpdate>(512);
    let s2 = state.clone();
    let run_id = run.id;
    tokio::spawn(consume_boxed(s2, run_id, rx));
    if let Err(e) = state.runner.start(spec, tx).await {
        tracing::warn!(error = ?e, %run_id, "runner refused to start the run");
        fail_now_locked(state, run_id, format!("runner could not start: {e}")).await;
    }
}

async fn fail_now_locked(state: &SharedState, run_id: Id, error: String) {
    let outcome = RunOutcome {
        status: RunStatus::Failed,
        summary: None,
        verdict: None,
        feedback: None,
        error: Some(error),
        plan: None,
    };
    if let Err(e) = finish_locked(state, run_id, outcome).await {
        tracing::error!(error = ?e, %run_id, "finishing failed run");
    }
}

// ---------------------------------------------------------------- update stream

fn consume_boxed(
    state: SharedState,
    run_id: Id,
    rx: mpsc::Receiver<RunUpdate>,
) -> BoxFuture<'static, ()> {
    Box::pin(consume(state, run_id, rx))
}

async fn consume(state: SharedState, run_id: Id, mut rx: mpsc::Receiver<RunUpdate>) {
    let mut pending_progress: Option<(f32, String)> = None;
    let mut pending_usage: Option<(f64, u64, u64)> = None;
    let mut tick = tokio::time::interval(PROGRESS_FLUSH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            upd = rx.recv() => {
                let Some(upd) = upd else {
                    flush_live(&state, run_id, &mut pending_progress, &mut pending_usage).await;
                    let _g = state.orch_lock.lock().await;
                    if !is_finished(&state, run_id).await {
                        fail_now_locked(&state, run_id, "runner stopped unexpectedly".into()).await;
                    }
                    return;
                };
                match upd {
                    RunUpdate::Log { mut line } => {
                        line.run_id = run_id;
                        state.append_log(line).await;
                    }
                    RunUpdate::Progress { progress, activity } => {
                        pending_progress = Some((progress.clamp(0.0, 1.0), activity));
                    }
                    RunUpdate::Usage { cost_usd, input_tokens, output_tokens } => {
                        pending_usage = Some((cost_usd, input_tokens, output_tokens));
                    }
                    RunUpdate::Finished { outcome } => {
                        flush_live(&state, run_id, &mut pending_progress, &mut pending_usage).await;
                        let _g = state.orch_lock.lock().await;
                        if let Err(e) = finish_locked(&state, run_id, outcome).await {
                            tracing::error!(error = ?e, %run_id, "finishing run");
                        }
                        return;
                    }
                    other => {
                        flush_live(&state, run_id, &mut pending_progress, &mut pending_usage).await;
                        let _g = state.orch_lock.lock().await;
                        if let Err(e) = on_event_locked(&state, run_id, other).await {
                            tracing::error!(error = ?e, %run_id, "handling run update");
                        }
                    }
                }
            }
            _ = tick.tick() => {
                flush_live(&state, run_id, &mut pending_progress, &mut pending_usage).await;
            }
        }
    }
}

async fn is_finished(state: &SharedState, run_id: Id) -> bool {
    match state.db.run(run_id).await {
        Ok(Some(r)) => r.status.is_terminal(),
        _ => true,
    }
}

async fn flush_live(
    state: &SharedState,
    run_id: Id,
    progress: &mut Option<(f32, String)>,
    usage: &mut Option<(f64, u64, u64)>,
) {
    if progress.is_none() && usage.is_none() {
        return;
    }
    if let Err(e) = state
        .db
        .set_run_live_fields(run_id, progress.take(), usage.take())
        .await
    {
        tracing::warn!(error = ?e, "saving run progress");
    }
    broadcast_run(state, run_id).await;
}

async fn on_event_locked(state: &SharedState, run_id: Id, upd: RunUpdate) -> anyhow::Result<()> {
    let db = &state.db;
    let Some(mut run) = db.run(run_id).await? else {
        return Ok(());
    };
    if run.status.is_terminal() {
        return Ok(());
    }
    match upd {
        RunUpdate::Started {
            worktree_path,
            branch,
            session_id,
        } => {
            run.status = RunStatus::Running;
            run.started_at.get_or_insert(Utc::now());
            run.worktree_path = Some(worktree_path);
            if !branch.is_empty() {
                run.branch = Some(branch.clone());
            }
            if session_id.is_some() {
                run.session_id = session_id;
            }
            run.activity = Some(match run.kind {
                RunKind::Review => "Reading the diff".into(),
                RunKind::Command => "Thinking".into(),
                RunKind::Work | RunKind::Job => "Getting started".into(),
            });
            save_run(state, &run).await?;
            if run.kind == RunKind::Work && !branch.is_empty() {
                if let Some(mut t) = run_ticket(db, &run).await? {
                    if t.branch.as_deref() != Some(branch.as_str()) {
                        t.branch = Some(branch.clone());
                        t.updated_at = Utc::now();
                        save_ticket(state, &t).await?;
                    }
                }
            }
            broadcast_runner(state).await;
        }
        RunUpdate::PermissionRequest {
            request_id,
            tool,
            input,
        } => {
            run.status = RunStatus::Waiting;
            run.activity = Some(format!("Waiting for permission: {tool}"));
            save_run(state, &run).await?;
            if let Some(mut a) = db.agent(run.agent_id).await? {
                a.status = AgentStatus::Waiting;
                save_agent(state, &a).await?;
                let refs = match run_ticket(db, &run).await? {
                    Some(t) => Refs::ticket(&t),
                    None => Refs::default(),
                }
                .agent(a.id)
                .run(run.id);
                state
                    .record(
                        HistoryKind::RunWaiting,
                        Actor::Agent(a.id),
                        refs,
                        format!("{} needs permission to use {tool}", a.name),
                        Some(json!({ "request_id": request_id, "tool": tool })),
                    )
                    .await;
            }
            state
                .server_log(
                    run.id,
                    LogKind::System,
                    format!("Permission needed: {tool} {}", short_json(&input)),
                    Some(json!({ "permission_request": { "request_id": request_id, "tool": tool, "input": input } })),
                )
                .await;
        }
        RunUpdate::PrOpened { number, url } => {
            if let Some(mut t) = run_ticket(db, &run).await? {
                let new_pr = t.pr_number != Some(number);
                t.pr_number = Some(number);
                t.pr_url = Some(url.clone());
                t.updated_at = Utc::now();
                save_ticket(state, &t).await?;
                if new_pr {
                    let name = db
                        .agent(run.agent_id)
                        .await?
                        .map(|a| a.name)
                        .unwrap_or_default();
                    state
                        .record(
                            HistoryKind::PrOpened,
                            Actor::Agent(run.agent_id),
                            Refs::ticket(&t).agent(run.agent_id).run(run.id),
                            format!("{name} opened PR #{number} for {}", tref(&t)),
                            Some(json!({ "number": number, "url": url })),
                        )
                        .await;
                }
            }
        }
        // Log / Progress / Usage / Finished are handled by the caller.
        _ => {}
    }
    Ok(())
}

fn short_json(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::Object(m) => m
            .get("command")
            .or_else(|| m.get("file_path"))
            .or_else(|| m.get("url"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string()),
        other => other.to_string(),
    };
    let mut out: String = s.chars().take(160).collect();
    if s.chars().count() > 160 {
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------- finish

pub async fn finish_locked(
    state: &SharedState,
    run_id: Id,
    outcome: RunOutcome,
) -> anyhow::Result<()> {
    finish_inner(state, run_id, outcome, true).await
}

/// `follow_up = false` (restart recovery) records the outcome but starts nothing new.
async fn finish_inner(
    state: &SharedState,
    run_id: Id,
    outcome: RunOutcome,
    follow_up: bool,
) -> anyhow::Result<()> {
    let db = &state.db;
    let Some(mut run) = db.run(run_id).await? else {
        cleanup_live(state, run_id).await;
        return Ok(());
    };
    if run.status.is_terminal() {
        cleanup_live(state, run_id).await;
        return Ok(());
    }

    let mut status = if outcome.status.is_terminal() {
        outcome.status
    } else {
        RunStatus::Failed
    };
    let mut error = outcome.error.clone();
    if run.kind == RunKind::Review && status == RunStatus::Succeeded && outcome.verdict.is_none() {
        status = RunStatus::Failed;
        error = Some("the reviewer finished without a verdict".into());
    }
    run.status = status;
    run.summary = outcome.summary.clone().or(run.summary);
    run.verdict = if status == RunStatus::Succeeded {
        outcome.verdict
    } else {
        None
    };
    run.error = if status == RunStatus::Succeeded {
        None
    } else {
        error.clone()
    };
    run.ended_at = Some(Utc::now());
    if status == RunStatus::Succeeded {
        run.progress = 1.0;
    }
    run.activity = Some(
        match status {
            RunStatus::Succeeded => "Done",
            RunStatus::Cancelled => "Cancelled",
            _ => "Failed",
        }
        .into(),
    );
    save_run(state, &run).await?;
    cleanup_live(state, run_id).await;

    let cost = run.cost_usd;
    state
        .update_stats(|s| {
            s.total_cost_usd += cost;
            if status == RunStatus::Succeeded {
                s.runs_completed += 1;
            }
        })
        .await?;

    let mut agent = db.agent(run.agent_id).await?;
    if let Some(a) = agent.as_mut() {
        if a.current_run_id == Some(run.id) || a.current_run_id.is_none() {
            a.status = AgentStatus::Idle;
            a.current_run_id = None;
        }
        if status == RunStatus::Succeeded {
            a.runs_completed += 1;
        }
    }

    let ticket = run_ticket(db, &run).await?;
    let tab = match &ticket {
        Some(t) => db.tab(t.tab_id).await?,
        None => None,
    };
    let mut landed_in: Option<Id> = None;
    if let (Some(mut ticket), Some(tab), Some(a)) = (ticket, tab, agent.as_mut()) {
        let refs = Refs::ticket(&ticket).agent(a.id).run(run.id);
        match (run.kind, status) {
            (RunKind::Work, RunStatus::Succeeded) => {
                let skills = db.skills().await?;
                let matches = ticket.tags.iter().any(|tag| {
                    a.skill_ids.iter().any(|sid| {
                        skills
                            .iter()
                            .any(|s| s.id == *sid && s.name.eq_ignore_ascii_case(tag))
                    })
                });
                let xp = if matches {
                    XP_WORK_SKILL_MATCH
                } else {
                    XP_WORK
                };
                let ups = grant_xp(a, xp);
                ticket.review_feedback = None;
                if let Some(col) =
                    first_of(&tab, ColumnKind::Review).or_else(|| first_of(&tab, ColumnKind::Done))
                {
                    place(state, &mut ticket, col).await?;
                }
                save_ticket(state, &ticket).await?;
                save_agent(state, a).await?;
                state.fx(Fx::WorkFinished {
                    agent_id: a.id,
                    ticket_id: ticket.id,
                });
                let pr = ticket
                    .pr_number
                    .map(|n| format!(", PR #{n} is ready"))
                    .unwrap_or_default();
                state
                    .record(
                        HistoryKind::RunSucceeded,
                        Actor::Agent(a.id),
                        refs,
                        format!("{} finished {}{pr} (+{xp} XP)", a.name, tref(&ticket)),
                        Some(json!({ "xp": xp, "cost_usd": run.cost_usd, "summary": run.summary })),
                    )
                    .await;
                xp_effects(state, a, xp, &ups, refs).await;
            }
            (RunKind::Review, RunStatus::Succeeded) => {
                let approved = run.verdict == Some(ReviewVerdict::Approved);
                let ups = grant_xp(a, XP_REVIEW);
                save_agent(state, a).await?;
                let here = column_named(&tab, Some(ticket.column_id)).cloned();
                if approved {
                    let first_round = ticket.review_rounds == 0;
                    let target = here
                        .as_ref()
                        .and_then(|c| column_named(&tab, c.on_approve_column_id))
                        .or_else(|| first_of(&tab, ColumnKind::Done))
                        .cloned();
                    if let Some(col) = target {
                        place(state, &mut ticket, &col).await?;
                    }
                    ticket.review_feedback = None;
                    save_ticket(state, &ticket).await?;
                    // The author gets credit for the approval.
                    let mut author_level = 1;
                    if let Some(author_id) = ticket.agent_id.filter(|id| *id != a.id) {
                        if let Some(mut author) = db.agent(author_id).await? {
                            author.prs_approved += 1;
                            let bonus = if first_round { XP_FIRST_ROUND_BONUS } else { 0 };
                            let ups = if bonus > 0 {
                                grant_xp(&mut author, bonus)
                            } else {
                                Vec::new()
                            };
                            author_level = author.level;
                            save_agent(state, &author).await?;
                            if bonus > 0 {
                                xp_effects(
                                    state,
                                    &author,
                                    bonus,
                                    &ups,
                                    Refs::ticket(&ticket).agent(author.id),
                                )
                                .await;
                            }
                        }
                    } else if ticket.agent_id == Some(a.id) {
                        a.prs_approved += 1;
                        author_level = a.level;
                        save_agent(state, a).await?;
                    }
                    let pearls = 10 + if first_round { 5 } else { 0 } + 2 * author_level;
                    let stats = state
                        .update_stats(|s| {
                            s.prs_approved += 1;
                            s.pearls += pearls as u64;
                        })
                        .await?;
                    state.fx(Fx::PrApproved {
                        agent_id: ticket.agent_id.unwrap_or(a.id),
                        ticket_id: ticket.id,
                        reviewer_id: a.id,
                    });
                    state.fx(Fx::PearlsEarned {
                        ticket_id: ticket.id,
                        amount: pearls,
                        total: stats.pearls,
                    });
                    let round = if first_round { " on the first try" } else { "" };
                    state
                        .record(
                            HistoryKind::ReviewApproved,
                            Actor::Agent(a.id),
                            refs,
                            format!(
                                "{} approved {} of {}{round} (+{XP_REVIEW} XP, +{pearls} pearls)",
                                a.name,
                                pr_ref(&ticket),
                                tref(&ticket)
                            ),
                            Some(json!({ "xp": XP_REVIEW, "pearls": pearls, "feedback": outcome.feedback })),
                        )
                        .await;
                } else {
                    ticket.review_feedback = outcome
                        .feedback
                        .clone()
                        .or_else(|| run.summary.clone())
                        .or_else(|| Some("Changes requested.".into()));
                    ticket.review_rounds += 1;
                    let target = here
                        .as_ref()
                        .and_then(|c| column_named(&tab, c.on_changes_column_id))
                        .or_else(|| first_of(&tab, ColumnKind::Todo))
                        .cloned();
                    if let Some(col) = target {
                        place(state, &mut ticket, &col).await?;
                    }
                    save_ticket(state, &ticket).await?;
                    state.fx(Fx::ChangesRequested {
                        ticket_id: ticket.id,
                        reviewer_id: a.id,
                    });
                    state
                        .record(
                            HistoryKind::ReviewChangesRequested,
                            Actor::Agent(a.id),
                            refs,
                            format!(
                                "{} requested changes on {} of {} (+{XP_REVIEW} XP)",
                                a.name,
                                pr_ref(&ticket),
                                tref(&ticket)
                            ),
                            Some(json!({ "xp": XP_REVIEW, "feedback": ticket.review_feedback })),
                        )
                        .await;
                }
                xp_effects(state, a, XP_REVIEW, &ups, refs).await;
            }
            (kind, status) => {
                save_agent(state, a).await?;
                let cancelled = status == RunStatus::Cancelled;
                let err = error.clone().unwrap_or_else(|| "unknown error".into());
                if kind == RunKind::Work {
                    if let Some(todo) = first_of(&tab, ColumnKind::Todo) {
                        place(state, &mut ticket, todo).await?;
                    }
                    if !cancelled {
                        ticket.review_feedback = Some(format!("Run failed: {err}"));
                    }
                    save_ticket(state, &ticket).await?;
                }
                if cancelled {
                    state
                        .record(
                            HistoryKind::RunCancelled,
                            Actor::System,
                            refs,
                            format!("{}'s run on {} was cancelled", a.name, tref(&ticket)),
                            None,
                        )
                        .await;
                } else {
                    state.fx(Fx::RunFailed {
                        agent_id: a.id,
                        ticket_id: ticket.id,
                    });
                    let what = if kind == RunKind::Review {
                        "review of"
                    } else {
                        "run on"
                    };
                    state
                        .record(
                            HistoryKind::RunFailed,
                            Actor::Agent(a.id),
                            refs,
                            format!("{}'s {what} {} failed: {err}", a.name, tref(&ticket)),
                            Some(json!({ "error": err })),
                        )
                        .await;
                }
            }
        }
        // Wherever the ticket landed (work done -> PR column, approve/changes routing into
        // another review column, a failed review left in place for one retry), its
        // column's auto reviewer may pick it up.
        landed_in = Some(ticket.id);
    } else if let Some(a) = agent.as_ref() {
        save_agent(state, a).await?;
    }

    if !follow_up {
        return Ok(());
    }
    if let Some(tid) = landed_in {
        maybe_auto_review_locked(state, tid).await;
    }
    // The agent is free again: it may have reviews waiting; a runner slot is free too.
    if let Some(a) = agent {
        drain_review_queue_locked(state, a.id).await;
    }
    dispatch_queued_locked(state).await;
    Ok(())
}

async fn cleanup_live(state: &SharedState, run_id: Id) {
    state.flush_logs().await;
    state.hub.close_run(run_id);
    state.drop_seq(run_id);
    if let Ok(mut live) = state.live.lock() {
        live.remove(&run_id);
    }
}

// ---------------------------------------------------------------- auto review

/// Start the column's auto reviewer on `ticket_id` if it sits in a review column, the
/// reviewer is idle and nothing runs on the ticket. Otherwise it waits in the queue.
pub async fn maybe_auto_review_locked(state: &SharedState, ticket_id: Id) {
    let Ok(Some(ticket)) = state.db.ticket(ticket_id).await else {
        return;
    };
    let Ok(Some(col)) = state.db.column(ticket.column_id).await else {
        return;
    };
    if col.kind != ColumnKind::Review {
        return;
    }
    let Some(reviewer) = col.auto_reviewer_agent_id else {
        return;
    };
    drain_review_queue_locked(state, reviewer).await;
}

/// If `agent_id` is an idle auto reviewer, start it on the oldest waiting ticket.
pub async fn drain_review_queue_locked(state: &SharedState, agent_id: Id) {
    let Ok(Some(agent)) = state.db.agent(agent_id).await else {
        return;
    };
    if agent.status != AgentStatus::Idle {
        return;
    }
    let queue = match state.db.review_queue(agent_id).await {
        Ok(q) => q,
        Err(e) => {
            tracing::error!(error = ?e, "review queue");
            return;
        }
    };
    for ticket_id in queue {
        let res = create_run_boxed(
            state,
            NewRun {
                ticket_id,
                agent_id,
                kind: Some(RunKind::Review),
                instructions: None,
                resume_session_id: None,
                trigger: Trigger::Auto,
            },
        )
        .await;
        match res {
            Ok(_) => return,
            Err(e) => tracing::warn!(error = %e, %ticket_id, "auto review could not start"),
        }
    }
}

/// Type-erased (and explicitly `Send`) to break the async recursion
/// create → finish → drain → create.
fn create_run_boxed(state: &SharedState, req: NewRun) -> BoxFuture<'_, AppResult<Run>> {
    Box::pin(create_run_locked(state, req))
}

/// Every idle auto reviewer picks up its queue (startup, column config changes).
pub async fn drain_all_locked(state: &SharedState) {
    let Ok(tabs) = state.db.tabs().await else {
        return;
    };
    let mut reviewers: Vec<Id> = tabs
        .iter()
        .flat_map(|t| t.columns.iter())
        .filter_map(|c| c.auto_reviewer_agent_id)
        .collect();
    reviewers.sort();
    reviewers.dedup();
    for r in reviewers {
        drain_review_queue_locked(state, r).await;
    }
    dispatch_queued_locked(state).await;
}

// ---------------------------------------------------------------- controls

pub async fn cancel(state: &SharedState, run_id: Id, by: Id) -> AppResult<Run> {
    let run = {
        let _g = state.orch_lock.lock().await;
        let run = state
            .db
            .run(run_id)
            .await?
            .ok_or(AppError::NotFound("run"))?;
        if run.status.is_terminal() {
            return Err(AppError::conflict("this run has already finished"));
        }
        let live = state
            .live
            .lock()
            .map(|l| l.contains(&run_id))
            .unwrap_or(false);
        state
            .server_log(
                run_id,
                LogKind::System,
                "Cancel requested",
                Some(json!({ "user_id": by })),
            )
            .await;
        if !live {
            finish_locked(state, run_id, cancelled_outcome()).await?;
            return Ok(state.db.run(run_id).await?.unwrap_or(run));
        }
        run
    };
    if let Err(e) = state.runner.cancel(run_id).await {
        tracing::warn!(error = ?e, %run_id, "runner cancel failed, forcing");
        let _g = state.orch_lock.lock().await;
        finish_locked(state, run_id, cancelled_outcome()).await?;
    } else {
        // Watchdog: a runner that never confirms must not keep the agent busy forever.
        let s = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_GRACE).await;
            let _g = s.orch_lock.lock().await;
            if !is_finished(&s, run_id).await {
                let _ = finish_locked(&s, run_id, cancelled_outcome()).await;
            }
        });
    }
    Ok(state.db.run(run_id).await?.unwrap_or(run))
}

fn cancelled_outcome() -> RunOutcome {
    RunOutcome {
        status: RunStatus::Cancelled,
        summary: None,
        verdict: None,
        feedback: None,
        error: Some("cancelled".into()),
        plan: None,
    }
}

/// Follow-up message: into the live run, or a new resumed run when it has finished.
pub async fn message(state: &SharedState, run_id: Id, text: String, by: Id) -> AppResult<Run> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::bad("message is empty"));
    }
    let run = state
        .db
        .run(run_id)
        .await?
        .ok_or(AppError::NotFound("run"))?;
    let live = state
        .live
        .lock()
        .map(|l| l.contains(&run_id))
        .unwrap_or(false);
    if !run.status.is_terminal() && live {
        state
            .runner
            .send_message(run_id, text.clone())
            .await
            .map_err(|e| AppError::conflict(format!("could not deliver the message: {e}")))?;
        state
            .server_log(run_id, LogKind::User, text, Some(json!({ "user_id": by })))
            .await;
        return Ok(run);
    }
    if !run.status.is_terminal() {
        return Err(AppError::conflict("this run has not started yet"));
    }
    let Some(ticket_id) = run.ticket_id else {
        return Err(AppError::bad(
            "follow-up messages are only supported on ticket runs",
        ));
    };
    assign(
        state,
        NewRun {
            ticket_id,
            agent_id: run.agent_id,
            kind: Some(run.kind),
            instructions: Some(text),
            resume_session_id: run.session_id.clone(),
            trigger: Trigger::User(by),
        },
    )
    .await
}

pub async fn answer_permission(
    state: &SharedState,
    run_id: Id,
    request_id: String,
    allow: bool,
    by: Id,
) -> AppResult<()> {
    let _g = state.orch_lock.lock().await;
    let mut run = state
        .db
        .run(run_id)
        .await?
        .ok_or(AppError::NotFound("run"))?;
    if run.status.is_terminal() {
        return Err(AppError::conflict("this run has already finished"));
    }
    state
        .runner
        .answer_permission(run_id, request_id.clone(), allow)
        .await
        .map_err(|e| AppError::conflict(format!("could not answer: {e}")))?;
    state
        .server_log(
            run_id,
            LogKind::System,
            if allow { "Permission granted" } else { "Permission denied" },
            Some(json!({ "permission_resolved": { "request_id": request_id, "allow": allow, "user_id": by } })),
        )
        .await;
    if run.status == RunStatus::Waiting {
        run.status = RunStatus::Running;
        run.activity = Some(if allow {
            "Permission granted".into()
        } else {
            "Permission denied".into()
        });
        save_run(state, &run).await?;
        if let Some(mut a) = state.db.agent(run.agent_id).await? {
            if a.status == AgentStatus::Waiting {
                a.status = status_for(run.kind);
                save_agent(state, &a).await?;
            }
        }
    }
    Ok(())
}

/// Stop the active run of a ticket without waiting (ticket is being deleted).
pub async fn abort_ticket_runs_locked(state: &SharedState, ticket_id: Id) -> anyhow::Result<()> {
    for run in state.db.runs_for_ticket(ticket_id).await? {
        if run.status.is_terminal() {
            continue;
        }
        let _ = state.runner.cancel(run.id).await;
        finish_locked(state, run.id, cancelled_outcome()).await?;
    }
    Ok(())
}

/// Startup: nothing survives a restart. Every non-terminal run is failed first, with no
/// dispatching or auto review in between (so nothing is started and then marked failed),
/// agents go Idle, and only then do the review queues resume once.
pub async fn recover(state: &SharedState) -> anyhow::Result<()> {
    let _g = state.orch_lock.lock().await;
    for run in state.db.active_runs().await? {
        tracing::info!(run_id = %run.id, "failing run interrupted by restart");
        finish_inner(
            state,
            run.id,
            RunOutcome {
                status: RunStatus::Failed,
                summary: None,
                verdict: None,
                feedback: None,
                error: Some("server restarted".into()),
                plan: None,
            },
            false,
        )
        .await?;
    }
    for mut a in state.db.agents().await? {
        if a.status != AgentStatus::Idle || a.current_run_id.is_some() {
            a.status = AgentStatus::Idle;
            a.current_run_id = None;
            state.db.save_agent(&a).await?;
        }
    }
    drain_all_locked(state).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        Agent {
            id: new_id(),
            name: "A".into(),
            role: "Dev".into(),
            sprite: "boba".into(),
            color: "#000".into(),
            tagline: String::new(),
            system_prompt: String::new(),
            skill_ids: vec![],
            model: None,
            permission_mode: PermissionMode::Default,
            allowed_tools: vec![],
            max_turns: None,
            xp: 0,
            level: 1,
            runs_completed: 0,
            prs_approved: 0,
            status: AgentStatus::Idle,
            current_run_id: None,
            connector_ids: vec![],
            board_tools: false,
            created_by: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn read_only_runs_never_get_write_tools() {
        let agent_tools: Vec<String> = [
            "Bash(*)",
            "Write",
            "Edit",
            "mcp__linear__get_issue",
            "mcp__linear__create_issue",
            "mcp__gsc__search_analytics",
            "mcp__x__*",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = read_only_tools(&agent_tools);
        for bad in [
            "Bash(*)",
            "Write",
            "Edit",
            "mcp__linear__create_issue",
            "mcp__x__*",
        ] {
            assert!(!got.iter().any(|t| t == bad), "{bad} leaked");
        }
        assert!(got.iter().any(|t| t == "mcp__linear__get_issue"));
        assert!(got.iter().any(|t| t == "mcp__gsc__search_analytics"));
        assert!(got.iter().any(|t| t == "Bash(gh pr diff:*)"));
    }

    #[test]
    fn xp_levels_up_and_carries_over() {
        let mut a = agent();
        assert!(grant_xp(&mut a, 90).is_empty());
        assert_eq!(grant_xp(&mut a, 20), vec![2]);
        assert_eq!((a.level, a.xp), (2, 10));
        // 150 to reach 3, 200 to reach 4
        assert_eq!(grant_xp(&mut a, 350), vec![3, 4]);
        assert_eq!((a.level, a.xp), (4, 10));
    }
}

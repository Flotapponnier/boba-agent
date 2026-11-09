//! GitHub repo sync: issues and PRs in, comments / reviews / closes back out.
//!
//! Imports are idempotent: `github_links (repo_id, kind, number)` is unique. HTTP calls never
//! run under the orchestrator lock; only the board updates do.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use boba_core::api::SyncReport;
use boba_core::automation::TicketSource;
use boba_core::event::{Entity, HistoryKind};
use boba_core::*;
use boba_github::{CloseReason, GitHub, Issue, PullRequest, RepoRef, ReviewEvent, WebhookEvent};
use chrono::Utc;
use serde_json::json;
use sqlx::Row;

use crate::auth::Auth;
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::orchestrator::{self, tref};
use crate::state::{Actor, Refs, SharedState};

const POLL_EVERY: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------- client

/// The GitHub client: `BOBA_GITHUB_API` (GitHub Enterprise) + `GH_TOKEN` / `GITHUB_TOKEN` /
/// `gh auth token`. Cached once it works; tests install their own.
pub async fn github(state: &SharedState) -> Result<GitHub, String> {
    if let Some(g) = state.github.lock().await.clone() {
        return Ok(g);
    }
    let token = boba_github::token_from_env()
        .await
        .ok_or_else(|| boba_github::Error::NoToken.to_string())?;
    let g = match std::env::var("BOBA_GITHUB_API") {
        Ok(api) if !api.trim().is_empty() => GitHub::with_api(&token, &api),
        _ => GitHub::new(&token),
    }
    .map_err(|e| e.to_string())?;
    *state.github.lock().await = Some(g.clone());
    Ok(g)
}

fn repo_ref(repo: &Repo) -> Result<RepoRef, String> {
    repo.remote_url
        .as_deref()
        .and_then(RepoRef::parse)
        .ok_or_else(|| format!("{} has no GitHub remote URL", repo.name))
}

// ---------------------------------------------------------------- links

async fn link(
    state: &SharedState,
    repo: Id,
    kind: &str,
    number: i64,
) -> anyhow::Result<Option<Id>> {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT ticket_id FROM github_links WHERE repo_id = ? AND kind = ? AND number = ?",
    )
    .bind(repo.to_string())
    .bind(kind)
    .bind(number)
    .fetch_optional(&state.db.pool)
    .await?;
    Ok(v.and_then(|s| Id::parse_str(&s).ok()))
}

async fn links(state: &SharedState, repo: Id, kind: &str) -> anyhow::Result<Vec<(i64, Id)>> {
    let rows =
        sqlx::query("SELECT number, ticket_id FROM github_links WHERE repo_id = ? AND kind = ?")
            .bind(repo.to_string())
            .bind(kind)
            .fetch_all(&state.db.pool)
            .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let n: i64 = r.try_get("number").ok()?;
            let t: String = r.try_get("ticket_id").ok()?;
            Some((n, Id::parse_str(&t).ok()?))
        })
        .collect())
}

async fn add_link(
    state: &SharedState,
    repo: Id,
    kind: &str,
    number: i64,
    ticket: Id,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO github_links(repo_id, kind, number, ticket_id) VALUES(?, ?, ?, ?)",
    )
    .bind(repo.to_string())
    .bind(kind)
    .bind(number)
    .bind(ticket.to_string())
    .execute(&state.db.pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- sync

async fn running(state: &SharedState, ticket: Id) -> anyhow::Result<bool> {
    Ok(state
        .db
        .runs_for_ticket(ticket)
        .await?
        .iter()
        .any(|r| !r.status.is_terminal()))
}

/// `place` = (tab, column, position).
fn new_ticket(
    place: (Id, Id, f64),
    number: i64,
    title: &str,
    body: &str,
    tags: Vec<String>,
    source: TicketSource,
) -> Ticket {
    let now = Utc::now();
    Ticket {
        id: new_id(),
        number,
        tab_id: place.0,
        column_id: place.1,
        position: place.2,
        title: title.chars().take(200).collect(),
        body: body.chars().take(50_000).collect(),
        tags: tags.into_iter().take(12).collect(),
        priority: Priority::Normal,
        color: None,
        repo_id: None,
        agent_id: None,
        branch: None,
        pr_url: None,
        pr_number: None,
        review_feedback: None,
        review_rounds: 0,
        created_by: None,
        created_at: now,
        updated_at: now,
        source,
        comment_count: 0,
        open_thread_count: 0,
    }
}

/// Sync one repository now.
pub async fn sync_repo(state: &SharedState, repo_id: Id) -> AppResult<SyncReport> {
    let repo = state
        .db
        .repo(repo_id)
        .await?
        .ok_or(AppError::NotFound("repo"))?;
    let tab_id = repo
        .sync
        .target_tab_id
        .ok_or_else(|| AppError::bad("pick the tab that receives imported tickets first"))?;
    let gh_ref = repo_ref(&repo).map_err(AppError::bad)?;
    let result = fetch_and_apply(state, &repo, tab_id, &gh_ref).await;
    // Record the outcome on the repo either way.
    let mut repo = state
        .db
        .repo(repo_id)
        .await?
        .ok_or(AppError::NotFound("repo"))?;
    match &result {
        Ok(_) => {
            repo.sync.last_synced_at = Some(Utc::now());
            repo.sync.last_error = None;
        }
        Err(e) => repo.sync.last_error = Some(e.clone()),
    }
    state.db.save_repo(&repo).await?;
    state.upsert(Entity::Repo(repo.clone()));
    let report = result.map_err(AppError::bad)?;
    let changed = report.imported_issues + report.imported_prs + report.updated + report.closed;
    if changed > 0 {
        state
            .record(
                HistoryKind::RepoSynced,
                Actor::System,
                Refs {
                    tab: Some(tab_id),
                    ..Default::default()
                },
                format!(
                    "Synced {}: {} new issue(s), {} new PR(s), {} updated, {} closed",
                    gh_ref,
                    report.imported_issues,
                    report.imported_prs,
                    report.updated,
                    report.closed
                ),
                Some(json!({ "repo_id": repo.id, "report": report })),
            )
            .await;
    }
    Ok(report)
}

async fn fetch_and_apply(
    state: &SharedState,
    repo: &Repo,
    tab_id: Id,
    gh_ref: &RepoRef,
) -> Result<SyncReport, String> {
    let gh = github(state).await?;
    let issues: Vec<Issue> = if repo.sync.import_issues {
        gh.list_open_issues(gh_ref, &repo.sync.issue_labels)
            .await
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    let pulls: Vec<PullRequest> = if repo.sync.import_prs {
        gh.list_open_pulls(gh_ref, repo.sync.skip_draft_prs)
            .await
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    apply(state, repo, tab_id, gh_ref, &issues, &pulls)
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "applying repo sync");
            "internal error while syncing".to_string()
        })
}

/// Board side of a sync (under the orchestrator lock).
async fn apply(
    state: &SharedState,
    repo: &Repo,
    tab_id: Id,
    gh_ref: &RepoRef,
    issues: &[Issue],
    pulls: &[PullRequest],
) -> anyhow::Result<SyncReport> {
    let mut report = SyncReport {
        imported_issues: 0,
        imported_prs: 0,
        updated: 0,
        closed: 0,
        errors: Vec::new(),
    };
    let _g = state.orch_lock.lock().await;
    let db = &state.db;
    let Some(tab) = db.tab(tab_id).await? else {
        report.errors.push("the target tab was deleted".into());
        return Ok(report);
    };
    let todo = tab
        .columns
        .iter()
        .find(|c| c.kind == ColumnKind::Todo)
        .or_else(|| tab.columns.first())
        .cloned();
    let review = tab
        .columns
        .iter()
        .find(|c| c.kind == ColumnKind::Review)
        .cloned()
        .or_else(|| todo.clone());
    let done = tab
        .columns
        .iter()
        .find(|c| c.kind == ColumnKind::Done)
        .cloned();
    let (Some(todo), Some(review)) = (todo, review) else {
        report.errors.push("the target tab has no columns".into());
        return Ok(report);
    };

    let mut to_review = Vec::new();
    for (kind, items) in [
        ("issue", issues.iter().map(Item::Issue).collect::<Vec<_>>()),
        ("pr", pulls.iter().map(Item::Pull).collect::<Vec<_>>()),
    ] {
        for item in items {
            let (number, title, body, labels) = item.fields();
            match link(state, repo.id, kind, number).await? {
                Some(tid) => {
                    // Keep title/body in sync while nobody works on it.
                    let Some(mut t) = db.ticket(tid).await? else {
                        continue;
                    };
                    let title: String = title.chars().take(200).collect();
                    if (t.title != title || t.body != body) && !running(state, tid).await? {
                        t.title = title;
                        t.body = body.to_string();
                        t.updated_at = Utc::now();
                        db.save_ticket(&t).await?;
                        state.upsert(Entity::Ticket(db.ticket(tid).await?.unwrap_or(t)));
                        report.updated += 1;
                    }
                }
                None => {
                    let column = if kind == "pr" { &review } else { &todo };
                    let (source, branch, pr) = match item {
                        Item::Issue(i) => (
                            TicketSource::GithubIssue {
                                repo_id: repo.id,
                                number: i.number,
                                url: i.url.clone(),
                                author: i.author.clone(),
                            },
                            None,
                            None,
                        ),
                        Item::Pull(p) => (
                            TicketSource::GithubPr {
                                repo_id: repo.id,
                                number: p.number,
                                url: p.url.clone(),
                                author: p.author.clone(),
                                head_branch: p.head_branch.clone(),
                            },
                            Some(p.head_branch.clone()),
                            Some((p.number, p.url.clone())),
                        ),
                    };
                    let position = db.max_position(column.id).await? + 1024.0;
                    let mut t = new_ticket(
                        (tab.id, column.id, position),
                        db.next_ticket_number().await?,
                        title,
                        body,
                        labels.to_vec(),
                        source,
                    );
                    t.repo_id = Some(repo.id);
                    t.branch = branch;
                    if let Some((n, url)) = pr {
                        t.pr_number = Some(n);
                        t.pr_url = Some(url);
                    }
                    db.save_ticket(&t).await?;
                    add_link(state, repo.id, kind, number, t.id).await?;
                    state.upsert(Entity::Ticket(t.clone()));
                    let what = if kind == "pr" { "PR" } else { "issue" };
                    state
                        .record(
                            HistoryKind::TicketImported,
                            Actor::System,
                            Refs::ticket(&t),
                            format!(
                                "Imported {what} #{number} from {gh_ref} as {}: {}",
                                tref(&t),
                                t.title
                            ),
                            Some(json!({ "repo_id": repo.id, "kind": kind, "number": number })),
                        )
                        .await;
                    if kind == "pr" {
                        report.imported_prs += 1;
                        to_review.push(t.id);
                    } else {
                        report.imported_issues += 1;
                    }
                }
            }
        }
    }

    // Closed (or merged) upstream: the paper goes to Done.
    if let Some(done) = &done {
        for (kind, open) in [
            ("issue", issues.iter().map(|i| i.number).collect::<Vec<_>>()),
            ("pr", pulls.iter().map(|p| p.number).collect::<Vec<_>>()),
        ] {
            let enabled = if kind == "pr" {
                repo.sync.import_prs
            } else {
                repo.sync.import_issues
            };
            if !enabled {
                continue;
            }
            for (number, tid) in links(state, repo.id, kind).await? {
                if open.contains(&number) {
                    continue;
                }
                let Some(mut t) = db.ticket(tid).await? else {
                    continue;
                };
                if t.column_id == done.id || running(state, tid).await? {
                    continue;
                }
                t.column_id = done.id;
                t.position = db.max_position(done.id).await? + 1024.0;
                t.updated_at = Utc::now();
                db.save_ticket(&t).await?;
                state.upsert(Entity::Ticket(db.ticket(tid).await?.unwrap_or(t.clone())));
                state
                    .record(
                        HistoryKind::TicketMoved,
                        Actor::System,
                        Refs::ticket(&t),
                        format!("{} was closed on GitHub, moved to {}", tref(&t), done.name),
                        Some(json!({ "to": done.name })),
                    )
                    .await;
                report.closed += 1;
            }
        }
    }
    // Imported PRs sit in the review column: its auto reviewer takes them.
    for tid in to_review {
        orchestrator::maybe_auto_review_locked(state, tid).await;
    }
    Ok(report)
}

enum Item<'a> {
    Issue(&'a Issue),
    Pull(&'a PullRequest),
}

impl Item<'_> {
    fn fields(&self) -> (i64, &str, &str, &[String]) {
        match self {
            Item::Issue(i) => (i.number, &i.title, &i.body, &i.labels),
            Item::Pull(p) => (p.number, &p.title, &p.body, &p.labels),
        }
    }
}

// ---------------------------------------------------------------- triggers

/// Poll every enabled repo whose `poll_minutes` elapsed.
pub fn spawn_poller(state: SharedState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(POLL_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Ok(repos) = state.db.repos().await else {
                continue;
            };
            for r in repos.into_iter().filter(|r| r.sync.enabled) {
                let due = r.sync.last_synced_at.is_none_or(|t| {
                    Utc::now() - t >= chrono::Duration::minutes(r.sync.poll_minutes.max(1) as i64)
                });
                if due {
                    if let Err(e) = sync_repo(&state, r.id).await {
                        tracing::warn!(repo = %r.name, error = %e, "repo sync");
                    }
                }
            }
        }
    });
}

/// `POST /api/repos/:id/sync` [RunAgents]
pub async fn sync_now(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<SyncReport>> {
    me.require(Permission::RunAgents)?;
    Ok(Json(sync_repo(&state, id).await?))
}

/// `POST /api/github/webhook` (public; HMAC with `BOBA_GITHUB_WEBHOOK_SECRET`).
pub async fn webhook(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let Some(secret) = state.cfg.github_webhook_secret.clone() else {
        return StatusCode::NOT_FOUND;
    };
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !boba_github::verify_signature(secret.as_bytes(), &body, sig) {
        return StatusCode::UNAUTHORIZED;
    }
    let event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let repo = match boba_github::parse_event(event, &body) {
        Ok(WebhookEvent::Ping { .. }) => return StatusCode::OK,
        Ok(WebhookEvent::Issue { repo, .. }) | Ok(WebhookEvent::PullRequest { repo, .. }) => repo,
        Ok(WebhookEvent::Other {
            repo: Some(repo), ..
        }) => repo,
        Ok(WebhookEvent::Other { repo: None, .. }) => return StatusCode::OK,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let Ok(repos) = state.db.repos().await else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    for r in repos {
        let matches = r.sync.enabled
            && r.remote_url.as_deref().and_then(RepoRef::parse).as_ref() == Some(&repo);
        if matches {
            let s = state.clone();
            tokio::spawn(async move {
                if let Err(e) = sync_repo(&s, r.id).await {
                    tracing::warn!(repo = %r.name, error = %e, "webhook sync");
                }
            });
        }
    }
    StatusCode::OK
}

// ---------------------------------------------------------------- back to GitHub

/// Where a ticket came from on GitHub, if it is linked to a synced repo.
async fn origin(state: &SharedState, t: &Ticket) -> Option<(Repo, RepoRef, i64, bool)> {
    let (repo_id, number, is_pr) = match &t.source {
        TicketSource::GithubIssue {
            repo_id, number, ..
        } => (*repo_id, *number, false),
        TicketSource::GithubPr {
            repo_id, number, ..
        } => (*repo_id, *number, true),
        _ => return None,
    };
    let repo = state.db.repo(repo_id).await.ok()??;
    let r = repo_ref(&repo).ok()?;
    Some((repo, r, number, is_pr))
}

/// After a run on an imported ticket ends: comments / PR reviews back on GitHub.
/// Spawned: never blocks the orchestrator, failures only log.
pub fn after_run(state: &SharedState, run: Run) {
    let state = state.clone();
    tokio::spawn(async move {
        let Some(tid) = run.ticket_id else { return };
        let Ok(Some(t)) = state.db.ticket(tid).await else {
            return;
        };
        let Some((repo, gh_ref, number, is_pr)) = origin(&state, &t).await else {
            return;
        };
        if run.status != RunStatus::Succeeded {
            return;
        }
        let Ok(gh) = github(&state).await else { return };
        let agent = state
            .db
            .agent(run.agent_id)
            .await
            .ok()
            .flatten()
            .map(|a| a.name)
            .unwrap_or_else(|| "An agent".into());
        let summary = run.summary.clone().unwrap_or_default();
        let res = match (run.kind, run.verdict) {
            (RunKind::Review, Some(v)) if is_pr && repo.sync.post_reviews => {
                let event = match v {
                    ReviewVerdict::Approved => ReviewEvent::Approve,
                    ReviewVerdict::ChangesRequested => ReviewEvent::RequestChanges,
                };
                let body = format!(
                    "**{agent}** (Boba Agent): {}\n\n{}",
                    verdict_word(v),
                    feedback(&t, &summary)
                );
                match gh.submit_review(&gh_ref, number, event, &body).await {
                    // GitHub refuses approve/request-changes on your own PR: leave a comment.
                    Err(boba_github::Error::Api { status: 422, .. }) => gh
                        .submit_review(&gh_ref, number, ReviewEvent::Comment, &body)
                        .await
                        .map(|_| ()),
                    other => other.map(|_| ()),
                }
            }
            (RunKind::Review, Some(v)) if repo.sync.comment_back => {
                let body = format!(
                    "🧋 **{agent}** reviewed this: {}\n\n{}",
                    verdict_word(v),
                    feedback(&t, &summary)
                );
                gh.comment(&gh_ref, number, &body).await.map(|_| ())
            }
            (RunKind::Work, _) if repo.sync.comment_back && !is_pr => {
                let pr = t
                    .pr_url
                    .as_deref()
                    .map(|u| format!("\n\nPull request: {u}"))
                    .unwrap_or_default();
                let body = format!(
                    "🧋 **{agent}** worked on this in Boba Agent ({}):\n\n{summary}{pr}",
                    tref(&t)
                );
                gh.comment(&gh_ref, number, &body).await.map(|_| ())
            }
            _ => Ok(()),
        };
        if let Err(e) = res {
            tracing::warn!(error = %e, ticket = %tref(&t), "posting back to GitHub");
        }
    });
}

fn verdict_word(v: ReviewVerdict) -> &'static str {
    match v {
        ReviewVerdict::Approved => "approved ✅",
        ReviewVerdict::ChangesRequested => "changes requested",
    }
}

fn feedback(t: &Ticket, summary: &str) -> String {
    t.review_feedback
        .clone()
        .unwrap_or_else(|| summary.to_string())
}

/// A ticket reached a Done column: close its issue upstream when the repo asks for it.
pub fn ticket_done(state: &SharedState, ticket: Ticket) {
    let state = state.clone();
    tokio::spawn(async move {
        let Some((repo, gh_ref, number, is_pr)) = origin(&state, &ticket).await else {
            return;
        };
        if is_pr || !repo.sync.close_issues_on_done {
            return;
        }
        let Ok(gh) = github(&state).await else { return };
        if let Err(e) = gh
            .close_issue(&gh_ref, number, CloseReason::Completed)
            .await
        {
            tracing::warn!(error = %e, ticket = %tref(&ticket), "closing issue on GitHub");
        }
    });
}

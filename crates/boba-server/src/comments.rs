//! Comment threads, mentions, reactions, and agents answering when @mentioned.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use boba_core::api::{CreateComment, ReactRequest, UpdateComment};
use boba_core::event::{Entity, HistoryKind};
use boba_core::team::{Comment, CommentAnchor, CommentAuthorKind, NotificationKind, Reaction};
use boba_core::*;
use chrono::Utc;
use serde_json::json;

use crate::auth::Auth;
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::notify::Target;
use crate::orchestrator::{self, tref, NewRun, Trigger};
use crate::state::{Actor, Refs, SharedState};

const MAX_BODY: usize = 10_000;
const MAX_MENTIONS: usize = 20;

// ---------------------------------------------------------------- mentions

#[derive(Debug, Default, PartialEq)]
pub struct Mentions {
    pub users: Vec<Id>,
    pub agents: Vec<Id>,
}

/// Every `@[Name](user:<id>)` / `@[Name](agent:<id>)` in a body, deduplicated, in order.
pub fn parse_mentions(body: &str) -> Mentions {
    let mut out = Mentions::default();
    for (_, kind, id) in mention_spans(body) {
        let list = if kind == "user" {
            &mut out.users
        } else {
            &mut out.agents
        };
        if !list.contains(&id) && list.len() < MAX_MENTIONS {
            list.push(id);
        }
    }
    out
}

/// Body with mentions rendered as plain `@Name` (what an agent reads as instructions).
pub fn plain(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("@[") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match parse_one(tail) {
            Some((len, name, _, _)) => {
                out.push('@');
                out.push_str(name);
                rest = &tail[len..];
            }
            None => {
                out.push_str("@[");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn mention_spans(body: &str) -> Vec<(&str, &str, Id)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("@[") {
        let tail = &rest[start..];
        match parse_one(tail) {
            Some((len, name, kind, id)) => {
                out.push((name, kind, id));
                rest = &tail[len..];
            }
            None => rest = &tail[2..],
        }
    }
    out
}

/// Parse `@[Name](kind:uuid)` at the start of `s`: (length, name, kind, id).
fn parse_one(s: &str) -> Option<(usize, &str, &str, Id)> {
    let after = s.strip_prefix("@[")?;
    let close = after.find("](")?;
    let name = &after[..close];
    if name.is_empty() || name.len() > 100 || name.contains('\n') {
        return None;
    }
    let link = &after[close + 2..];
    let end = link.find(')')?;
    let (kind, id) = link[..end].split_once(':')?;
    if kind != "user" && kind != "agent" {
        return None;
    }
    let id = Id::parse_str(id.trim()).ok()?;
    Some((2 + close + 2 + end + 1, name, kind, id))
}

// ---------------------------------------------------------------- helpers

async fn broadcast_ticket(state: &SharedState, ticket_id: Option<Id>) {
    if let Some(tid) = ticket_id {
        if let Ok(Some(t)) = state.db.ticket(tid).await {
            state.upsert(Entity::Ticket(t));
        }
    }
}

fn author_name(state_users: &[User], agents: &[Agent], c: &Comment) -> String {
    match c.author_kind {
        CommentAuthorKind::User => state_users
            .iter()
            .find(|u| u.id == c.author_id)
            .map(|u| u.name.clone()),
        CommentAuthorKind::Agent => agents
            .iter()
            .find(|a| a.id == c.author_id)
            .map(|a| a.name.clone()),
    }
    .unwrap_or_else(|| "Someone".into())
}

/// Humans who took part in a thread (root author + human repliers).
fn participants(thread: &[Comment]) -> Vec<Id> {
    let mut out: Vec<Id> = Vec::new();
    for c in thread {
        if c.author_kind == CommentAuthorKind::User && !out.contains(&c.author_id) {
            out.push(c.author_id);
        }
    }
    out
}

fn short(text: &str, n: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n {
        format!("{}…", flat.chars().take(n).collect::<String>())
    } else {
        flat
    }
}

async fn validate_anchor(
    state: &SharedState,
    ticket_id: Id,
    anchor: &CommentAnchor,
) -> AppResult<()> {
    let run_id = match anchor {
        CommentAnchor::Ticket => return Ok(()),
        CommentAnchor::TicketBody { quote } => {
            if quote.trim().is_empty() || quote.chars().count() > 2_000 {
                return Err(AppError::bad(
                    "the quoted passage must be 1 to 2000 characters",
                ));
            }
            return Ok(());
        }
        CommentAnchor::RunLog { run_id, .. } | CommentAnchor::Run { run_id } => *run_id,
    };
    let run = state
        .db
        .run(run_id)
        .await?
        .ok_or(AppError::NotFound("run"))?;
    if run.ticket_id != Some(ticket_id) {
        return Err(AppError::bad("that run belongs to another ticket"));
    }
    Ok(())
}

// ---------------------------------------------------------------- routes

pub async fn list(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<Json<Vec<Comment>>> {
    me.require(Permission::View)?;
    state
        .db
        .ticket(id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    Ok(Json(state.db.ticket_comments(id).await?))
}

pub async fn create(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateComment>,
) -> AppResult<Json<Comment>> {
    let me = me.require(Permission::EditTickets)?.clone();
    if req.job_id.is_some() {
        return Err(AppError::bad("quest threads arrive with the Quest Board"));
    }
    let ticket_id = req
        .ticket_id
        .ok_or_else(|| AppError::bad("ticket_id is required"))?;
    let ticket = state
        .db
        .ticket(ticket_id)
        .await?
        .ok_or(AppError::NotFound("ticket"))?;
    let body = req.body.trim().to_string();
    if body.is_empty() || body.chars().count() > MAX_BODY {
        return Err(AppError::bad(format!(
            "comments must be 1 to {MAX_BODY} characters"
        )));
    }

    // Replies attach to the thread's root and inherit its anchor.
    let (parent_id, anchor) = match req.parent_id {
        Some(pid) => {
            let parent = state
                .db
                .comment(pid)
                .await?
                .ok_or(AppError::NotFound("comment"))?;
            if parent.ticket_id != Some(ticket_id) {
                return Err(AppError::bad("that thread belongs to another ticket"));
            }
            let root = match parent.parent_id {
                Some(r) => state
                    .db
                    .comment(r)
                    .await?
                    .ok_or(AppError::NotFound("comment"))?,
                None => parent,
            };
            (Some(root.id), root.anchor)
        }
        None => {
            validate_anchor(&state, ticket_id, &req.anchor).await?;
            (None, req.anchor)
        }
    };

    let users = state.db.users().await?;
    let agents = state.db.agents().await?;
    let m = parse_mentions(&body);
    let mentioned_users: Vec<Id> = m
        .users
        .into_iter()
        .filter(|u| users.iter().any(|x| x.id == *u))
        .collect();
    let mentioned_agents: Vec<Id> = m
        .agents
        .into_iter()
        .filter(|a| agents.iter().any(|x| x.id == *a))
        .collect();

    let mut comment = Comment {
        id: new_id(),
        ticket_id: Some(ticket_id),
        job_id: None,
        anchor,
        parent_id,
        author_kind: CommentAuthorKind::User,
        author_id: me.id,
        body,
        mentioned_user_ids: mentioned_users.clone(),
        mentioned_agent_ids: mentioned_agents.clone(),
        triggered_run_ids: Vec::new(),
        reactions: Vec::new(),
        resolved: false,
        resolved_by: None,
        created_at: Utc::now(),
        edited_at: None,
        deleted: false,
    };
    state.db.save_comment(&comment).await?;
    let root_id = parent_id.unwrap_or(comment.id);

    // @agent: start a run on the ticket with the comment as instructions.
    let mut refusals: Vec<(Id, String)> = Vec::new();
    for agent_id in &mentioned_agents {
        if !me.role.grants(Permission::RunAgents) {
            refusals.push((
                *agent_id,
                "I can only start working for teammates who are allowed to run agents.".into(),
            ));
            continue;
        }
        let resume = state.db.last_agent_session(ticket_id, *agent_id).await?;
        let started = orchestrator::assign(
            &state,
            NewRun {
                ticket_id,
                agent_id: *agent_id,
                kind: None,
                instructions: Some(format!(
                    "{} asked you in a comment on {}:\n\n{}",
                    me.name,
                    tref(&ticket),
                    plain(&comment.body)
                )),
                resume_session_id: resume,
                trigger: Trigger::User(me.id),
                reply_to: Some(root_id),
            },
        )
        .await;
        match started {
            Ok(run) => comment.triggered_run_ids.push(run.id),
            Err(e) => refusals.push((*agent_id, format!("I can't start right now: {e}."))),
        }
    }
    if !comment.triggered_run_ids.is_empty() {
        state.db.save_comment(&comment).await?;
    }
    state.upsert(Entity::Comment(comment.clone()));

    let preview = short(&plain(&comment.body), 120);
    state
        .record(
            HistoryKind::Comment,
            Actor::User(me.id),
            Refs::ticket(&ticket),
            format!("{} commented on {}: {preview}", me.name, tref(&ticket)),
            Some(json!({ "comment_id": comment.id, "thread_id": root_id })),
        )
        .await;

    let target = Target {
        ticket: Some(ticket_id),
        comment: Some(comment.id),
        actor_user: Some(me.id),
        ..Default::default()
    };
    state
        .notify(
            mentioned_users.iter().copied(),
            NotificationKind::Mention,
            &format!("{} mentioned you on {}: {preview}", me.name, tref(&ticket)),
            target,
        )
        .await;
    if parent_id.is_some() {
        let thread = state.db.thread(root_id).await?;
        let others: Vec<Id> = participants(&thread)
            .into_iter()
            .filter(|u| !mentioned_users.contains(u))
            .collect();
        state
            .notify(
                others,
                NotificationKind::Reply,
                &format!("{} replied on {}: {preview}", me.name, tref(&ticket)),
                target,
            )
            .await;
    }
    for (agent_id, text) in refusals {
        if let Err(e) = post_agent_reply(&state, ticket_id, root_id, agent_id, &text, None).await {
            tracing::warn!(error = ?e, "posting agent refusal");
        }
    }
    broadcast_ticket(&state, Some(ticket_id)).await;
    Ok(Json(state.db.comment(comment.id).await?.unwrap_or(comment)))
}

pub async fn update(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpdateComment>,
) -> AppResult<Json<Comment>> {
    let me = me.require(Permission::View)?.clone();
    let mut c = state
        .db
        .comment(id)
        .await?
        .ok_or(AppError::NotFound("comment"))?;
    if c.deleted {
        return Err(AppError::conflict("this comment was deleted"));
    }
    let mut newly_mentioned: Vec<Id> = Vec::new();
    if let Some(body) = req.body {
        if c.author_kind != CommentAuthorKind::User || c.author_id != me.id {
            return Err(AppError::forbidden("only the author can edit a comment"));
        }
        let body = body.trim().to_string();
        if body.is_empty() || body.chars().count() > MAX_BODY {
            return Err(AppError::bad(format!(
                "comments must be 1 to {MAX_BODY} characters"
            )));
        }
        let users = state.db.users().await?;
        let agents = state.db.agents().await?;
        let m = parse_mentions(&body);
        let mentioned: Vec<Id> = m
            .users
            .into_iter()
            .filter(|u| users.iter().any(|x| x.id == *u))
            .collect();
        newly_mentioned = mentioned
            .iter()
            .copied()
            .filter(|u| !c.mentioned_user_ids.contains(u))
            .collect();
        c.mentioned_user_ids = mentioned;
        // Editing never starts agents (no surprise runs); new agent mentions are just text.
        c.mentioned_agent_ids = m
            .agents
            .into_iter()
            .filter(|a| agents.iter().any(|x| x.id == *a))
            .collect();
        c.body = body;
        c.edited_at = Some(Utc::now());
    }
    let mut resolved_change = None;
    if let Some(r) = req.resolved {
        if !me.role.grants(Permission::EditTickets) {
            return Err(AppError::forbidden("your role cannot resolve threads"));
        }
        if c.parent_id.is_some() {
            return Err(AppError::bad(
                "only the first comment of a thread can be resolved",
            ));
        }
        if c.resolved != r {
            c.resolved = r;
            c.resolved_by = r.then_some(me.id);
            resolved_change = Some(r);
        }
    }
    state.db.save_comment(&c).await?;
    state.upsert(Entity::Comment(c.clone()));
    if let Some(tid) = c.ticket_id {
        if let Some(ticket) = state.db.ticket(tid).await? {
            if let Some(r) = resolved_change {
                state
                    .record(
                        HistoryKind::CommentResolved,
                        Actor::User(me.id),
                        Refs::ticket(&ticket),
                        format!(
                            "{} {} a thread on {}",
                            me.name,
                            if r { "resolved" } else { "reopened" },
                            tref(&ticket)
                        ),
                        Some(json!({ "comment_id": c.id, "resolved": r })),
                    )
                    .await;
            }
            if !newly_mentioned.is_empty() {
                state
                    .notify(
                        newly_mentioned,
                        NotificationKind::Mention,
                        &format!(
                            "{} mentioned you on {}: {}",
                            me.name,
                            tref(&ticket),
                            short(&plain(&c.body), 120)
                        ),
                        Target {
                            ticket: Some(tid),
                            comment: Some(c.id),
                            actor_user: Some(me.id),
                            ..Default::default()
                        },
                    )
                    .await;
            }
        }
    }
    broadcast_ticket(&state, c.ticket_id).await;
    Ok(Json(c))
}

pub async fn delete(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
) -> AppResult<StatusCode> {
    let me = me.require(Permission::View)?.clone();
    let mut c = state
        .db
        .comment(id)
        .await?
        .ok_or(AppError::NotFound("comment"))?;
    let is_author = c.author_kind == CommentAuthorKind::User && c.author_id == me.id;
    if !is_author && me.role < Role::Admin {
        return Err(AppError::forbidden(
            "only the author or an admin can delete a comment",
        ));
    }
    if !c.deleted {
        c.deleted = true;
        c.body = String::new();
        c.mentioned_user_ids.clear();
        c.mentioned_agent_ids.clear();
        c.reactions.clear();
        c.edited_at = Some(Utc::now());
        state.db.save_comment(&c).await?;
        state.upsert(Entity::Comment(c.clone()));
        broadcast_ticket(&state, c.ticket_id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn react(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<ReactRequest>,
) -> AppResult<Json<Comment>> {
    let me = me.require(Permission::View)?.clone();
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty()
        || emoji.chars().count() > 8
        || emoji
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_ascii_alphanumeric())
    {
        return Err(AppError::bad("a reaction is a single emoji"));
    }
    let _g = state.orch_lock.lock().await; // toggles must not race each other
    let mut c = state
        .db
        .comment(id)
        .await?
        .ok_or(AppError::NotFound("comment"))?;
    if c.deleted {
        return Err(AppError::conflict("this comment was deleted"));
    }
    match c.reactions.iter_mut().find(|r| r.emoji == emoji) {
        Some(r) => {
            if let Some(i) = r.user_ids.iter().position(|u| *u == me.id) {
                r.user_ids.remove(i);
            } else {
                r.user_ids.push(me.id);
            }
        }
        None => {
            if c.reactions.len() >= 20 {
                return Err(AppError::bad(
                    "too many different reactions on this comment",
                ));
            }
            c.reactions.push(Reaction {
                emoji,
                user_ids: vec![me.id],
            });
        }
    }
    c.reactions.retain(|r| !r.user_ids.is_empty());
    state.db.save_comment(&c).await?;
    state.upsert(Entity::Comment(c.clone()));
    Ok(Json(c))
}

// ---------------------------------------------------------------- agent replies

/// An agent posts in a thread (its answer when a mention run finishes, or why it can't).
pub async fn post_agent_reply(
    state: &SharedState,
    ticket_id: Id,
    root_id: Id,
    agent_id: Id,
    text: &str,
    run_id: Option<Id>,
) -> anyhow::Result<Comment> {
    let root = state.db.comment(root_id).await?;
    let anchor = root
        .as_ref()
        .map(|r| r.anchor.clone())
        .unwrap_or(CommentAnchor::Ticket);
    let body: String = text.trim().chars().take(MAX_BODY).collect();
    let c = Comment {
        id: new_id(),
        ticket_id: Some(ticket_id),
        job_id: None,
        anchor,
        parent_id: root.as_ref().map(|r| r.id),
        author_kind: CommentAuthorKind::Agent,
        author_id: agent_id,
        body: if body.is_empty() {
            "Done.".into()
        } else {
            body
        },
        mentioned_user_ids: Vec::new(),
        mentioned_agent_ids: Vec::new(),
        triggered_run_ids: run_id.into_iter().collect(),
        reactions: Vec::new(),
        resolved: false,
        resolved_by: None,
        created_at: Utc::now(),
        edited_at: None,
        deleted: false,
    };
    state.db.save_comment(&c).await?;
    state.upsert(Entity::Comment(c.clone()));
    if let (Some(ticket), Some(_)) = (state.db.ticket(ticket_id).await?, root) {
        let thread = state.db.thread(root_id).await?;
        let users = state.db.users().await?;
        let agents = state.db.agents().await?;
        let name = author_name(&users, &agents, &c);
        state
            .notify(
                participants(&thread),
                NotificationKind::Reply,
                &format!(
                    "{name} replied on {}: {}",
                    tref(&ticket),
                    short(&c.body, 120)
                ),
                Target {
                    ticket: Some(ticket_id),
                    comment: Some(c.id),
                    run: run_id,
                    actor_agent: Some(agent_id),
                    ..Default::default()
                },
            )
            .await;
    }
    broadcast_ticket(state, Some(ticket_id)).await;
    Ok(c)
}

/// Called when a run finishes: if an @mention started it, the agent answers in the thread.
pub async fn reply_for_finished_run(state: &SharedState, run: &Run) {
    let Ok(Some(root_id)) = state.db.run_reply_comment(run.id).await else {
        return;
    };
    let Some(ticket_id) = run.ticket_id else {
        return;
    };
    let text = match run.status {
        RunStatus::Succeeded => run
            .summary
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Done.".into()),
        RunStatus::Cancelled => "I stopped: the run was cancelled.".into(),
        _ => format!(
            "I couldn't finish: {}",
            run.error
                .clone()
                .unwrap_or_else(|| "something went wrong".into())
        ),
    };
    if let Err(e) =
        post_agent_reply(state, ticket_id, root_id, run.agent_id, &text, Some(run.id)).await
    {
        tracing::warn!(error = ?e, run_id = %run.id, "posting agent reply");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_parsed_and_rendered() {
        let u = Id::now_v7();
        let a = Id::now_v7();
        let body = format!("hey @[Mia](user:{u}) and @[Boba Dev](agent:{a}), also @[Mia](user:{u}) @[x](bogus:1) @[ broken");
        assert_eq!(
            parse_mentions(&body),
            Mentions {
                users: vec![u],
                agents: vec![a]
            }
        );
        assert_eq!(
            plain(&body),
            "hey @Mia and @Boba Dev, also @Mia @[x](bogus:1) @[ broken"
        );
    }
}

//! Profile and notifications (comments live in `comments.rs`).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use boba_core::api::{MarkRead, UpdateMe};
use boba_core::event::{Entity, HistoryKind, ServerMessage};
use boba_core::team::{Notification, USER_AVATARS};
use boba_core::User;
use serde::Deserialize;

use crate::auth::{self, SessionAuth};
use crate::error::{AppError, AppResult};
use crate::state::{Actor, Refs, SharedState};

pub async fn update_me(
    State(state): State<SharedState>,
    me: SessionAuth,
    Json(req): Json<UpdateMe>,
) -> AppResult<Json<User>> {
    let SessionAuth { user, token_hash } = me;
    let mut name = user.name.clone();
    let mut avatar = user.avatar.clone();
    if let Some(n) = &req.name {
        let n = n.trim();
        if n.is_empty() || n.chars().count() > 80 {
            return Err(AppError::bad("name must be 1 to 80 characters"));
        }
        name = n.to_string();
    }
    if let Some(a) = &req.avatar {
        if !USER_AVATARS.contains(&a.as_str()) {
            return Err(AppError::bad("pick one of the boba portraits"));
        }
        avatar = a.clone();
    }
    if let Some(new) = &req.new_password {
        let current = req
            .current_password
            .as_deref()
            .ok_or_else(|| AppError::bad("enter your current password to set a new one"))?;
        let hash = state.db.password_hash(user.id).await?.unwrap_or_default();
        if !auth::verify_password(current, &hash) {
            return Err(AppError::bad("your current password is not correct"));
        }
        auth::validate_password(new)?;
        state
            .db
            .set_password(user.id, &auth::hash_password(new)?)
            .await?;
        // Every other device is signed out; this one stays.
        for h in state.db.delete_other_sessions(user.id, &token_hash).await? {
            state.hub.revoke_session(&h);
        }
    }
    let profile_changed = name != user.name || avatar != user.avatar;
    if profile_changed {
        state.db.update_profile(user.id, &name, &avatar).await?;
    }
    let updated = state
        .db
        .user(user.id)
        .await?
        .ok_or(AppError::NotFound("user"))?;
    if profile_changed {
        state.upsert(Entity::User(updated.clone()));
        state
            .record(
                HistoryKind::ProfileUpdated,
                Actor::User(user.id),
                Refs::default(),
                if name != user.name {
                    format!("{} is now called {}", user.name, name)
                } else {
                    format!("{} has a new look", name)
                },
                None,
            )
            .await;
    }
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
pub struct NotificationQuery {
    before: Option<String>,
    limit: Option<u32>,
}

pub async fn list_notifications(
    State(state): State<SharedState>,
    me: SessionAuth,
    Query(q): Query<NotificationQuery>,
) -> AppResult<Json<Vec<Notification>>> {
    Ok(Json(
        state
            .db
            .notifications(me.user.id, q.before.as_deref(), q.limit.unwrap_or(50))
            .await?,
    ))
}

pub async fn mark_read(
    State(state): State<SharedState>,
    me: SessionAuth,
    Json(req): Json<MarkRead>,
) -> AppResult<StatusCode> {
    let changed = state.db.mark_read(me.user.id, req.ids.as_deref()).await?;
    // Other open tabs of the same user update their bell too.
    for id in changed.into_iter().take(500) {
        if let Some(n) = state.db.notification(id).await? {
            state.hub.send_to_user(
                me.user.id,
                ServerMessage::Upsert {
                    item: Entity::Notification(n),
                },
            );
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

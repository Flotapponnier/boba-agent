use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use boba_core::api::*;
use boba_core::event::{Entity, HistoryKind};
use boba_core::team::{NotificationKind, USER_AVATARS};
use boba_core::*;
use chrono::Utc;

use crate::auth::{self, Auth};
use crate::db::new_id;
use crate::error::{AppError, AppResult};
use crate::seed;
use crate::state::{Actor, Refs, SharedState};

pub async fn status(
    State(state): State<SharedState>,
    jar: CookieJar,
) -> AppResult<Json<AuthStatus>> {
    Ok(Json(AuthStatus {
        setup_done: state.db.user_count().await? > 0,
        me: auth::current_user(&state, &jar).await?,
    }))
}

fn clean_email(email: &str) -> AppResult<String> {
    let e = email.trim().to_lowercase();
    let valid = e.len() <= 254
        && e.split_once('@')
            .map(|(a, d)| {
                !a.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
            })
            .unwrap_or(false);
    if !valid {
        return Err(AppError::bad("please enter a valid email address"));
    }
    Ok(e)
}

fn clean_name(name: &str) -> AppResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 80 {
        return Err(AppError::bad("name must be 1 to 80 characters"));
    }
    Ok(n.to_string())
}

/// Sprite used as avatar, picked deterministically from the email.
/// Humans get a random square portrait (agents use the full-body sprites).
fn avatar_for(_email: &str) -> String {
    use rand::seq::SliceRandom;
    USER_AVATARS
        .choose(&mut rand::thread_rng())
        .unwrap_or(&"boba_avatar")
        .to_string()
}

pub async fn setup(
    State(state): State<SharedState>,
    jar: CookieJar,
    Json(req): Json<SetupRequest>,
) -> AppResult<(CookieJar, Json<User>)> {
    let _g = state.orch_lock.lock().await;
    if state.db.user_count().await? > 0 {
        return Err(AppError::conflict("this workspace is already set up"));
    }
    let email = clean_email(&req.email)?;
    let name = clean_name(&req.name)?;
    auth::validate_password(&req.password)?;
    let hash = auth::hash_password(&req.password)?;
    let user = User {
        id: new_id(),
        name,
        avatar: avatar_for(&email),
        email,
        role: Role::Owner,
        created_at: Utc::now(),
    };
    state.db.insert_user(&user, Some(&hash), None).await?;

    let mut settings = state.db.workspace_settings(state.cfg.demo_mode).await?;
    let ws = req.workspace_name.trim();
    if !ws.is_empty() {
        settings.name = ws.chars().take(80).collect();
    }
    state.db.save_workspace_settings(&settings).await?;
    if state.db.tab_count().await? == 0 {
        seed::seed_demo(&state, user.id).await?;
    }
    drop(_g);
    state
        .record(
            HistoryKind::UserJoined,
            Actor::User(user.id),
            Refs::default(),
            format!("{} opened the {} tea shop", user.name, settings.name),
            None,
        )
        .await;
    let jar = auth::start_session(&state, jar, &user).await?;
    Ok((jar, Json(user)))
}

pub async fn login(
    State(state): State<SharedState>,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> AppResult<(CookieJar, Json<User>)> {
    let bad = || AppError::bad("wrong email or password");
    let Some((user, hash)) = state
        .db
        .user_by_email(&req.email.trim().to_lowercase())
        .await?
    else {
        // Spend comparable time so emails cannot be probed by timing.
        let _ = auth::hash_password(&req.password);
        return Err(bad());
    };
    // A pending invite has no password: same answer as a wrong password, so the
    // endpoint does not reveal who was invited.
    let Some(hash) = hash else {
        let _ = auth::hash_password(&req.password);
        return Err(bad());
    };
    if !auth::verify_password(&req.password, &hash) {
        return Err(bad());
    }
    let jar = auth::start_session(&state, jar, &user).await?;
    Ok((jar, Json(user)))
}

pub async fn logout(
    State(state): State<SharedState>,
    jar: CookieJar,
) -> AppResult<(CookieJar, StatusCode)> {
    if let Some(c) = jar.get(auth::COOKIE) {
        let hash = auth::hash_token(c.value());
        state.db.delete_session(&hash).await?;
        state.hub.revoke_session(&hash);
    }
    Ok((jar.remove(auth::clear_cookie()), StatusCode::NO_CONTENT))
}

pub async fn invite(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<InviteRequest>,
) -> AppResult<Json<InviteResponse>> {
    let me = me.require(Permission::ManageUsers)?.clone();
    if req.role == Role::Owner {
        return Err(AppError::bad(
            "there is only one owner; invite as admin instead",
        ));
    }
    if req.role == Role::Admin && me.role != Role::Owner {
        return Err(AppError::forbidden("only the owner can invite admins"));
    }
    let email = clean_email(&req.email)?;
    let name = clean_name(&req.name)?;
    if state.db.email_taken(&email).await? {
        return Err(AppError::conflict(
            "someone with this email is already in the workspace",
        ));
    }
    let token = auth::new_token();
    let user = User {
        id: new_id(),
        name,
        avatar: avatar_for(&email),
        email,
        role: req.role,
        created_at: Utc::now(),
    };
    state
        .db
        .insert_user(&user, None, Some(&auth::hash_token(&token)))
        .await?;
    state.upsert(Entity::User(user.clone()));
    Ok(Json(InviteResponse {
        invite_url: format!("{}/invite/{token}", state.cfg.public_url),
        user,
    }))
}

pub async fn accept_invite(
    State(state): State<SharedState>,
    jar: CookieJar,
    Json(req): Json<AcceptInvite>,
) -> AppResult<(CookieJar, Json<User>)> {
    auth::validate_password(&req.password)?;
    let hash = auth::hash_password(&req.password)?;
    let user = state
        .db
        .accept_invite(&auth::hash_token(req.token.trim()), &hash)
        .await?
        .ok_or_else(|| AppError::bad("this invite link is invalid, expired or already used"))?;
    state
        .record(
            HistoryKind::UserJoined,
            Actor::User(user.id),
            Refs::default(),
            format!("{} joined the team", user.name),
            None,
        )
        .await;
    let jar = auth::start_session(&state, jar, &user).await?;
    Ok((jar, Json(user)))
}

pub async fn update_user(
    State(state): State<SharedState>,
    me: Auth,
    Path(id): Path<Id>,
    Json(req): Json<UpdateUser>,
) -> AppResult<Json<User>> {
    let me = me.require(Permission::ManageUsers)?.clone();
    let target = state.db.user(id).await?.ok_or(AppError::NotFound("user"))?;
    if target.id == me.id {
        return Err(AppError::bad("you cannot change your own role"));
    }
    if target.role == Role::Owner || req.role == Role::Owner {
        return Err(AppError::forbidden("ownership cannot be changed here"));
    }
    if me.role != Role::Owner && (target.role == Role::Admin || req.role == Role::Admin) {
        return Err(AppError::forbidden(
            "only the owner can promote or demote admins",
        ));
    }
    state.db.set_role(id, req.role).await?;
    if target.role != req.role {
        state
            .notify(
                [id],
                NotificationKind::Account,
                &format!(
                    "{} changed your role to {}",
                    me.name,
                    crate::db::enc(&req.role)
                ),
                crate::notify::Target {
                    actor_user: Some(me.id),
                    ..Default::default()
                },
            )
            .await;
    }
    let user = state.db.user(id).await?.ok_or(AppError::NotFound("user"))?;
    state.upsert(Entity::User(user.clone()));
    Ok(Json(user))
}

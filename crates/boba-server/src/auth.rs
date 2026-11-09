//! Passwords, sessions and the authenticated-user extractor.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use boba_core::{Permission, User};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::state::SharedState;

pub const COOKIE: &str = "boba_session";
pub const SESSION_DAYS: i64 = 30;

pub fn hash_password(password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hashing password: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

pub fn validate_password(password: &str) -> AppResult<()> {
    if password.chars().count() < 8 {
        return Err(AppError::bad("password must be at least 8 characters"));
    }
    Ok(())
}

/// Random URL-safe token (hex, 256 bits).
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Tokens are stored hashed: a leaked database does not leak live sessions.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .path("/")
        .max_age(time_days(SESSION_DAYS))
        .build()
}

pub fn clear_cookie() -> Cookie<'static> {
    Cookie::build((COOKIE, "")).path("/").build()
}

fn time_days(days: i64) -> time::Duration {
    time::Duration::days(days)
}

/// Create a session for `user` and return the cookie jar to send back.
pub async fn start_session(
    state: &SharedState,
    jar: CookieJar,
    user: &User,
) -> AppResult<CookieJar> {
    let token = new_token();
    let expires = chrono::Utc::now() + chrono::Duration::days(SESSION_DAYS);
    state
        .db
        .insert_session(&hash_token(&token), user.id, expires)
        .await?;
    Ok(jar.add(session_cookie(token, state.cfg.secure_cookies)))
}

/// Resolve the session cookie to a user, if any.
pub async fn current_user(state: &SharedState, jar: &CookieJar) -> AppResult<Option<User>> {
    let Some(c) = jar.get(COOKIE) else {
        return Ok(None);
    };
    if c.value().is_empty() {
        return Ok(None);
    }
    Ok(state.db.session_user(&hash_token(c.value())).await?)
}

/// Extractor: a signed-in user (401 otherwise).
pub struct Auth(pub User);

impl Auth {
    /// 403 unless the user's role grants `p`.
    pub fn require(&self, p: Permission) -> AppResult<&User> {
        if self.0.role.grants(p) {
            Ok(&self.0)
        } else {
            Err(AppError::forbidden(format!(
                "your role ({}) cannot {}",
                crate::db::enc(&self.0.role),
                permission_phrase(p)
            )))
        }
    }
}

fn permission_phrase(p: Permission) -> &'static str {
    match p {
        Permission::View => "view this workspace",
        Permission::EditTickets => "edit tickets",
        Permission::RunAgents => "run agents",
        Permission::ManageAgents => "manage agents",
        Permission::ManageSkills => "manage skills",
        Permission::ManageWorkspace => "manage the workspace",
        Permission::ManageUsers => "manage users",
    }
}

impl FromRequestParts<SharedState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        match current_user(state, &jar).await? {
            Some(u) => Ok(Auth(u)),
            None => Err(AppError::Unauthorized),
        }
    }
}

//! The "Boba Agent" GitHub App: created with GitHub's manifest flow (two clicks), then
//! used for everything GitHub: repo sync, comments and reviews, and short-lived push tokens
//! for agents. No personal token has to live on the server.
//!
//! Credentials (private key, webhook secret, client secret) are stored encrypted with the
//! connectors key. Installation tokens are minted from a short JWT and cached.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use boba_core::api::{GithubAppStatus, GithubInstallation, GithubRepo};
use boba_core::runner::GitAuthor;
use boba_core::Permission;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::Auth;
use crate::connectors::{decrypt, encrypt};
use crate::error::{AppError, AppResult};
use crate::state::SharedState;

const SETTING_APP: &str = "github_app";
const SETTING_INSTALLATIONS: &str = "github_installations";
const STATE_TTL: Duration = Duration::from_secs(3600);

/// What we keep about the app (secrets encrypted).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredApp {
    app_id: i64,
    slug: String,
    name: String,
    client_id: String,
    html_url: String,
    pem_enc: String,
    webhook_secret_enc: String,
    client_secret_enc: String,
}

/// Decrypted, ready to sign with.
#[derive(Clone)]
pub struct AppCreds {
    pub app_id: i64,
    pub slug: String,
    pub name: String,
    pub html_url: String,
    pem: String,
    pub webhook_secret: String,
}

/// In-memory: manifest `state` values and cached installation tokens.
#[derive(Default)]
pub struct Runtime {
    states: Mutex<HashMap<String, Instant>>,
    tokens: Mutex<HashMap<i64, (String, DateTime<Utc>)>>,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("boba-agent/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

fn api(state: &SharedState, path: &str) -> String {
    format!("{}{path}", state.cfg.github_api.trim_end_matches('/'))
}

pub async fn creds(state: &SharedState) -> anyhow::Result<Option<AppCreds>> {
    let Some(raw) = state.db.setting(SETTING_APP).await? else {
        return Ok(None);
    };
    let s: StoredApp = serde_json::from_str(&raw)?;
    Ok(Some(AppCreds {
        app_id: s.app_id,
        slug: s.slug,
        name: s.name,
        html_url: s.html_url,
        pem: decrypt(&s.pem_enc)?,
        webhook_secret: decrypt(&s.webhook_secret_enc)?,
    }))
}

/// The webhook secret: the app's, else `BOBA_GITHUB_WEBHOOK_SECRET`.
pub async fn webhook_secret(state: &SharedState) -> Option<String> {
    match creds(state).await {
        Ok(Some(c)) => Some(c.webhook_secret),
        _ => state.cfg.github_webhook_secret.clone(),
    }
}

/// Commit author for repos driven by the app.
pub fn git_author(c: &AppCreds) -> GitAuthor {
    GitAuthor {
        name: c.name.clone(),
        email: format!("{}+{}[bot]@users.noreply.github.com", c.app_id, c.slug),
    }
}

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}

/// App JWT (RS256): backdated 60 s for clock skew, valid 9 minutes.
pub fn jwt(c: &AppCreds) -> anyhow::Result<String> {
    let now = Utc::now().timestamp();
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(c.pem.as_bytes())?;
    Ok(jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &Claims {
            iat: now - 60,
            exp: now + 9 * 60,
            iss: c.app_id.to_string(),
        },
        &key,
    )?)
}

async fn app_get(state: &SharedState, c: &AppCreds, path: &str) -> anyhow::Result<Value> {
    let res = http()
        .get(api(state, path))
        .bearer_auth(jwt(c)?)
        .header(header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    anyhow::ensure!(status.is_success(), "GitHub {status}: {}", v["message"]);
    Ok(v)
}

/// An installation access token, cached until 5 minutes before it expires.
pub async fn installation_token(state: &SharedState, installation: i64) -> anyhow::Result<String> {
    if let Some((t, exp)) = state
        .gh_app
        .tokens
        .lock()
        .ok()
        .and_then(|m| m.get(&installation).cloned())
    {
        if exp - Utc::now() > chrono::Duration::minutes(5) {
            return Ok(t);
        }
    }
    let c = creds(state)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the GitHub App is not set up"))?;
    let res = http()
        .post(api(
            state,
            &format!("/app/installations/{installation}/access_tokens"),
        ))
        .bearer_auth(jwt(&c)?)
        .header(header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    anyhow::ensure!(status.is_success(), "GitHub {status}: {}", v["message"]);
    let token = v["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no token in GitHub's answer"))?
        .to_string();
    let exp = v["expires_at"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() + chrono::Duration::minutes(55));
    if let Ok(mut m) = state.gh_app.tokens.lock() {
        m.insert(installation, (token.clone(), exp));
    }
    Ok(token)
}

async fn installation_repos(state: &SharedState, installation: i64) -> anyhow::Result<Vec<Value>> {
    let token = installation_token(state, installation).await?;
    let mut out = Vec::new();
    for page in 1..=10 {
        let res = http()
            .get(api(
                state,
                &format!("/installation/repositories?per_page=100&page={page}"),
            ))
            .bearer_auth(&token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        anyhow::ensure!(status.is_success(), "GitHub {status}: {}", v["message"]);
        let repos = v["repositories"].as_array().cloned().unwrap_or_default();
        let n = repos.len();
        out.extend(repos);
        if n < 100 {
            break;
        }
    }
    Ok(out)
}

/// Re-read where the app is installed (and on which repos); stored for routing tokens.
pub async fn refresh_installations(state: &SharedState) -> anyhow::Result<Vec<GithubInstallation>> {
    let Some(c) = creds(state).await? else {
        return Ok(Vec::new());
    };
    let list = app_get(state, &c, "/app/installations?per_page=100").await?;
    let mut out = Vec::new();
    for inst in list.as_array().cloned().unwrap_or_default() {
        let Some(id) = inst["id"].as_i64() else {
            continue;
        };
        let account = inst["account"]["login"].as_str().unwrap_or("?").to_string();
        let repos = match installation_repos(state, id).await {
            Ok(r) => r
                .iter()
                .filter_map(|r| r["full_name"].as_str().map(str::to_string))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, installation = id, "listing installation repos");
                Vec::new()
            }
        };
        out.push(GithubInstallation { id, account, repos });
    }
    state
        .db
        .set_setting(SETTING_INSTALLATIONS, &serde_json::to_string(&out)?)
        .await?;
    Ok(out)
}

async fn stored_installations(state: &SharedState) -> Vec<GithubInstallation> {
    match state.db.setting(SETTING_INSTALLATIONS).await {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The installation covering `owner/name` (refreshes once if unknown).
pub async fn installation_for(state: &SharedState, full_name: &str) -> Option<i64> {
    let find = |list: &[GithubInstallation]| {
        list.iter()
            .find(|i| i.repos.iter().any(|r| r.eq_ignore_ascii_case(full_name)))
            .map(|i| i.id)
    };
    if let Some(id) = find(&stored_installations(state).await) {
        return Some(id);
    }
    creds(state).await.ok()??;
    find(&refresh_installations(state).await.ok()?)
}

/// A token able to act on `owner/name`, if the app is installed there.
pub async fn token_for_repo(state: &SharedState, full_name: &str) -> Option<String> {
    let inst = installation_for(state, full_name).await?;
    match installation_token(state, inst).await {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(error = %e, repo = full_name, "minting installation token");
            None
        }
    }
}

// ---------------------------------------------------------------- routes

#[derive(Debug, Deserialize)]
pub struct SetupQuery {
    /// Create the app under an organisation instead of the signed-in GitHub user.
    org: Option<String>,
    /// App name if "Boba Agent" is taken (GitHub app names are global).
    name: Option<String>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// What GitHub needs to create the app: where to post, and the manifest.
pub struct Prepared {
    pub name: String,
    pub action_url: String,
    pub manifest: Value,
    pub warnings: Vec<String>,
}

/// Build the manifest and mint a one-time `state` (shared by the HTML and JSON routes).
pub fn prepare(state: &SharedState, org: Option<&str>, name: Option<&str>) -> Prepared {
    let public = state.cfg.public_url.trim_end_matches('/').to_string();
    let name = name
        .map(|n| n.trim().chars().take(34).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Boba Agent".into());
    let manifest = json!({
        "name": name,
        "url": public,
        "hook_attributes": {"url": format!("{public}/api/github/webhook"), "active": true},
        "redirect_url": format!("{public}/api/github/app/callback"),
        "setup_url": format!("{public}/settings/integrations"),
        "setup_on_update": true,
        "public": false,
        "default_permissions": {
            "contents": "write", "issues": "write", "pull_requests": "write",
            "checks": "write", "metadata": "read"
        },
        "default_events": ["issues", "issue_comment", "pull_request", "pull_request_review", "push"]
    });
    let token = crate::auth::new_token();
    if let Ok(mut m) = state.gh_app.states.lock() {
        m.retain(|_, t| t.elapsed() < STATE_TTL);
        m.insert(token.clone(), Instant::now());
    }
    let web = state.cfg.github_web.trim_end_matches('/');
    let action_url = match org.map(str::trim).filter(|o| !o.is_empty()) {
        Some(org) => format!(
            "{web}/organizations/{}/settings/apps/new?state={token}",
            urlencoding::encode(org)
        ),
        None => format!("{web}/settings/apps/new?state={token}"),
    };
    let mut warnings = Vec::new();
    if public.contains("localhost") || public.contains("127.0.0.1") {
        warnings.push(
            "BOBA_PUBLIC_URL is a local address: GitHub can create the app, but its webhooks will not reach this server."
                .to_string(),
        );
    } else if !public.starts_with("https://") {
        warnings.push(
            "BOBA_PUBLIC_URL is not https: GitHub will refuse to deliver webhooks to it."
                .to_string(),
        );
    }
    Prepared {
        name,
        action_url,
        manifest,
        warnings,
    }
}

/// `GET /api/github/app/manifest` [ManageAutomations]: the same as the setup page, as data,
/// for the SPA to render its own page and post `manifest` to `action_url`.
pub async fn manifest(
    State(state): State<SharedState>,
    me: Auth,
    Query(q): Query<SetupQuery>,
) -> AppResult<Json<Value>> {
    me.require(Permission::ManageAutomations)?;
    let p = prepare(&state, q.org.as_deref(), q.name.as_deref());
    Ok(Json(json!({
        "name": p.name,
        "action_url": p.action_url,
        "manifest": p.manifest,
        "warnings": p.warnings,
    })))
}

/// `GET /api/github/app/setup` [ManageAutomations]: a page that posts the app manifest to
/// GitHub. Florent clicks "Create GitHub App", then "Install". (The SPA renders its own page
/// from `/api/github/app/manifest`; this one stays for direct links.)
pub async fn setup(
    State(state): State<SharedState>,
    me: Auth,
    Query(q): Query<SetupQuery>,
) -> AppResult<Response> {
    me.require(Permission::ManageAutomations)?;
    let Prepared {
        name,
        action_url: action,
        manifest,
        warnings,
    } = prepare(&state, q.org.as_deref(), q.name.as_deref());
    let warning: String = warnings
        .iter()
        .map(|w| format!("<p class=\"warn\">{}</p>", esc(w)))
        .collect();
    let page = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Connect GitHub</title>\
<style>body{{font-family:system-ui;max-width:36rem;margin:4rem auto;padding:0 1rem;color:#3b2a1a}}\
button{{font-size:1rem;padding:.7rem 1.2rem;border-radius:.6rem;border:0;background:#c98b5a;color:#fff;cursor:pointer}}\
.warn{{background:#fff3cd;padding:.6rem;border-radius:.4rem}}</style></head><body>\
<h1>Connect GitHub</h1><p>This creates a private GitHub App named <b>{}</b> for this Boba Agent server. \
GitHub will ask you to confirm, then to pick the repositories it may access.</p>{warning}\
<form id=\"f\" method=\"post\" action=\"{}\"><input type=\"hidden\" name=\"manifest\" value=\"{}\">\
<button type=\"submit\">Create the GitHub App</button></form></body></html>",
        esc(&name),
        esc(&action),
        esc(&manifest.to_string())
    );
    Ok((
        [(header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'; form-action https://github.com https://*.github.com http://127.0.0.1:* http://localhost:*")],
        Html(page),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

/// `GET /api/github/app/callback`: GitHub sends the browser back with a one-time `code`.
pub async fn callback(
    State(state): State<SharedState>,
    Query(q): Query<CallbackQuery>,
) -> AppResult<Response> {
    let valid = q.state.as_ref().is_some_and(|s| {
        state
            .gh_app
            .states
            .lock()
            .ok()
            .and_then(|mut m| m.remove(s))
            .is_some_and(|t| t.elapsed() < STATE_TTL)
    });
    if !valid {
        return Err(AppError::bad(
            "this GitHub setup link expired: start again from Settings → Integrations",
        ));
    }
    let code = q
        .code
        .filter(|c| !c.is_empty())
        .ok_or_else(|| AppError::bad("GitHub did not send a code"))?;
    let res = http()
        .post(api(
            &state,
            &format!("/app-manifests/{}/conversions", urlencoding::encode(&code)),
        ))
        .header(header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| AppError::bad(format!("could not reach GitHub: {e}")))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(AppError::bad(format!(
            "GitHub refused the app: {}",
            v["message"]
        )));
    }
    let field = |k: &str| {
        v[k].as_str()
            .map(str::to_string)
            .ok_or_else(|| AppError::bad(format!("GitHub's answer has no {k}")))
    };
    let stored = StoredApp {
        app_id: v["id"]
            .as_i64()
            .ok_or_else(|| AppError::bad("GitHub's answer has no id"))?,
        slug: field("slug")?,
        name: field("name")?,
        client_id: field("client_id")?,
        html_url: field("html_url")?,
        pem_enc: encrypt(&field("pem")?)?,
        webhook_secret_enc: encrypt(&field("webhook_secret")?)?,
        client_secret_enc: encrypt(&field("client_secret")?)?,
    };
    state
        .db
        .set_setting(SETTING_APP, &serde_json::to_string(&stored)?)
        .await?;
    tracing::info!(slug = %stored.slug, app_id = stored.app_id, "GitHub App created");
    let web = state.cfg.github_web.trim_end_matches('/');
    Ok(Redirect::to(&format!("{web}/apps/{}/installations/new", stored.slug)).into_response())
}

/// `GET /api/github/app` [View]
pub async fn status(
    State(state): State<SharedState>,
    me: Auth,
) -> AppResult<Json<GithubAppStatus>> {
    me.require(Permission::View)?;
    let Some(c) = creds(&state).await? else {
        return Ok(Json(GithubAppStatus::default()));
    };
    let installations = match refresh_installations(&state).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "refreshing GitHub installations");
            stored_installations(&state).await
        }
    };
    let web = state.cfg.github_web.trim_end_matches('/');
    Ok(Json(GithubAppStatus {
        configured: true,
        install_url: Some(format!("{web}/apps/{}/installations/new", c.slug)),
        slug: Some(c.slug),
        app_id: Some(c.app_id),
        html_url: Some(c.html_url),
        installations,
    }))
}

/// `GET /api/github/repos` [ManageWorkspace]: the repo picker.
pub async fn repos(State(state): State<SharedState>, me: Auth) -> AppResult<Json<Vec<GithubRepo>>> {
    me.require(Permission::ManageWorkspace)?;
    if creds(&state).await?.is_none() {
        return Err(AppError::bad(
            "connect GitHub first (Settings → Integrations)",
        ));
    }
    let board = state.db.repos().await?;
    let installations = refresh_installations(&state)
        .await
        .map_err(|e| AppError::bad(e.to_string()))?;
    let mut out = Vec::new();
    for inst in installations {
        for r in installation_repos(&state, inst.id)
            .await
            .map_err(|e| AppError::bad(e.to_string()))?
        {
            let Some(full) = r["full_name"].as_str() else {
                continue;
            };
            let repo_id = board
                .iter()
                .find(|b| {
                    b.remote_url
                        .as_deref()
                        .and_then(boba_github::RepoRef::parse)
                        .is_some_and(|x| x.full_name().eq_ignore_ascii_case(full))
                })
                .map(|b| b.id);
            out.push(GithubRepo {
                full_name: full.to_string(),
                default_branch: r["default_branch"].as_str().unwrap_or("main").to_string(),
                private: r["private"].as_bool().unwrap_or(true),
                html_url: r["html_url"].as_str().unwrap_or_default().to_string(),
                installation_id: inst.id,
                repo_id,
            });
        }
    }
    out.sort_by_key(|r| r.full_name.to_lowercase());
    Ok(Json(out))
}

/// Clone `owner/name` with an installation token into `BOBA_REPOS_DIR/<owner>-<name>`
/// (or reuse an existing checkout). The token is passed as a one-off header, never written
/// into the repo's config.
pub async fn clone_repo(state: &SharedState, full_name: &str) -> AppResult<std::path::PathBuf> {
    let token = token_for_repo(state, full_name)
        .await
        .ok_or_else(|| AppError::bad(format!("the GitHub App is not installed on {full_name}")))?;
    let dir = state.cfg.repos_dir.join(full_name.replace('/', "-"));
    if dir.join(".git").exists() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&state.cfg.repos_dir).map_err(|e| AppError::Internal(e.into()))?;
    let web = state.cfg.github_web.trim_end_matches('/');
    let url = format!("{web}/{full_name}.git");
    let basic = base64_basic(&format!("x-access-token:{token}"));
    let out = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::process::Command::new("git")
            .arg("clone")
            .arg("--quiet")
            .arg(&url)
            .arg(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Basic {basic}"),
            )
            .output(),
    )
    .await
    .map_err(|_| AppError::bad("cloning took longer than 10 minutes"))?
    .map_err(|e| AppError::bad(format!("could not run git: {e}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        let err = String::from_utf8_lossy(&out.stderr).replace(&token, "***");
        return Err(AppError::bad(format!("git clone failed: {}", err.trim())));
    }
    Ok(dir)
}

fn base64_basic(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Webhook side: installation changes → refresh the stored list.
pub fn on_installation_event(state: &SharedState) {
    let s = state.clone();
    tokio::spawn(async move {
        if let Ok(mut t) = s.gh_app.tokens.lock() {
            t.clear();
        }
        if let Err(e) = refresh_installations(&s).await {
            tracing::warn!(error = %e, "refreshing installations after webhook");
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn basic_auth_encoding() {
        assert_eq!(
            super::base64_basic("x-access-token:abc"),
            "eC1hY2Nlc3MtdG9rZW46YWJj"
        );
        assert_eq!(super::base64_basic("a"), "YQ==");
    }
}

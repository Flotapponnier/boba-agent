//! Connectors: MCP servers that agents can be given (Google Workspace, Linear, Sentry…).
//!
//! * Secret values are encrypted at rest with AES-256-GCM. The key comes from
//!   `BOBA_SECRET_KEY` (64 hex chars or base64 of 32 bytes) or is generated once into
//!   `<data>/secret.key` (mode 0600). Values are write-only for clients: the API only ever
//!   returns secret *names*.
//! * [`resolve_mcp_servers`] turns an agent's `connector_ids` into ready-to-run
//!   [`McpServerSpec`]s (secrets decrypted) for the orchestrator.
//! * `POST /api/connectors/:id/test` starts the MCP server (stdio or HTTP), runs the MCP
//!   handshake and lists its tools.
//!
//! Creating a stdio connector lets its author run a command on the runner machine, which
//! is why every route here requires `ManageAutomations` (Admin and up).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, bail, Context};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use boba_core::api::{ConnectorTestResult, CreateConnector, UpdateConnector};
use boba_core::automation::{Connector, ConnectorKind, McpTransport};
use boba_core::event::{Entity, EntityKind, HistoryKind};
use boba_core::runner::McpServerSpec;
use boba_core::{Agent, Id, Permission};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::auth::Auth;
use crate::db::{dec, enc, new_id, parse_ts, ts, Db};
use crate::error::{AppError, AppResult};
use crate::state::{Actor, Refs, SharedState};

/// Name reserved for the server's own board-tools MCP server.
pub const BOARD_SERVER_NAME: &str = "boba";
const TEST_TIMEOUT: Duration = Duration::from_secs(60);
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

// ================================================================ secret key

static KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Load (or create) the secret key. Call once at startup with the data dir; if it was
/// never called, the first use falls back to `BOBA_DATA_DIR` (default `./data`).
pub fn init_secret_key(data_dir: &Path) -> anyhow::Result<()> {
    if KEY.get().is_some() {
        return Ok(());
    }
    let key = load_or_create_key(data_dir)?;
    let _ = KEY.set(key);
    Ok(())
}

fn key() -> anyhow::Result<&'static [u8; 32]> {
    if let Some(k) = KEY.get() {
        return Ok(k);
    }
    let dir = std::env::var("BOBA_DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("data"));
    init_secret_key(&dir)?;
    KEY.get().ok_or_else(|| anyhow!("secret key unavailable"))
}

fn load_or_create_key(data_dir: &Path) -> anyhow::Result<[u8; 32]> {
    if let Ok(v) = std::env::var("BOBA_SECRET_KEY") {
        return parse_key(v.trim()).context("BOBA_SECRET_KEY must be 32 bytes as 64 hex chars or base64");
    }
    let path = data_dir.join("secret.key");
    if path.exists() {
        let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        return parse_key(raw.trim()).with_context(|| format!("{} is not a valid key", path.display()));
    }
    std::fs::create_dir_all(data_dir)?;
    let key: [u8; 32] = Aes256Gcm::generate_key(OsRng).into();
    write_private(&path, hex::encode(key).as_bytes())?;
    tracing::info!(path = %path.display(), "generated a new secret key for connector secrets; back it up");
    Ok(key)
}

fn parse_key(s: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        hex::decode(s)?
    } else {
        B64.decode(s)?
    };
    bytes.try_into().map_err(|_| anyhow!("key must be exactly 32 bytes"))
}

fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        f.write_all(data)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)?;
        Ok(())
    }
}

fn encrypt(plain: &str) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key()?));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plain.as_bytes()).map_err(|_| anyhow!("encryption failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(B64.encode(out))
}

fn decrypt(stored: &str) -> anyhow::Result<String> {
    let raw = B64.decode(stored)?;
    if raw.len() < 12 {
        bail!("corrupt secret");
    }
    let (nonce, ct) = raw.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key()?));
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow!("cannot decrypt a connector secret: was BOBA_SECRET_KEY / secret.key changed?"))?;
    Ok(String::from_utf8(plain)?)
}

// ================================================================ storage

fn row_to_connector(row: &SqliteRow, secret_keys: Vec<String>) -> anyhow::Result<Connector> {
    let created_by: Option<String> = row.try_get("created_by")?;
    Ok(Connector {
        id: Id::parse_str(row.try_get::<String, _>("id")?.as_str())?,
        name: row.try_get("name")?,
        kind: dec(row.try_get::<String, _>("kind")?.as_str())?,
        transport: dec(row.try_get::<String, _>("transport")?.as_str())?,
        secret_keys,
        allowed_tools: dec(row.try_get::<String, _>("allowed_tools")?.as_str())?,
        created_by: created_by.map(|s| Id::parse_str(&s)).transpose()?,
        created_at: parse_ts(row.try_get::<String, _>("created_at")?.as_str())?,
    })
}

async fn secret_keys(db: &Db, cid: Id) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT key FROM connector_secrets WHERE connector_id = ? ORDER BY key")
        .bind(cid.to_string())
        .fetch_all(&db.pool)
        .await?)
}

async fn secret_values(db: &Db, cid: Id) -> anyhow::Result<BTreeMap<String, String>> {
    let rows = sqlx::query("SELECT key, value_enc FROM connector_secrets WHERE connector_id = ?")
        .bind(cid.to_string())
        .fetch_all(&db.pool)
        .await?;
    let mut out = BTreeMap::new();
    for r in rows {
        let k: String = r.try_get("key")?;
        let v: String = r.try_get("value_enc")?;
        out.insert(k, decrypt(&v)?);
    }
    Ok(out)
}

/// All connectors (secret names only), for the snapshot.
pub async fn list(db: &Db) -> anyhow::Result<Vec<Connector>> {
    let rows = sqlx::query("SELECT * FROM connectors ORDER BY name").fetch_all(&db.pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id = Id::parse_str(r.try_get::<String, _>("id")?.as_str())?;
        out.push(row_to_connector(r, secret_keys(db, id).await?)?);
    }
    Ok(out)
}

pub async fn get(db: &Db, cid: Id) -> anyhow::Result<Option<Connector>> {
    let Some(r) = sqlx::query("SELECT * FROM connectors WHERE id = ?")
        .bind(cid.to_string())
        .fetch_optional(&db.pool)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(row_to_connector(&r, secret_keys(db, cid).await?)?))
}

async fn save(db: &Db, c: &Connector, secrets: Option<&BTreeMap<String, String>>) -> anyhow::Result<()> {
    let mut tx = db.pool.begin().await?;
    sqlx::query(
        "INSERT INTO connectors(id, name, kind, transport, allowed_tools, created_by, created_at)
         VALUES(?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, transport = excluded.transport,
           allowed_tools = excluded.allowed_tools",
    )
    .bind(c.id.to_string())
    .bind(&c.name)
    .bind(enc(&c.kind))
    .bind(enc(&c.transport))
    .bind(enc(&c.allowed_tools))
    .bind(c.created_by.map(|i| i.to_string()))
    .bind(ts(c.created_at))
    .execute(&mut *tx)
    .await?;
    if let Some(secrets) = secrets {
        for (k, v) in secrets {
            if v.is_empty() {
                sqlx::query("DELETE FROM connector_secrets WHERE connector_id = ? AND key = ?")
                    .bind(c.id.to_string())
                    .bind(k)
                    .execute(&mut *tx)
                    .await?;
            } else {
                sqlx::query(
                    "INSERT INTO connector_secrets(connector_id, key, value_enc) VALUES(?, ?, ?)
                     ON CONFLICT(connector_id, key) DO UPDATE SET value_enc = excluded.value_enc",
                )
                .bind(c.id.to_string())
                .bind(k)
                .bind(encrypt(v)?)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

// ================================================================ resolution for runs

/// MCP server name as seen by Claude Code (`mcp__<name>__<tool>`): lowercase `[a-z0-9_]`,
/// unique within the run, never the reserved board-tools name.
fn server_name(name: &str, taken: &mut HashSet<String>) -> String {
    let mut base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if base.is_empty() {
        base = "connector".into();
    }
    if base == BOARD_SERVER_NAME {
        base = format!("{BOARD_SERVER_NAME}_ext");
    }
    let mut candidate = base.clone();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    candidate
}

/// The agent's connectors as runnable MCP server specs, secrets decrypted.
/// Unknown connector ids (deleted connectors) are skipped with a warning.
pub async fn resolve_mcp_servers(db: &Db, agent: &Agent) -> anyhow::Result<Vec<McpServerSpec>> {
    let mut taken = HashSet::from([BOARD_SERVER_NAME.to_string()]);
    let mut out = Vec::new();
    for cid in &agent.connector_ids {
        let Some(c) = get(db, *cid).await? else {
            tracing::warn!(agent = %agent.id, connector = %cid, "agent references a deleted connector");
            continue;
        };
        out.push(McpServerSpec {
            name: server_name(&c.name, &mut taken),
            transport: c.transport.clone(),
            secrets: secret_values(db, c.id).await?,
            allowed_tools: c.allowed_tools.clone(),
        });
    }
    Ok(out)
}

// ================================================================ validation

fn valid_secret_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && k.len() <= 64
}

fn validate(name: &str, transport: &McpTransport, secrets: &BTreeMap<String, String>) -> AppResult<()> {
    if name.trim().is_empty() || name.len() > 60 {
        return Err(AppError::bad("connector name must be 1-60 characters"));
    }
    for k in secrets.keys() {
        if !valid_secret_key(k) {
            return Err(AppError::bad(format!("secret name {k:?} must look like an env var: UPPER_SNAKE_CASE")));
        }
    }
    match transport {
        McpTransport::Stdio { command, .. } => {
            if command.trim().is_empty() {
                return Err(AppError::bad("stdio connector needs a command"));
            }
        }
        McpTransport::Http { url, .. } => {
            let ok = url.starts_with("https://") || url.starts_with("http://");
            if !ok {
                return Err(AppError::bad("HTTP connector URL must start with http:// or https://"));
            }
        }
    }
    Ok(())
}

fn check_header_secrets(transport: &McpTransport, known: &HashSet<String>) -> AppResult<()> {
    if let McpTransport::Http { header_secrets, .. } = transport {
        for (h, k) in header_secrets {
            if !known.contains(k) {
                return Err(AppError::bad(format!("header {h} refers to unknown secret {k}")));
            }
        }
    }
    Ok(())
}

fn kind_label(k: ConnectorKind) -> String {
    enc(&k).trim_matches('"').replace('_', " ")
}

// ================================================================ routes

pub async fn create(
    State(state): State<SharedState>,
    me: Auth,
    Json(req): Json<CreateConnector>,
) -> AppResult<Json<Connector>> {
    let user = me.require(Permission::ManageAutomations)?.clone();
    let secrets: BTreeMap<String, String> = req.secrets.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    validate(&req.name, &req.transport, &secrets)?;
    check_header_secrets(&req.transport, &secrets.keys().cloned().collect())?;
    let mut c = Connector {
        id: new_id(),
        name: req.name.trim().to_string(),
        kind: req.kind,
        transport: req.transport,
        secret_keys: vec![],
        allowed_tools: req.allowed_tools.unwrap_or_default(),
        created_by: Some(user.id),
        created_at: Utc::now(),
    };
    save(&state.db, &c, Some(&secrets)).await?;
    c.secret_keys = secrets.keys().cloned().collect();
    state.upsert(Entity::Connector(c.clone()));
    state
        .record(
            HistoryKind::ConnectorCreated,
            Actor::User(user.id),
            Refs::default(),
            format!("{} added the {} connector \"{}\"", user.name, kind_label(c.kind), c.name),
            None,
        )
        .await;
    Ok(Json(c))
}

pub async fn update(
    State(state): State<SharedState>,
    me: Auth,
    UrlPath(id): UrlPath<Id>,
    Json(req): Json<UpdateConnector>,
) -> AppResult<Json<Connector>> {
    let user = me.require(Permission::ManageAutomations)?.clone();
    let mut c = get(&state.db, id).await?.ok_or(AppError::NotFound("connector"))?;
    if let Some(n) = req.name {
        c.name = n.trim().to_string();
    }
    if let Some(t) = req.transport {
        c.transport = t;
    }
    if let Some(a) = req.allowed_tools {
        c.allowed_tools = a;
    }
    let patch = req.secrets.unwrap_or_default();
    validate(&c.name, &c.transport, &patch.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect())?;
    for k in patch.keys() {
        if !valid_secret_key(k) {
            return Err(AppError::bad(format!("secret name {k:?} must look like an env var: UPPER_SNAKE_CASE")));
        }
    }
    let mut known: HashSet<String> = c.secret_keys.iter().cloned().collect();
    for (k, v) in &patch {
        if v.is_empty() {
            known.remove(k);
        } else {
            known.insert(k.clone());
        }
    }
    check_header_secrets(&c.transport, &known)?;
    save(&state.db, &c, Some(&patch)).await?;
    let c = get(&state.db, id).await?.ok_or(AppError::NotFound("connector"))?;
    state.upsert(Entity::Connector(c.clone()));
    state
        .record(
            HistoryKind::ConnectorUpdated,
            Actor::User(user.id),
            Refs::default(),
            format!("{} updated the connector \"{}\"", user.name, c.name),
            None,
        )
        .await;
    Ok(Json(c))
}

pub async fn delete(
    State(state): State<SharedState>,
    me: Auth,
    UrlPath(id): UrlPath<Id>,
) -> AppResult<StatusCode> {
    let user = me.require(Permission::ManageAutomations)?.clone();
    let c = get(&state.db, id).await?.ok_or(AppError::NotFound("connector"))?;
    sqlx::query("DELETE FROM connectors WHERE id = ?")
        .bind(id.to_string())
        .execute(&state.db.pool)
        .await
        .map_err(anyhow::Error::from)?;
    state.deleted(EntityKind::Connector, id);
    state
        .record(
            HistoryKind::ConnectorDeleted,
            Actor::User(user.id),
            Refs::default(),
            format!("{} removed the connector \"{}\"", user.name, c.name),
            None,
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test(
    State(state): State<SharedState>,
    me: Auth,
    UrlPath(id): UrlPath<Id>,
) -> AppResult<Json<ConnectorTestResult>> {
    me.require(Permission::ManageAutomations)?;
    let c = get(&state.db, id).await?.ok_or(AppError::NotFound("connector"))?;
    let secrets = secret_values(&state.db, id).await?;
    let result = match tokio::time::timeout(TEST_TIMEOUT, list_tools(&c.transport, &secrets)).await {
        Ok(Ok(tools)) => ConnectorTestResult { ok: true, tools, error: None },
        Ok(Err(e)) => ConnectorTestResult { ok: false, tools: vec![], error: Some(redact(&format!("{e:#}"), &secrets)) },
        Err(_) => ConnectorTestResult {
            ok: false,
            tools: vec![],
            error: Some(format!("no answer within {}s", TEST_TIMEOUT.as_secs())),
        },
    };
    Ok(Json(result))
}

/// Never echo secret values back, even inside an error from the MCP server.
fn redact(msg: &str, secrets: &BTreeMap<String, String>) -> String {
    let mut out = msg.to_string();
    for (k, v) in secrets {
        if v.len() >= 4 {
            out = out.replace(v, &format!("<{k}>"));
        }
    }
    out
}

// ================================================================ minimal MCP client

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "boba-agent", "version": env!("CARGO_PKG_VERSION")}
    })
}

fn tool_names(result: &Value) -> anyhow::Result<Vec<String>> {
    if let Some(err) = result.get("error") {
        bail!("MCP error: {err}");
    }
    let tools = result
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("tools/list answer has no tools array"))?;
    let mut names: Vec<String> = tools.iter().filter_map(|t| t.get("name")?.as_str().map(String::from)).collect();
    names.sort();
    Ok(names)
}

/// Start/contact the MCP server, perform the handshake and return its tool names.
pub async fn list_tools(transport: &McpTransport, secrets: &BTreeMap<String, String>) -> anyhow::Result<Vec<String>> {
    match transport {
        McpTransport::Stdio { command, args } => stdio_list_tools(command, args, secrets).await,
        McpTransport::Http { url, header_secrets } => http_list_tools(url, header_secrets, secrets).await,
    }
}

async fn stdio_list_tools(command: &str, args: &[String], secrets: &BTreeMap<String, String>) -> anyhow::Result<Vec<String>> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .envs(secrets)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot start {command}"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let mut lines = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?).lines();

    async fn send(stdin: &mut tokio::process::ChildStdin, v: &Value) -> anyhow::Result<()> {
        stdin.write_all(format!("{v}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
    async fn wait_for(lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>, id: u64) -> anyhow::Result<Value> {
        while let Some(line) = lines.next_line().await? {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(v);
            }
        }
        bail!("the MCP server exited before answering")
    }

    send(&mut stdin, &rpc(1, "initialize", initialize_params())).await?;
    let init = wait_for(&mut lines, 1).await?;
    if let Some(err) = init.get("error") {
        bail!("initialize failed: {err}");
    }
    send(&mut stdin, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await?;
    send(&mut stdin, &rpc(2, "tools/list", json!({}))).await?;
    let tools = tool_names(&wait_for(&mut lines, 2).await?);
    let _ = child.kill().await;
    tools
}

fn header_values(header_secrets: &BTreeMap<String, String>, secrets: &BTreeMap<String, String>) -> anyhow::Result<Vec<(String, String)>> {
    header_secrets
        .iter()
        .map(|(h, k)| {
            let v = secrets.get(k).ok_or_else(|| anyhow!("secret {k} is not set"))?;
            let v = if h.eq_ignore_ascii_case("authorization") && !v.contains(' ') {
                format!("Bearer {v}")
            } else {
                v.clone()
            };
            Ok((h.clone(), v))
        })
        .collect()
}

/// Parse a streamable-HTTP MCP answer: plain JSON, or SSE whose `data:` lines carry JSON.
fn parse_http_answer(content_type: &str, body: &str, id: u64) -> anyhow::Result<Value> {
    if content_type.contains("text/event-stream") {
        for line in body.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    if v.get("id").and_then(Value::as_u64) == Some(id) {
                        return Ok(v);
                    }
                }
            }
        }
        bail!("no answer for request {id} in the event stream")
    } else {
        Ok(serde_json::from_str(body).context("MCP server did not return JSON")?)
    }
}

async fn http_list_tools(
    url: &str,
    header_secrets: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let headers = header_values(header_secrets, secrets)?;
    let mut session: Option<String> = None;
    let post = |session: &Option<String>, body: Value| {
        let mut req = client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .json(&body);
        for (h, v) in &headers {
            req = req.header(h.as_str(), v.as_str());
        }
        if let Some(s) = session {
            req = req.header("mcp-session-id", s.as_str());
        }
        req
    };

    let resp = post(&session, rpc(1, "initialize", initialize_params())).send().await?;
    let status = resp.status();
    if !status.is_success() {
        bail!("initialize returned HTTP {status}");
    }
    session = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let init = parse_http_answer(&ct, &resp.text().await?, 1)?;
    if let Some(err) = init.get("error") {
        bail!("initialize failed: {err}");
    }
    let _ = post(&session, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).send().await;
    let resp = post(&session, rpc(2, "tools/list", json!({}))).send().await?;
    let status = resp.status();
    if !status.is_success() {
        bail!("tools/list returned HTTP {status}");
    }
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    tool_names(&parse_http_answer(&ct, &resp.text().await?, 2)?)
}

// ================================================================ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn init_test_key() {
        let _ = KEY.set([7u8; 32]);
    }

    #[test]
    fn encrypt_roundtrip_and_nonce_uniqueness() {
        init_test_key();
        let a = encrypt("s3cr3t-value").unwrap();
        let b = encrypt("s3cr3t-value").unwrap();
        assert_ne!(a, b, "nonce must be random");
        assert_eq!(decrypt(&a).unwrap(), "s3cr3t-value");
        assert!(!a.contains("s3cr3t"));
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        init_test_key();
        let mut raw = B64.decode(encrypt("hello").unwrap()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert!(decrypt(&B64.encode(raw)).is_err());
    }

    #[test]
    fn key_parsing() {
        assert!(parse_key(&"ab".repeat(32)).is_ok());
        assert!(parse_key(&B64.encode([1u8; 32])).is_ok());
        assert!(parse_key("short").is_err());
    }

    #[test]
    fn server_names_are_unique_and_avoid_boba() {
        let mut taken = HashSet::from([BOARD_SERVER_NAME.to_string()]);
        assert_eq!(server_name("Search Console", &mut taken), "search_console");
        assert_eq!(server_name("search-console", &mut taken), "search_console_2");
        assert_eq!(server_name("Boba", &mut taken), "boba_ext");
        assert_eq!(server_name("!!!", &mut taken), "connector");
    }

    #[test]
    fn secret_key_names() {
        assert!(valid_secret_key("GITHUB_TOKEN"));
        assert!(valid_secret_key("_X1"));
        assert!(!valid_secret_key("lower"));
        assert!(!valid_secret_key("1ABC"));
        assert!(!valid_secret_key("A-B"));
    }

    #[test]
    fn redaction_hides_values() {
        let s = BTreeMap::from([("TOKEN".to_string(), "abcd1234".to_string())]);
        assert_eq!(redact("bad token abcd1234!", &s), "bad token <TOKEN>!");
    }

    #[test]
    fn authorization_header_gets_bearer() {
        let hs = BTreeMap::from([("Authorization".to_string(), "TOKEN".to_string()), ("X-Key".to_string(), "TOKEN".to_string())]);
        let s = BTreeMap::from([("TOKEN".to_string(), "abc".to_string())]);
        let v = header_values(&hs, &s).unwrap();
        assert!(v.contains(&("Authorization".into(), "Bearer abc".into())));
        assert!(v.contains(&("X-Key".into(), "abc".into())));
    }

    #[test]
    fn sse_answers_are_parsed() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"b\"},{\"name\":\"a\"}]}}\n\n";
        let v = parse_http_answer("text/event-stream", body, 2).unwrap();
        assert_eq!(tool_names(&v).unwrap(), vec!["a", "b"]);
    }

    /// End-to-end stdio handshake against a tiny fake MCP server written in sh.
    #[tokio::test]
    async fn stdio_handshake_lists_tools() {
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake","version":"0"}}}' ;;
    *'"id":2'*) echo "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"query_$TOKEN\"}]}}" ;;
  esac
done"#;
        let secrets = BTreeMap::from([("TOKEN".to_string(), "xyz".to_string())]);
        let tools = list_tools(
            &McpTransport::Stdio { command: "sh".into(), args: vec!["-c".into(), script.into()] },
            &secrets,
        )
        .await
        .unwrap();
        assert_eq!(tools, vec!["query_xyz"]);
    }
}

//! SQLite persistence. Plain runtime queries (no compile-time `DATABASE_URL`), one
//! loader + one full-row upsert per entity. Writes that must be consistent with each
//! other are serialised by the orchestrator lock, so we do not need long transactions.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use boba_core::api::{HistoryQuery, WorkspaceSettings};
use boba_core::event::HistoryEvent;
use boba_core::*;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

// ---------------------------------------------------------------- encoding helpers

/// Fixed-width RFC 3339 so that text order == time order.
pub fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub fn parse_ts(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("bad timestamp {s:?}"))?
        .with_timezone(&Utc))
}

/// Serde enum -> its wire string ("in_progress", "acceptEdits"...).
pub fn enc<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

pub fn dec<T: DeserializeOwned>(s: &str) -> anyhow::Result<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .with_context(|| format!("bad enum value {s:?}"))
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn id(row: &SqliteRow, col: &str) -> anyhow::Result<Id> {
    let s: String = row.try_get(col)?;
    Ok(Id::parse_str(&s)?)
}

fn opt_id(row: &SqliteRow, col: &str) -> anyhow::Result<Option<Id>> {
    let s: Option<String> = row.try_get(col)?;
    s.map(|s| Id::parse_str(&s).map_err(Into::into)).transpose()
}

fn time(row: &SqliteRow, col: &str) -> anyhow::Result<DateTime<Utc>> {
    let s: String = row.try_get(col)?;
    parse_ts(&s)
}

fn opt_time(row: &SqliteRow, col: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
    let s: Option<String> = row.try_get(col)?;
    s.as_deref().map(parse_ts).transpose()
}

fn s(v: Option<Id>) -> Option<String> {
    v.map(|i| i.to_string())
}

/// Automatic review attempts per entry into a review column.
pub const REVIEW_ATTEMPTS: i64 = 2;

/// Invite links are valid this long.
pub const INVITE_DAYS: i64 = 7;

pub fn new_id() -> Id {
    Id::now_v7()
}

// ---------------------------------------------------------------- pool

#[derive(Clone)]
pub struct Db {
    pub pool: SqlitePool,
}

impl Db {
    pub async fn open(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let path = data_dir.join("boba.db");
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .context("opening SQLite database")?;
        MIGRATOR.run(&pool).await.context("running migrations")?;
        Ok(Self { pool })
    }

    // ------------------------------------------------------------ settings

    pub async fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO settings(key, value) VALUES(?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Workspace settings without `demo_mode`, which is runtime state.
    pub async fn workspace_settings(&self, demo_mode: bool) -> anyhow::Result<WorkspaceSettings> {
        let mut out = match self.setting("workspace").await? {
            Some(v) => serde_json::from_str(&v).unwrap_or_default(),
            None => WorkspaceSettings::default(),
        };
        out.demo_mode = demo_mode;
        Ok(out)
    }

    pub async fn save_workspace_settings(&self, s: &WorkspaceSettings) -> anyhow::Result<()> {
        self.set_setting("workspace", &json(s)).await
    }

    /// Next human ticket number (T-n): a monotonic counter, never reused after deletes.
    pub async fn next_ticket_number(&self) -> anyhow::Result<i64> {
        let v: String = sqlx::query_scalar(
            "INSERT INTO settings(key, value)
               VALUES('ticket_seq', (SELECT CAST(COALESCE(MAX(number), 0) + 1 AS TEXT) FROM tickets))
             ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)
             RETURNING value",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(v.parse()?)
    }

    // ------------------------------------------------------------ users & sessions

    pub async fn user_count(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?)
    }

    fn user_row(row: &SqliteRow) -> anyhow::Result<User> {
        Ok(User {
            id: id(row, "id")?,
            name: row.try_get("name")?,
            email: row.try_get("email")?,
            role: dec(&row.try_get::<String, _>("role")?)?,
            avatar: row.try_get("avatar")?,
            created_at: time(row, "created_at")?,
        })
    }

    pub async fn users(&self) -> anyhow::Result<Vec<User>> {
        sqlx::query("SELECT * FROM users ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::user_row)
            .collect()
    }

    pub async fn user(&self, uid: Id) -> anyhow::Result<Option<User>> {
        sqlx::query("SELECT * FROM users WHERE id = ?")
            .bind(uid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::user_row)
            .transpose()
    }

    /// (user, password hash) for login.
    pub async fn user_by_email(
        &self,
        email: &str,
    ) -> anyhow::Result<Option<(User, Option<String>)>> {
        let row = sqlx::query("SELECT * FROM users WHERE email = ?")
            .bind(email.trim())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| Ok((Self::user_row(&r)?, r.try_get("password_hash")?)))
            .transpose()
    }

    /// Consume an invite atomically: sets the password only if the token is valid and
    /// unexpired, and clears it in the same statement (no double accept).
    pub async fn accept_invite(
        &self,
        token_hash: &str,
        password_hash: &str,
    ) -> anyhow::Result<Option<User>> {
        let uid: Option<String> = sqlx::query_scalar(
            "UPDATE users SET password_hash = ?, invite_token_hash = NULL, invite_expires_at = NULL
             WHERE invite_token_hash = ? AND invite_expires_at > ? RETURNING id",
        )
        .bind(password_hash)
        .bind(token_hash)
        .bind(ts(Utc::now()))
        .fetch_optional(&self.pool)
        .await?;
        match uid {
            Some(id) => self.user(Id::parse_str(&id)?).await,
            None => Ok(None),
        }
    }

    pub async fn insert_user(
        &self,
        u: &User,
        password_hash: Option<&str>,
        invite_token_hash: Option<&str>,
    ) -> anyhow::Result<()> {
        let expires =
            invite_token_hash.map(|_| ts(Utc::now() + chrono::Duration::days(INVITE_DAYS)));
        sqlx::query(
            "INSERT INTO users(id, name, email, role, avatar, password_hash, invite_token_hash,
               invite_expires_at, created_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(u.id.to_string())
        .bind(&u.name)
        .bind(u.email.trim())
        .bind(enc(&u.role))
        .bind(&u.avatar)
        .bind(password_hash)
        .bind(invite_token_hash)
        .bind(expires)
        .bind(ts(u.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_role(&self, uid: Id, role: Role) -> anyhow::Result<()> {
        sqlx::query("UPDATE users SET role = ? WHERE id = ?")
            .bind(enc(&role))
            .bind(uid.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn email_taken(&self, email: &str) -> anyhow::Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email = ?")
            .bind(email.trim())
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
    }

    pub async fn insert_session(
        &self,
        token_hash: &str,
        uid: Id,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO sessions(token_hash, user_id, created_at, expires_at) VALUES(?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(uid.to_string())
        .bind(ts(Utc::now()))
        .bind(ts(expires_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn session_user(&self, token_hash: &str) -> anyhow::Result<Option<User>> {
        sqlx::query(
            "SELECT u.* FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ? AND s.expires_at > ?",
        )
        .bind(token_hash)
        .bind(ts(Utc::now()))
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(Self::user_row)
        .transpose()
    }

    pub async fn delete_session(&self, token_hash: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn purge_expired_sessions(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(ts(Utc::now()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------ repos & runners

    fn repo_row(row: &SqliteRow) -> anyhow::Result<Repo> {
        Ok(Repo {
            id: id(row, "id")?,
            name: row.try_get("name")?,
            path: row.try_get("path")?,
            remote_url: row.try_get("remote_url")?,
            default_branch: row.try_get("default_branch")?,
            runner_id: opt_id(row, "runner_id")?,
            sync: serde_json::from_str(&row.try_get::<String, _>("sync")?).unwrap_or_default(),
        })
    }

    pub async fn repos(&self) -> anyhow::Result<Vec<Repo>> {
        sqlx::query("SELECT * FROM repos ORDER BY name")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::repo_row)
            .collect()
    }

    pub async fn repo(&self, rid: Id) -> anyhow::Result<Option<Repo>> {
        sqlx::query("SELECT * FROM repos WHERE id = ?")
            .bind(rid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::repo_row)
            .transpose()
    }

    pub async fn save_repo(&self, r: &Repo) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO repos(id, name, path, remote_url, default_branch, runner_id, sync)
             VALUES(?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, path = excluded.path,
               remote_url = excluded.remote_url, default_branch = excluded.default_branch,
               runner_id = excluded.runner_id, sync = excluded.sync",
        )
        .bind(r.id.to_string())
        .bind(&r.name)
        .bind(&r.path)
        .bind(&r.remote_url)
        .bind(&r.default_branch)
        .bind(s(r.runner_id))
        .bind(json(&r.sync))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_repo(&self, rid: Id) -> anyhow::Result<bool> {
        let rid = rid.to_string();
        sqlx::query("UPDATE tickets SET repo_id = NULL WHERE repo_id = ?")
            .bind(&rid)
            .execute(&self.pool)
            .await?;
        let r = sqlx::query("DELETE FROM repos WHERE id = ?")
            .bind(&rid)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn upsert_runner(&self, r: &RunnerInfo) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO runners(id, name, max_concurrent_runs, last_seen_at) VALUES(?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name,
               max_concurrent_runs = excluded.max_concurrent_runs, last_seen_at = excluded.last_seen_at",
        )
        .bind(r.id.to_string())
        .bind(&r.name)
        .bind(r.max_concurrent_runs as i64)
        .bind(r.last_seen_at.map(ts))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ------------------------------------------------------------ tabs & columns

    fn column_row(row: &SqliteRow) -> anyhow::Result<Column> {
        Ok(Column {
            id: id(row, "id")?,
            tab_id: id(row, "tab_id")?,
            name: row.try_get("name")?,
            kind: dec(&row.try_get::<String, _>("kind")?)?,
            color: row.try_get("color")?,
            position: row.try_get("position")?,
            auto_reviewer_agent_id: opt_id(row, "auto_reviewer_agent_id")?,
            on_approve_column_id: opt_id(row, "on_approve_column_id")?,
            on_changes_column_id: opt_id(row, "on_changes_column_id")?,
        })
    }

    fn tab_row(row: &SqliteRow) -> anyhow::Result<Tab> {
        Ok(Tab {
            id: id(row, "id")?,
            name: row.try_get("name")?,
            icon: row.try_get("icon")?,
            color: row.try_get("color")?,
            position: row.try_get("position")?,
            repo_id: opt_id(row, "repo_id")?,
            columns: Vec::new(),
        })
    }

    pub async fn tabs(&self) -> anyhow::Result<Vec<Tab>> {
        let mut tabs: Vec<Tab> = sqlx::query("SELECT * FROM tabs ORDER BY position, name")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::tab_row)
            .collect::<anyhow::Result<_>>()?;
        let cols: Vec<Column> = sqlx::query("SELECT * FROM columns ORDER BY position")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::column_row)
            .collect::<anyhow::Result<_>>()?;
        for c in cols {
            if let Some(t) = tabs.iter_mut().find(|t| t.id == c.tab_id) {
                t.columns.push(c);
            }
        }
        Ok(tabs)
    }

    pub async fn tab(&self, tid: Id) -> anyhow::Result<Option<Tab>> {
        let Some(row) = sqlx::query("SELECT * FROM tabs WHERE id = ?")
            .bind(tid.to_string())
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let mut tab = Self::tab_row(&row)?;
        tab.columns = sqlx::query("SELECT * FROM columns WHERE tab_id = ? ORDER BY position")
            .bind(tid.to_string())
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::column_row)
            .collect::<anyhow::Result<_>>()?;
        Ok(Some(tab))
    }

    pub async fn column(&self, cid: Id) -> anyhow::Result<Option<Column>> {
        sqlx::query("SELECT * FROM columns WHERE id = ?")
            .bind(cid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::column_row)
            .transpose()
    }

    /// Insert or update the tab and replace its column list atomically.
    pub async fn save_tab(&self, t: &Tab) -> anyhow::Result<()> {
        self.save_tab_moving(t, &[]).await
    }

    /// Save the tab + its columns and move tickets (id, column, position) in one transaction,
    /// so tickets of removed columns are never left pointing at a deleted column.
    pub async fn save_tab_moving(&self, t: &Tab, moves: &[(Id, Id, f64)]) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO tabs(id, name, icon, color, position, repo_id) VALUES(?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, icon = excluded.icon,
               color = excluded.color, position = excluded.position, repo_id = excluded.repo_id",
        )
        .bind(t.id.to_string())
        .bind(&t.name)
        .bind(&t.icon)
        .bind(&t.color)
        .bind(t.position)
        .bind(s(t.repo_id))
        .execute(&mut *tx)
        .await?;
        let keep: Vec<String> = t.columns.iter().map(|c| c.id.to_string()).collect();
        let existing: Vec<String> = sqlx::query_scalar("SELECT id FROM columns WHERE tab_id = ?")
            .bind(t.id.to_string())
            .fetch_all(&mut *tx)
            .await?;
        for old in existing.iter().filter(|e| !keep.contains(e)) {
            sqlx::query("DELETE FROM columns WHERE id = ?")
                .bind(old)
                .execute(&mut *tx)
                .await?;
        }
        for c in &t.columns {
            sqlx::query(
                "INSERT INTO columns(id, tab_id, name, kind, color, position, auto_reviewer_agent_id,
                   on_approve_column_id, on_changes_column_id) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, kind = excluded.kind,
                   color = excluded.color, position = excluded.position,
                   auto_reviewer_agent_id = excluded.auto_reviewer_agent_id,
                   on_approve_column_id = excluded.on_approve_column_id,
                   on_changes_column_id = excluded.on_changes_column_id",
            )
            .bind(c.id.to_string())
            .bind(t.id.to_string())
            .bind(&c.name)
            .bind(enc(&c.kind))
            .bind(&c.color)
            .bind(c.position)
            .bind(s(c.auto_reviewer_agent_id))
            .bind(s(c.on_approve_column_id))
            .bind(s(c.on_changes_column_id))
            .execute(&mut *tx)
            .await?;
        }
        let now = ts(Utc::now());
        for (ticket, column, position) in moves {
            sqlx::query(
                "UPDATE tickets SET column_id = ?, position = ?, updated_at = ?, entered_column_at = ?
                 WHERE id = ?",
            )
            .bind(column.to_string())
            .bind(position)
            .bind(&now)
            .bind(&now)
            .bind(ticket.to_string())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_tab(&self, tid: Id) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM tabs WHERE id = ?")
            .bind(tid.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn tab_count(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM tabs")
            .fetch_one(&self.pool)
            .await?)
    }

    /// Columns whose auto reviewer is this agent (they must be cleared when it is deleted).
    pub async fn clear_reviewer(&self, agent_id: Id) -> anyhow::Result<Vec<Id>> {
        let tabs: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT tab_id FROM columns WHERE auto_reviewer_agent_id = ?",
        )
        .bind(agent_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE columns SET auto_reviewer_agent_id = NULL WHERE auto_reviewer_agent_id = ?",
        )
        .bind(agent_id.to_string())
        .execute(&self.pool)
        .await?;
        tabs.iter()
            .map(|t| Id::parse_str(t).map_err(Into::into))
            .collect()
    }

    // ------------------------------------------------------------ skills

    fn skill_row(row: &SqliteRow) -> anyhow::Result<Skill> {
        Ok(Skill {
            id: id(row, "id")?,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            source: dec(&row.try_get::<String, _>("source")?)?,
            path: row.try_get("path")?,
            editable: row.try_get::<i64, _>("editable")? != 0,
        })
    }

    pub async fn skills(&self) -> anyhow::Result<Vec<Skill>> {
        sqlx::query("SELECT * FROM skills ORDER BY name")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::skill_row)
            .collect()
    }

    pub async fn skill(&self, sid: Id) -> anyhow::Result<Option<Skill>> {
        sqlx::query("SELECT * FROM skills WHERE id = ?")
            .bind(sid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::skill_row)
            .transpose()
    }

    /// Upsert keyed by path: a rediscovered skill keeps its id. Returns the stored skill.
    pub async fn upsert_skill(&self, sk: &Skill) -> anyhow::Result<Skill> {
        sqlx::query(
            "INSERT INTO skills(id, name, description, source, path, editable) VALUES(?, ?, ?, ?, ?, ?)
             ON CONFLICT(path) DO UPDATE SET name = excluded.name, description = excluded.description,
               source = excluded.source, editable = excluded.editable",
        )
        .bind(sk.id.to_string())
        .bind(&sk.name)
        .bind(&sk.description)
        .bind(enc(&sk.source))
        .bind(&sk.path)
        .bind(sk.editable as i64)
        .execute(&self.pool)
        .await?;
        let row = sqlx::query("SELECT * FROM skills WHERE path = ?")
            .bind(&sk.path)
            .fetch_one(&self.pool)
            .await?;
        Self::skill_row(&row)
    }

    /// Remove discovered skills that disappeared (Boba skills are never pruned here).
    pub async fn prune_skills(&self, keep_paths: &[String]) -> anyhow::Result<Vec<Id>> {
        let all = self.skills().await?;
        let mut removed = Vec::new();
        for sk in all {
            if !keep_paths.contains(&sk.path) {
                sqlx::query("DELETE FROM skills WHERE id = ?")
                    .bind(sk.id.to_string())
                    .execute(&self.pool)
                    .await?;
                removed.push(sk.id);
            }
        }
        Ok(removed)
    }

    // ------------------------------------------------------------ agents

    fn agent_row(row: &SqliteRow) -> anyhow::Result<Agent> {
        let tools: String = row.try_get("allowed_tools")?;
        Ok(Agent {
            id: id(row, "id")?,
            name: row.try_get("name")?,
            role: row.try_get("role")?,
            sprite: row.try_get("sprite")?,
            color: row.try_get("color")?,
            tagline: row.try_get("tagline")?,
            system_prompt: row.try_get("system_prompt")?,
            skill_ids: Vec::new(),
            model: row.try_get("model")?,
            permission_mode: dec(&row.try_get::<String, _>("permission_mode")?)?,
            allowed_tools: serde_json::from_str(&tools).unwrap_or_default(),
            max_turns: row
                .try_get::<Option<i64>, _>("max_turns")?
                .map(|v| v as u32),
            xp: row.try_get::<i64, _>("xp")? as u32,
            level: row.try_get::<i64, _>("level")? as u32,
            runs_completed: row.try_get::<i64, _>("runs_completed")? as u32,
            prs_approved: row.try_get::<i64, _>("prs_approved")? as u32,
            status: dec(&row.try_get::<String, _>("status")?)?,
            current_run_id: opt_id(row, "current_run_id")?,
            connector_ids: Vec::new(),
            board_tools: row.try_get::<i64, _>("board_tools")? != 0,
            created_by: opt_id(row, "created_by")?,
            created_at: time(row, "created_at")?,
        })
    }

    /// Skills + connectors of one agent (indexed lookups).
    async fn attach_skills_for(&self, one: &mut [Agent; 1]) -> anyhow::Result<()> {
        let aid = one[0].id.to_string();
        one[0].skill_ids = sqlx::query_scalar::<_, String>(
            "SELECT skill_id FROM agent_skills WHERE agent_id = ? ORDER BY position",
        )
        .bind(&aid)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .filter_map(|s| Id::parse_str(s).ok())
        .collect();
        one[0].connector_ids = sqlx::query_scalar::<_, String>(
            "SELECT connector_id FROM agent_connectors WHERE agent_id = ? ORDER BY position",
        )
        .bind(&aid)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .filter_map(|s| Id::parse_str(s).ok())
        .collect();
        Ok(())
    }

    async fn attach_skills(&self, agents: &mut [Agent]) -> anyhow::Result<()> {
        let rows = sqlx::query("SELECT agent_id, skill_id FROM agent_skills ORDER BY position")
            .fetch_all(&self.pool)
            .await?;
        for r in rows {
            let aid = id(&r, "agent_id")?;
            let sid = id(&r, "skill_id")?;
            if let Some(a) = agents.iter_mut().find(|a| a.id == aid) {
                a.skill_ids.push(sid);
            }
        }
        let rows =
            sqlx::query("SELECT agent_id, connector_id FROM agent_connectors ORDER BY position")
                .fetch_all(&self.pool)
                .await?;
        for r in rows {
            let aid = id(&r, "agent_id")?;
            let cid = id(&r, "connector_id")?;
            if let Some(a) = agents.iter_mut().find(|a| a.id == aid) {
                a.connector_ids.push(cid);
            }
        }
        Ok(())
    }

    pub async fn agents(&self) -> anyhow::Result<Vec<Agent>> {
        let mut agents: Vec<Agent> = sqlx::query("SELECT * FROM agents ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::agent_row)
            .collect::<anyhow::Result<_>>()?;
        self.attach_skills(&mut agents).await?;
        Ok(agents)
    }

    pub async fn agent(&self, aid: Id) -> anyhow::Result<Option<Agent>> {
        let Some(row) = sqlx::query("SELECT * FROM agents WHERE id = ?")
            .bind(aid.to_string())
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let mut a = [Self::agent_row(&row)?];
        self.attach_skills_for(&mut a).await?;
        let [a] = a;
        Ok(Some(a))
    }

    pub async fn save_agent(&self, a: &Agent) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO agents(id, name, role, sprite, color, tagline, system_prompt, model,
               permission_mode, allowed_tools, max_turns, xp, level, runs_completed, prs_approved,
               status, current_run_id, board_tools, created_by, created_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, role = excluded.role,
               sprite = excluded.sprite, color = excluded.color, tagline = excluded.tagline,
               system_prompt = excluded.system_prompt, model = excluded.model,
               permission_mode = excluded.permission_mode, allowed_tools = excluded.allowed_tools,
               max_turns = excluded.max_turns, xp = excluded.xp, level = excluded.level,
               runs_completed = excluded.runs_completed, prs_approved = excluded.prs_approved,
               status = excluded.status, current_run_id = excluded.current_run_id,
               board_tools = excluded.board_tools",
        )
        .bind(a.id.to_string())
        .bind(&a.name)
        .bind(&a.role)
        .bind(&a.sprite)
        .bind(&a.color)
        .bind(&a.tagline)
        .bind(&a.system_prompt)
        .bind(&a.model)
        .bind(enc(&a.permission_mode))
        .bind(json(&a.allowed_tools))
        .bind(a.max_turns.map(|v| v as i64))
        .bind(a.xp as i64)
        .bind(a.level as i64)
        .bind(a.runs_completed as i64)
        .bind(a.prs_approved as i64)
        .bind(enc(&a.status))
        .bind(s(a.current_run_id))
        .bind(a.board_tools as i64)
        .bind(s(a.created_by))
        .bind(ts(a.created_at))
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM agent_skills WHERE agent_id = ?")
            .bind(a.id.to_string())
            .execute(&mut *tx)
            .await?;
        for (i, sid) in a.skill_ids.iter().enumerate() {
            sqlx::query(
                "INSERT OR IGNORE INTO agent_skills(agent_id, skill_id, position)
                 SELECT ?, ?, ? WHERE EXISTS (SELECT 1 FROM skills WHERE id = ?)",
            )
            .bind(a.id.to_string())
            .bind(sid.to_string())
            .bind(i as i64)
            .bind(sid.to_string())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM agent_connectors WHERE agent_id = ?")
            .bind(a.id.to_string())
            .execute(&mut *tx)
            .await?;
        for (i, cid) in a.connector_ids.iter().enumerate() {
            sqlx::query("INSERT OR IGNORE INTO agent_connectors(agent_id, connector_id, position) VALUES(?, ?, ?)")
                .bind(a.id.to_string())
                .bind(cid.to_string())
                .bind(i as i64)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_agent(&self, aid: Id) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM agents WHERE id = ?")
            .bind(aid.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------ tickets

    fn ticket_row(row: &SqliteRow) -> anyhow::Result<Ticket> {
        let tags: String = row.try_get("tags")?;
        Ok(Ticket {
            id: id(row, "id")?,
            number: row.try_get("number")?,
            tab_id: id(row, "tab_id")?,
            column_id: id(row, "column_id")?,
            position: row.try_get("position")?,
            title: row.try_get("title")?,
            body: row.try_get("body")?,
            tags: serde_json::from_str(&tags).unwrap_or_default(),
            priority: dec(&row.try_get::<String, _>("priority")?)?,
            color: row.try_get("color")?,
            repo_id: opt_id(row, "repo_id")?,
            agent_id: opt_id(row, "agent_id")?,
            branch: row.try_get("branch")?,
            pr_url: row.try_get("pr_url")?,
            pr_number: row.try_get("pr_number")?,
            review_feedback: row.try_get("review_feedback")?,
            review_rounds: row.try_get::<i64, _>("review_rounds")? as u32,
            created_by: opt_id(row, "created_by")?,
            created_at: time(row, "created_at")?,
            updated_at: time(row, "updated_at")?,
            source: serde_json::from_str(&row.try_get::<String, _>("source")?).unwrap_or_default(),
            // Maintained by the comments feature (server workstream).
            comment_count: 0,
            open_thread_count: 0,
        })
    }

    pub async fn tickets(&self) -> anyhow::Result<Vec<Ticket>> {
        sqlx::query("SELECT * FROM tickets ORDER BY position")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::ticket_row)
            .collect()
    }

    pub async fn ticket(&self, tid: Id) -> anyhow::Result<Option<Ticket>> {
        sqlx::query("SELECT * FROM tickets WHERE id = ?")
            .bind(tid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::ticket_row)
            .transpose()
    }

    pub async fn tickets_in_tab(&self, tab_id: Id) -> anyhow::Result<Vec<Ticket>> {
        sqlx::query("SELECT * FROM tickets WHERE tab_id = ? ORDER BY position")
            .bind(tab_id.to_string())
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::ticket_row)
            .collect()
    }

    /// Largest position in a column (new tickets go to the bottom).
    pub async fn max_position(&self, column_id: Id) -> anyhow::Result<f64> {
        Ok(sqlx::query_scalar::<_, Option<f64>>(
            "SELECT MAX(position) FROM tickets WHERE column_id = ?",
        )
        .bind(column_id.to_string())
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0.0))
    }

    /// Full-row upsert. `entered_column_at` is bumped only when the column changes.
    pub async fn save_ticket(&self, t: &Ticket) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO tickets(id, number, tab_id, column_id, position, title, body, tags, priority,
               color, repo_id, agent_id, branch, pr_url, pr_number, review_feedback, review_rounds,
               created_by, created_at, updated_at, source, entered_column_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               entered_column_at = CASE WHEN tickets.column_id <> excluded.column_id
                                   THEN excluded.entered_column_at ELSE tickets.entered_column_at END,
               tab_id = excluded.tab_id, column_id = excluded.column_id, position = excluded.position,
               title = excluded.title, body = excluded.body, tags = excluded.tags,
               priority = excluded.priority, color = excluded.color, repo_id = excluded.repo_id,
               agent_id = excluded.agent_id, branch = excluded.branch, pr_url = excluded.pr_url,
               pr_number = excluded.pr_number, review_feedback = excluded.review_feedback,
               review_rounds = excluded.review_rounds, updated_at = excluded.updated_at,
               source = excluded.source",
        )
        .bind(t.id.to_string())
        .bind(t.number)
        .bind(t.tab_id.to_string())
        .bind(t.column_id.to_string())
        .bind(t.position)
        .bind(&t.title)
        .bind(&t.body)
        .bind(json(&t.tags))
        .bind(enc(&t.priority))
        .bind(&t.color)
        .bind(s(t.repo_id))
        .bind(s(t.agent_id))
        .bind(&t.branch)
        .bind(&t.pr_url)
        .bind(t.pr_number)
        .bind(&t.review_feedback)
        .bind(t.review_rounds as i64)
        .bind(s(t.created_by))
        .bind(ts(t.created_at))
        .bind(ts(t.updated_at))
        .bind(json(&t.source))
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_ticket(&self, tid: Id) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM tickets WHERE id = ?")
            .bind(tid.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Tickets waiting for `reviewer` in review columns: no review run started since they
    /// entered the column, oldest first.
    pub async fn review_queue(&self, reviewer: Id) -> anyhow::Result<Vec<Id>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT t.id FROM tickets t JOIN columns c ON c.id = t.column_id
             WHERE c.kind = 'review' AND c.auto_reviewer_agent_id = ?
               -- nothing running on it
               AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.ticket_id = t.id
                               AND r.status IN ('queued', 'running', 'waiting'))
               -- not reviewed (or cancelled by a human) since it entered the column
               AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.ticket_id = t.id
                               AND r.created_at >= t.entered_column_at AND r.status <> 'failed')
               -- a failed review is retried once, then the ticket waits for a human
               AND (SELECT COUNT(*) FROM runs r WHERE r.ticket_id = t.id
                    AND r.created_at >= t.entered_column_at AND r.status = 'failed') < ?
             ORDER BY t.entered_column_at",
        )
        .bind(reviewer.to_string())
        .bind(REVIEW_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|s| Id::parse_str(s).map_err(Into::into))
            .collect()
    }

    // ------------------------------------------------------------ runs

    fn run_row(row: &SqliteRow) -> anyhow::Result<Run> {
        let verdict: Option<String> = row.try_get("verdict")?;
        Ok(Run {
            id: id(row, "id")?,
            ticket_id: opt_id(row, "ticket_id")?,
            job_id: opt_id(row, "job_id")?,
            agent_id: id(row, "agent_id")?,
            kind: dec(&row.try_get::<String, _>("kind")?)?,
            status: dec(&row.try_get::<String, _>("status")?)?,
            runner_id: opt_id(row, "runner_id")?,
            branch: row.try_get("branch")?,
            worktree_path: row.try_get("worktree_path")?,
            session_id: row.try_get("session_id")?,
            progress: row.try_get::<f64, _>("progress")? as f32,
            activity: row.try_get("activity")?,
            summary: row.try_get("summary")?,
            verdict: verdict.as_deref().map(dec).transpose()?,
            cost_usd: row.try_get("cost_usd")?,
            input_tokens: row.try_get::<i64, _>("input_tokens")? as u64,
            output_tokens: row.try_get::<i64, _>("output_tokens")? as u64,
            started_by: opt_id(row, "started_by")?,
            created_at: time(row, "created_at")?,
            started_at: opt_time(row, "started_at")?,
            ended_at: opt_time(row, "ended_at")?,
            error: row.try_get("error")?,
        })
    }

    pub async fn run(&self, rid: Id) -> anyhow::Result<Option<Run>> {
        sqlx::query("SELECT * FROM runs WHERE id = ?")
            .bind(rid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::run_row)
            .transpose()
    }

    /// (extra_instructions, resume_session_id) the run was created with.
    pub async fn run_inputs(&self, rid: Id) -> anyhow::Result<(Option<String>, Option<String>)> {
        let row =
            sqlx::query("SELECT extra_instructions, resume_session_id FROM runs WHERE id = ?")
                .bind(rid.to_string())
                .fetch_one(&self.pool)
                .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }

    pub async fn runs_for_ticket(&self, tid: Id) -> anyhow::Result<Vec<Run>> {
        sqlx::query("SELECT * FROM runs WHERE ticket_id = ? ORDER BY created_at DESC")
            .bind(tid.to_string())
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::run_row)
            .collect()
    }

    pub async fn active_runs(&self) -> anyhow::Result<Vec<Run>> {
        sqlx::query("SELECT * FROM runs WHERE status IN ('queued', 'running', 'waiting') ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::run_row)
            .collect()
    }

    /// Non-terminal runs + the latest run of every ticket.
    pub async fn snapshot_runs(&self) -> anyhow::Result<Vec<Run>> {
        sqlx::query(
            "SELECT * FROM runs r WHERE r.status IN ('queued', 'running', 'waiting')
                OR r.id = (SELECT r2.id FROM runs r2 WHERE r2.ticket_id = r.ticket_id
                           ORDER BY r2.created_at DESC LIMIT 1)
             ORDER BY r.created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(Self::run_row)
        .collect()
    }

    /// Latest work run of a ticket that has a Claude Code session (to resume).
    pub async fn last_work_session(&self, tid: Id) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT session_id FROM runs WHERE ticket_id = ? AND kind = 'work'
               AND session_id IS NOT NULL ORDER BY created_at DESC LIMIT 1",
        )
        .bind(tid.to_string())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn insert_run(
        &self,
        r: &Run,
        extra_instructions: Option<&str>,
        resume_session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO runs(id, ticket_id, job_id, agent_id, kind, status, runner_id, created_at,
               started_by, extra_instructions, resume_session_id) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(r.id.to_string())
        .bind(s(r.ticket_id))
        .bind(s(r.job_id))
        .bind(r.agent_id.to_string())
        .bind(enc(&r.kind))
        .bind(enc(&r.status))
        .bind(s(r.runner_id))
        .bind(ts(r.created_at))
        .bind(s(r.started_by))
        .bind(extra_instructions)
        .bind(resume_session_id)
        .execute(&self.pool)
        .await?;
        self.save_run(r).await
    }

    pub async fn save_run(&self, r: &Run) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE runs SET status = ?, runner_id = ?, branch = ?, worktree_path = ?, session_id = ?,
               progress = ?, activity = ?, summary = ?, verdict = ?, cost_usd = ?, input_tokens = ?,
               output_tokens = ?, started_at = ?, ended_at = ?, error = ? WHERE id = ?",
        )
        .bind(enc(&r.status))
        .bind(s(r.runner_id))
        .bind(&r.branch)
        .bind(&r.worktree_path)
        .bind(&r.session_id)
        .bind(r.progress as f64)
        .bind(&r.activity)
        .bind(&r.summary)
        .bind(r.verdict.as_ref().map(enc))
        .bind(r.cost_usd)
        .bind(r.input_tokens as i64)
        .bind(r.output_tokens as i64)
        .bind(r.started_at.map(ts))
        .bind(r.ended_at.map(ts))
        .bind(&r.error)
        .bind(r.id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Targeted update used by the hot path (does not clobber status changes).
    pub async fn set_run_live_fields(
        &self,
        rid: Id,
        progress: Option<(f32, String)>,
        usage: Option<(f64, u64, u64)>,
    ) -> anyhow::Result<()> {
        if let Some((p, a)) = progress {
            sqlx::query("UPDATE runs SET progress = ?, activity = ? WHERE id = ?")
                .bind(p as f64)
                .bind(a)
                .bind(rid.to_string())
                .execute(&self.pool)
                .await?;
        }
        if let Some((c, i, o)) = usage {
            sqlx::query(
                "UPDATE runs SET cost_usd = ?, input_tokens = ?, output_tokens = ? WHERE id = ?",
            )
            .bind(c)
            .bind(i as i64)
            .bind(o as i64)
            .bind(rid.to_string())
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn next_log_seq(&self, rid: Id) -> anyhow::Result<u64> {
        let n: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq) + 1, 0) FROM run_logs WHERE run_id = ?")
                .bind(rid.to_string())
                .fetch_one(&self.pool)
                .await?;
        Ok(n as u64)
    }

    // ------------------------------------------------------------ run logs

    pub async fn insert_logs(&self, lines: &[RunLogLine]) -> anyhow::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        // SQLite caps bound parameters; 6 per row, chunks of 150 rows stay well below.
        for chunk in lines.chunks(150) {
            let mut qb = sqlx::QueryBuilder::new(
                "INSERT OR IGNORE INTO run_logs(run_id, seq, ts, kind, text, data) ",
            );
            qb.push_values(chunk, |mut b, l| {
                b.push_bind(l.run_id.to_string())
                    .push_bind(l.seq as i64)
                    .push_bind(ts(l.ts))
                    .push_bind(enc(&l.kind))
                    .push_bind(l.text.clone())
                    .push_bind(l.data.as_ref().map(|d| d.to_string()));
            });
            qb.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    fn log_row(row: &SqliteRow) -> anyhow::Result<RunLogLine> {
        let data: Option<String> = row.try_get("data")?;
        Ok(RunLogLine {
            run_id: id(row, "run_id")?,
            seq: row.try_get::<i64, _>("seq")? as u64,
            ts: time(row, "ts")?,
            kind: dec(&row.try_get::<String, _>("kind")?)?,
            text: row.try_get("text")?,
            data: data.and_then(|d| serde_json::from_str(&d).ok()),
        })
    }

    pub async fn logs(
        &self,
        rid: Id,
        after_seq: Option<u64>,
        limit: u32,
    ) -> anyhow::Result<Vec<RunLogLine>> {
        sqlx::query("SELECT * FROM run_logs WHERE run_id = ? AND seq > ? ORDER BY seq LIMIT ?")
            .bind(rid.to_string())
            .bind(after_seq.map(|s| s as i64).unwrap_or(-1))
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::log_row)
            .collect()
    }

    /// The last `n` lines of a run, in order.
    pub async fn tail_logs(&self, rid: Id, n: u32) -> anyhow::Result<Vec<RunLogLine>> {
        let mut out: Vec<RunLogLine> =
            sqlx::query("SELECT * FROM run_logs WHERE run_id = ? ORDER BY seq DESC LIMIT ?")
                .bind(rid.to_string())
                .bind(n as i64)
                .fetch_all(&self.pool)
                .await?
                .iter()
                .map(Self::log_row)
                .collect::<anyhow::Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    // ------------------------------------------------------------ history

    pub async fn insert_history(&self, e: &HistoryEvent) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO history(id, ts, kind, actor_kind, actor_id, ticket_id, agent_id, run_id, tab_id, message, data)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(e.id.to_string())
        .bind(ts(e.ts))
        .bind(enc(&e.kind))
        .bind(enc(&e.actor_kind))
        .bind(s(e.actor_id))
        .bind(s(e.ticket_id))
        .bind(s(e.agent_id))
        .bind(s(e.run_id))
        .bind(s(e.tab_id))
        .bind(&e.message)
        .bind(e.data.as_ref().map(|d| d.to_string()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn history_row(row: &SqliteRow) -> anyhow::Result<HistoryEvent> {
        let data: Option<String> = row.try_get("data")?;
        Ok(HistoryEvent {
            id: id(row, "id")?,
            ts: time(row, "ts")?,
            kind: dec(&row.try_get::<String, _>("kind")?)?,
            actor_kind: dec(&row.try_get::<String, _>("actor_kind")?)?,
            actor_id: opt_id(row, "actor_id")?,
            ticket_id: opt_id(row, "ticket_id")?,
            agent_id: opt_id(row, "agent_id")?,
            run_id: opt_id(row, "run_id")?,
            tab_id: opt_id(row, "tab_id")?,
            message: row.try_get("message")?,
            data: data.and_then(|d| serde_json::from_str(&d).ok()),
        })
    }

    pub async fn history(&self, q: &HistoryQuery) -> anyhow::Result<Vec<HistoryEvent>> {
        let mut qb = sqlx::QueryBuilder::new("SELECT * FROM history WHERE 1 = 1");
        if let Some(v) = q.ticket_id {
            qb.push(" AND ticket_id = ").push_bind(v.to_string());
        }
        if let Some(v) = q.agent_id {
            qb.push(" AND agent_id = ").push_bind(v.to_string());
        }
        if let Some(v) = q.run_id {
            qb.push(" AND run_id = ").push_bind(v.to_string());
        }
        if let Some(v) = q.tab_id {
            qb.push(" AND tab_id = ").push_bind(v.to_string());
        }
        if let Some(before) = &q.before {
            let b = parse_ts(before).map(ts).unwrap_or_else(|_| before.clone());
            qb.push(" AND ts < ").push_bind(b);
        }
        let limit = q.limit.unwrap_or(50).clamp(1, 500);
        qb.push(" ORDER BY ts DESC, id DESC LIMIT ")
            .push_bind(limit as i64);
        qb.build()
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(Self::history_row)
            .collect()
    }

    // ------------------------------------------------------------ notifications

    pub async fn insert_notification(
        &self,
        n: &boba_core::team::Notification,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO notifications(id, user_id, kind, message, ticket_id, comment_id, run_id, job_id,
               actor_user_id, actor_agent_id, read, created_at) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(n.id.to_string())
        .bind(n.user_id.to_string())
        .bind(enc(&n.kind))
        .bind(&n.message)
        .bind(s(n.ticket_id))
        .bind(s(n.comment_id))
        .bind(s(n.run_id))
        .bind(s(n.job_id))
        .bind(s(n.actor_user_id))
        .bind(s(n.actor_agent_id))
        .bind(n.read as i64)
        .bind(ts(n.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn notification_row(row: &SqliteRow) -> anyhow::Result<boba_core::team::Notification> {
        Ok(boba_core::team::Notification {
            id: id(row, "id")?,
            user_id: id(row, "user_id")?,
            kind: dec(&row.try_get::<String, _>("kind")?)?,
            message: row.try_get("message")?,
            ticket_id: opt_id(row, "ticket_id")?,
            comment_id: opt_id(row, "comment_id")?,
            run_id: opt_id(row, "run_id")?,
            job_id: opt_id(row, "job_id")?,
            actor_user_id: opt_id(row, "actor_user_id")?,
            actor_agent_id: opt_id(row, "actor_agent_id")?,
            read: row.try_get::<i64, _>("read")? != 0,
            created_at: time(row, "created_at")?,
        })
    }

    /// Newest first, optional `before` cursor (ISO timestamp, exclusive).
    pub async fn notifications(
        &self,
        user: Id,
        before: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<boba_core::team::Notification>> {
        let before = before
            .map(|b| parse_ts(b).map(ts).unwrap_or_else(|_| b.to_string()))
            .unwrap_or_else(|| "9999".into());
        sqlx::query(
            "SELECT * FROM notifications WHERE user_id = ? AND created_at < ?
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(user.to_string())
        .bind(before)
        .bind(limit.clamp(1, 200) as i64)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(Self::notification_row)
        .collect()
    }

    /// Snapshot view: every unread one plus the 50 latest (capped at 500).
    pub async fn snapshot_notifications(
        &self,
        user: Id,
    ) -> anyhow::Result<Vec<boba_core::team::Notification>> {
        sqlx::query(
            "SELECT * FROM notifications WHERE user_id = ?1
               AND (read = 0 OR id IN (SELECT id FROM notifications WHERE user_id = ?1
                                       ORDER BY created_at DESC LIMIT 50))
             ORDER BY created_at DESC LIMIT 500",
        )
        .bind(user.to_string())
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(Self::notification_row)
        .collect()
    }

    /// Mark some (or all) of a user's notifications read. Returns the ids changed.
    pub async fn mark_read(&self, user: Id, ids: Option<&[Id]>) -> anyhow::Result<Vec<Id>> {
        let rows: Vec<String> =
            match ids {
                None => sqlx::query_scalar(
                    "UPDATE notifications SET read = 1 WHERE user_id = ? AND read = 0 RETURNING id",
                )
                .bind(user.to_string())
                .fetch_all(&self.pool)
                .await?,
                Some(ids) => {
                    let mut out = Vec::new();
                    for chunk in ids.chunks(200) {
                        let mut qb = sqlx::QueryBuilder::new(
                            "UPDATE notifications SET read = 1 WHERE user_id = ",
                        );
                        qb.push_bind(user.to_string())
                            .push(" AND read = 0 AND id IN (");
                        let mut sep = qb.separated(", ");
                        for i in chunk {
                            sep.push_bind(i.to_string());
                        }
                        qb.push(") RETURNING id");
                        out.extend(
                            qb.build_query_scalar::<String>()
                                .fetch_all(&self.pool)
                                .await?,
                        );
                    }
                    out
                }
            };
        rows.iter()
            .map(|s| Id::parse_str(s).map_err(Into::into))
            .collect()
    }

    pub async fn notification(
        &self,
        nid: Id,
    ) -> anyhow::Result<Option<boba_core::team::Notification>> {
        sqlx::query("SELECT * FROM notifications WHERE id = ?")
            .bind(nid.to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(Self::notification_row)
            .transpose()
    }

    // ------------------------------------------------------------ profile

    pub async fn update_profile(&self, uid: Id, name: &str, avatar: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE users SET name = ?, avatar = ? WHERE id = ?")
            .bind(name)
            .bind(avatar)
            .bind(uid.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn password_hash(&self, uid: Id) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
                .bind(uid.to_string())
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
        )
    }

    pub async fn set_password(&self, uid: Id, hash: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(uid.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Sign the user out everywhere except `keep` (session token hash). Returns the removed hashes.
    pub async fn delete_other_sessions(&self, uid: Id, keep: &str) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "DELETE FROM sessions WHERE user_id = ? AND token_hash <> ? RETURNING token_hash",
        )
        .bind(uid.to_string())
        .bind(keep)
        .fetch_all(&self.pool)
        .await?)
    }
}

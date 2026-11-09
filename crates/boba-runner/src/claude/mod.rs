//! `ClaudeCodeRunner`: runs the real `claude` CLI headless in a git worktree.
//!
//! Per run:
//! 1. Worktree: work runs get `<data>/worktrees/T-<n>` on `boba/T-<n>-<slug>` (reused on
//!    change rounds); review runs get a detached `<data>/worktrees/T-<n>-review` at the PR head.
//! 2. Skills: the agent's skill folders are symlinked into a per-run plugin
//!    (`<data>/runs/<id>/plugin`, loaded with `--plugin-dir`, so they show as `boba:<name>`),
//!    and `--setting-sources project --strict-mcp-config` hides the server user's own skills,
//!    plugins, hooks and MCP servers. With `isolate_config_dir`, `CLAUDE_CONFIG_DIR` is
//!    `<data>/agents/<agent_id>/claude` (per agent, so `--resume` finds earlier sessions).
//! 3. `claude -p --input-format stream-json --output-format stream-json --verbose
//!    --replay-user-messages --permission-prompt-tool stdio …` with a whitelisted env.
//!    Permission prompts arrive as `control_request`s and are answered on stdin.
//! 4. Work runs: commit leftovers, push, open (or reuse) the PR → `PrOpened`.
//!    Review runs: parse the ```` ```boba-verdict ```` block.
//! 5. Worktrees are removed on success and kept on failure for debugging.

pub mod prompt;
pub mod stream;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use boba_core::automation::{JobOutput, McpTransport, TicketSource};
use boba_core::runner::{RunOutcome, RunSpec, RunUpdate};
use boba_core::{Id, LogKind, PermissionMode, ReviewVerdict, RunKind, RunStatus, Skill};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};

use crate::emit::{self, first_line, Emitter};
use crate::git::Tools;
use crate::process::GroupGuard;
use crate::table::{Control, ControlRx, RunTable, Stop};
use crate::util::{slugify, ticket_branch};
use crate::{Runner, UpdateTx};
use stream::{Event, ResultInfo, StreamParser};

/// Tools a review run may use without asking.
pub const REVIEW_ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(gh pr diff:*)",
    "Bash(gh pr view:*)",
];

/// Always allowed in runs that write (work, jobs): local git bookkeeping inside the worktree.
/// `git push` stays out: the runner pushes after the run.
pub const WORK_ALLOWED_TOOLS: &[&str] = &[
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git add:*)",
    "Bash(git commit:*)",
    "Bash(git rm:*)",
    "Bash(git mv:*)",
    "Bash(git restore:*)",
];

/// Removed from every run: writing outside the run, or reaching other Claude sessions.
pub const ALWAYS_DISALLOWED_TOOLS: &[&str] = &[
    "SendMessage",
    "ListAgents",
    "CronCreate",
    "CronDelete",
    "ScheduleWakeup",
    "RemoteTrigger",
    "PushNotification",
    "EnterWorktree",
    "ExitWorktree",
];

pub const REVIEW_DISALLOWED_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// Environment variables passed to `claude` (everything else is dropped).
pub const ENV_WHITELIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TZ",
    "SSH_AUTH_SOCK",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_HOST",
    // Anthropic API and cloud providers.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "AWS_REGION",
    "AWS_PROFILE",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "CLOUD_ML_REGION",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    pub id: Id,
    pub name: String,
    pub data_dir: PathBuf,
    pub claude_bin: PathBuf,
    pub tools: Tools,
    pub max_concurrent_runs: u32,
    pub run_timeout: Duration,
    /// Per-agent `CLAUDE_CONFIG_DIR`. Needs API-key (or Bedrock/Vertex) auth: a fresh
    /// config dir has no OAuth login.
    pub isolate_config_dir: bool,
    /// `bypassPermissions` is honoured only when true (run inside a container).
    pub allow_bypass: bool,
    /// Extra env vars to pass through, on top of [`ENV_WHITELIST`].
    pub env_passthrough: Vec<String>,
    /// Push and open PRs after work runs (off = leave the commits on the local branch).
    pub open_prs: bool,
    pub cleanup_on_success: bool,
}

impl ClaudeConfig {
    pub fn new(id: Id, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            id,
            name: "local".into(),
            data_dir: data_dir.into(),
            claude_bin: "claude".into(),
            tools: Tools::default(),
            max_concurrent_runs: 4,
            run_timeout: Duration::from_secs(2 * 60 * 60),
            isolate_config_dir: api_key_auth_available(),
            allow_bypass: false,
            env_passthrough: Vec::new(),
            open_prs: true,
            cleanup_on_success: true,
        }
    }

    /// [`ClaudeConfig::new`] plus `BOBA_CLAUDE_BIN`, `BOBA_GIT_BIN`, `BOBA_GH_BIN`,
    /// `BOBA_MAX_CONCURRENT_RUNS`, `BOBA_RUN_TIMEOUT_SECS`, `BOBA_ISOLATE_CLAUDE_CONFIG`
    /// (0/1), `BOBA_ALLOW_BYPASS` (0/1), `BOBA_ENV_PASSTHROUGH` (comma list),
    /// `BOBA_OPEN_PRS` (0/1), `BOBA_KEEP_WORKTREES` (0/1).
    pub fn from_env(id: Id, data_dir: impl Into<PathBuf>) -> Self {
        let mut c = Self::new(id, data_dir);
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let flag = |k: &str| var(k).map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
        if let Some(v) = var("BOBA_CLAUDE_BIN") {
            c.claude_bin = v.into();
        }
        if let Some(v) = var("BOBA_GIT_BIN") {
            c.tools.git = v.into();
        }
        if let Some(v) = var("BOBA_GH_BIN") {
            c.tools.gh = v.into();
        }
        if let Some(n) = var("BOBA_MAX_CONCURRENT_RUNS")
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
        {
            c.max_concurrent_runs = n;
        }
        if let Some(s) = var("BOBA_RUN_TIMEOUT_SECS")
            .and_then(|v| v.parse().ok())
            .filter(|s| *s > 0)
        {
            c.run_timeout = Duration::from_secs(s);
        }
        if let Some(b) = flag("BOBA_ISOLATE_CLAUDE_CONFIG") {
            c.isolate_config_dir = b;
        }
        if let Some(b) = flag("BOBA_ALLOW_BYPASS") {
            c.allow_bypass = b;
        }
        if let Some(b) = flag("BOBA_OPEN_PRS") {
            c.open_prs = b;
        }
        if let Some(b) = flag("BOBA_KEEP_WORKTREES") {
            c.cleanup_on_success = !b;
        }
        if let Some(v) = var("BOBA_ENV_PASSTHROUGH") {
            c.env_passthrough = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        c
    }
}

/// True when Claude Code can authenticate without the server user's OAuth login.
pub fn api_key_auth_available() -> bool {
    [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
    ]
    .iter()
    .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
}

struct Inner {
    cfg: ClaudeConfig,
    table: Arc<RunTable>,
}

#[derive(Clone)]
pub struct ClaudeCodeRunner {
    inner: Arc<Inner>,
}

impl ClaudeCodeRunner {
    /// Creates `data_dir` and makes it absolute (worktree paths are handed to Claude).
    pub fn new(mut cfg: ClaudeConfig) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        cfg.data_dir = std::fs::canonicalize(&cfg.data_dir)?;
        let table = Arc::new(RunTable::new(cfg.max_concurrent_runs));
        Ok(Self {
            inner: Arc::new(Inner { cfg, table }),
        })
    }

    pub fn active_runs(&self) -> usize {
        self.inner.table.len()
    }

    /// `claude --version`, to check the CLI is installed before choosing this runner.
    pub async fn claude_version(&self) -> anyhow::Result<String> {
        crate::git::run(
            &self.inner.cfg.claude_bin,
            &["--version"],
            &self.inner.cfg.data_dir,
        )
        .await
    }
}

#[async_trait]
impl Runner for ClaudeCodeRunner {
    fn id(&self) -> Id {
        self.inner.cfg.id
    }

    fn name(&self) -> &str {
        &self.inner.cfg.name
    }

    fn max_concurrent_runs(&self) -> u32 {
        self.inner.cfg.max_concurrent_runs
    }

    async fn start(&self, spec: RunSpec, updates: UpdateTx) -> anyhow::Result<()> {
        let inner = self.inner.clone();
        let em = Emitter::new(spec.run_id, updates);
        self.inner
            .table
            .spawn(spec.run_id, em, move |em, rx| async move {
                execute(inner, spec, em, rx).await
            })
    }

    async fn cancel(&self, run_id: Id) -> anyhow::Result<()> {
        self.inner.table.send(run_id, Control::Cancel)
    }

    async fn send_message(&self, run_id: Id, text: String) -> anyhow::Result<()> {
        self.inner.table.send(run_id, Control::Message(text))
    }

    async fn answer_permission(
        &self,
        run_id: Id,
        request_id: String,
        allow: bool,
    ) -> anyhow::Result<()> {
        self.inner
            .table
            .send(run_id, Control::Permission { request_id, allow })
    }

    async fn discover_skills(
        &self,
        repo_paths: &[String],
        boba_skills_dir: &str,
    ) -> anyhow::Result<Vec<Skill>> {
        let repos = repo_paths.to_vec();
        let dir = boba_skills_dir.to_string();
        Ok(tokio::task::spawn_blocking(move || {
            crate::skills::discover(&repos, &dir, crate::skills::claude_home().as_deref())
        })
        .await?)
    }
}

// ------------------------------------------------------------------ one run

/// How a run ends once Claude has produced its result.
#[derive(Debug, Clone, PartialEq)]
enum Finish {
    /// Commit, push, open/reuse a PR (work runs and PullRequest jobs).
    PullRequest { title: String, label: String },
    /// Parse a ```boba-verdict``` block.
    Verdict,
    /// Store the final message as the report (Report / Tickets jobs).
    Report,
    /// Parse a ```boba-plan``` block (command runs).
    Plan,
}

/// Where the run happens, decided before Claude starts.
struct Place {
    dir: PathBuf,
    /// Empty for runs outside git (scratch jobs, commands).
    branch: String,
    /// Ref new work is compared against (`origin/main`), for PR runs.
    base: String,
    /// The repo that owns `dir` when it is a worktree (removed on success).
    worktree_of: Option<PathBuf>,
    prompt: String,
    finish: Finish,
}

async fn execute(
    inner: Arc<Inner>,
    spec: RunSpec,
    em: Emitter,
    rx: ControlRx,
) -> Result<RunOutcome, Stop> {
    let cfg = &inner.cfg;
    let place = match prepare(cfg, &spec, &em).await {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("Could not prepare the run: {e:#}");
            em.error(msg.clone()).await?;
            return Ok(emit::failed(msg));
        }
    };
    let session_id = match spec.kind {
        RunKind::Work | RunKind::Job => spec.resume_session_id.clone(),
        RunKind::Review | RunKind::Command => None,
    }
    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    em.send(RunUpdate::Started {
        worktree_path: place.dir.to_string_lossy().into_owned(),
        branch: place.branch.clone(),
        session_id: Some(session_id.clone()),
    })
    .await?;
    match (&place.worktree_of, spec.kind) {
        (Some(_), RunKind::Review) => {
            em.system(format!(
                "Worktree ready at {} (review of {})",
                place.dir.display(),
                place.branch
            ))
            .await?
        }
        (Some(_), _) if !place.branch.is_empty() => {
            em.system(format!(
                "Worktree ready at {} (branch {})",
                place.dir.display(),
                place.branch
            ))
            .await?
        }
        (Some(_), _) => {
            em.system(format!("Worktree ready at {}", place.dir.display()))
                .await?
        }
        (None, RunKind::Command) => {}
        (None, _) => {
            em.system(format!("Working in {}", place.dir.display()))
                .await?
        }
    }

    let run_dir = cfg.data_dir.join("runs").join(spec.run_id.to_string());
    let outcome = match drive(cfg, &spec, &place, &session_id, &run_dir, &em, rx).await {
        Ok(Ok(result)) => match &place.finish {
            Finish::PullRequest { title, label } => {
                finish_pr(cfg, &spec, &place, title, label, &result, &em).await
            }
            Finish::Verdict => Ok(finish_review(&result)),
            Finish::Report => Ok(RunOutcome {
                summary: result
                    .text
                    .clone()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty()),
                ..emit::outcome(RunStatus::Succeeded)
            }),
            Finish::Plan => Ok(finish_command(&result)),
        },
        Ok(Err(msg)) => Ok(emit::failed(msg)),
        Err(stop) => Err(stop),
    };
    // Secrets (mcp.json) and skill links never outlive the run, whatever happened.
    let _ = std::fs::remove_dir_all(&run_dir);
    let outcome = outcome?;

    if let Some(repo) = &place.worktree_of {
        if outcome.status == RunStatus::Succeeded && cfg.cleanup_on_success {
            if let Err(e) = cfg.tools.remove_worktree(repo, &place.dir).await {
                tracing::warn!("could not remove worktree {}: {e:#}", place.dir.display());
            }
        } else if outcome.status != RunStatus::Succeeded {
            em.system(format!(
                "Worktree kept at {} for debugging",
                place.dir.display()
            ))
            .await?;
        }
    }
    if outcome.status != RunStatus::Succeeded {
        if let Some(e) = &outcome.error {
            em.error(e.clone()).await?;
        }
    }
    Ok(outcome)
}

fn need<'a, T>(v: &'a Option<T>, what: &str, kind: RunKind) -> anyhow::Result<&'a T> {
    v.as_ref()
        .ok_or_else(|| anyhow::anyhow!("{kind:?} run without {what}"))
}

async fn fetch_base(cfg: &ClaudeConfig, repo: &Path, default_branch: &str, em: &Emitter) -> String {
    if let Err(e) = cfg.tools.fetch(repo).await {
        tracing::warn!("git fetch failed in {}: {e:#}", repo.display());
        let _ = em
            .system(format!(
                "git fetch failed, using local refs: {}",
                first_line(&e.to_string(), 200)
            ))
            .await;
    }
    cfg.tools.base_ref(repo, default_branch).await
}

async fn prepare(cfg: &ClaudeConfig, spec: &RunSpec, em: &Emitter) -> anyhow::Result<Place> {
    let worktrees = cfg.data_dir.join("worktrees");
    match spec.kind {
        RunKind::Work => {
            let t = need(&spec.ticket, "a ticket", spec.kind)?;
            let r = need(&spec.repo, "a repo", spec.kind)?;
            let repo = PathBuf::from(&r.path);
            let base = fetch_base(cfg, &repo, &r.default_branch, em).await;
            let branch = t
                .branch
                .clone()
                .unwrap_or_else(|| ticket_branch(t.number, &t.title));
            let dir = worktrees.join(format!("T-{}", t.number));
            if cfg.tools.work_worktree(&repo, &dir, &branch, &base).await? {
                let _ = em
                    .system("Reusing the existing worktree (kept from an earlier run)")
                    .await;
            }
            Ok(Place {
                prompt: prompt::work_prompt(spec, t, &branch),
                dir,
                branch,
                base,
                worktree_of: Some(repo),
                finish: Finish::PullRequest {
                    title: format!("T-{}: {}", t.number, t.title),
                    label: format!("Ticket T-{}", t.number),
                },
            })
        }
        RunKind::Review => {
            let t = need(&spec.ticket, "a ticket", spec.kind)?;
            let r = need(&spec.repo, "a repo", spec.kind)?;
            let repo = PathBuf::from(&r.path);
            let base = fetch_base(cfg, &repo, &r.default_branch, em).await;
            let branch = match &t.source {
                TicketSource::GithubPr { head_branch, .. } => head_branch.clone(),
                _ => t
                    .branch
                    .clone()
                    .unwrap_or_else(|| ticket_branch(t.number, &t.title)),
            };
            let pr = t.pr_number.or(match &t.source {
                TicketSource::GithubPr { number, .. } => Some(*number),
                _ => None,
            });
            let dir = worktrees.join(format!("T-{}-review", t.number));
            cfg.tools.review_worktree(&repo, &dir, &branch, pr).await?;
            Ok(Place {
                prompt: prompt::review_prompt(spec, t, &branch),
                dir,
                branch,
                base,
                worktree_of: Some(repo),
                finish: Finish::Verdict,
            })
        }
        RunKind::Job => {
            let job = need(&spec.job, "a job", spec.kind)?;
            let slug = slugify(&job.name);
            match (job.output, &spec.repo) {
                (JobOutput::PullRequest, None) => {
                    anyhow::bail!("job \"{}\" opens a pull request but has no repo", job.name)
                }
                (JobOutput::PullRequest, Some(r)) => {
                    let repo = PathBuf::from(&r.path);
                    let base = fetch_base(cfg, &repo, &r.default_branch, em).await;
                    let branch = format!(
                        "boba/job-{slug}-{}",
                        chrono::Utc::now().format("%Y%m%d-%H%M")
                    );
                    let dir = worktrees.join(format!("job-{slug}"));
                    cfg.tools.work_worktree(&repo, &dir, &branch, &base).await?;
                    let wd = dir.to_string_lossy().into_owned();
                    Ok(Place {
                        prompt: prompt::job_prompt(spec, job, &wd, Some(&branch)),
                        dir,
                        branch,
                        base,
                        worktree_of: Some(repo),
                        finish: Finish::PullRequest {
                            title: format!(
                                "{} ({})",
                                job.name,
                                chrono::Utc::now().format("%Y-%m-%d")
                            ),
                            label: format!("Quest \"{}\"", job.name),
                        },
                    })
                }
                (_, Some(r)) => {
                    // Read the repo at its latest default branch; changes are not kept.
                    let repo = PathBuf::from(&r.path);
                    let base = fetch_base(cfg, &repo, &r.default_branch, em).await;
                    let dir = worktrees.join(format!("job-{slug}"));
                    if dir.exists() {
                        cfg.tools.remove_worktree(&repo, &dir).await?;
                    }
                    std::fs::create_dir_all(&worktrees)?;
                    cfg.tools
                        .git(
                            &repo,
                            &["worktree", "add", "--detach", &dir.to_string_lossy(), &base],
                        )
                        .await?;
                    let wd = dir.to_string_lossy().into_owned();
                    Ok(Place {
                        prompt: prompt::job_prompt(spec, job, &wd, None),
                        dir,
                        branch: String::new(),
                        base,
                        worktree_of: Some(repo),
                        finish: Finish::Report,
                    })
                }
                (_, None) => {
                    // Persistent per job, so a recurring quest can keep notes between runs.
                    let dir = cfg.data_dir.join("scratch").join(format!("job-{slug}"));
                    std::fs::create_dir_all(&dir)?;
                    let wd = dir.to_string_lossy().into_owned();
                    Ok(Place {
                        prompt: prompt::job_prompt(spec, job, &wd, None),
                        dir,
                        branch: String::new(),
                        base: String::new(),
                        worktree_of: None,
                        finish: Finish::Report,
                    })
                }
            }
        }
        RunKind::Command => {
            let cmd = need(&spec.command, "a command", spec.kind)?;
            let dir = cfg.data_dir.join("scratch").join("concierge");
            std::fs::create_dir_all(&dir)?;
            Ok(Place {
                prompt: prompt::command_prompt(cmd),
                dir,
                branch: String::new(),
                base: String::new(),
                worktree_of: None,
                finish: Finish::Plan,
            })
        }
    }
}

/// Symlink the agent's skills into a throwaway plugin. Returns its directory, if any skill.
fn prepare_skills(
    run_dir: &Path,
    skill_paths: &[String],
) -> std::io::Result<(Option<PathBuf>, Vec<String>)> {
    let plugin = run_dir.join("plugin");
    let skills_dir = plugin.join("skills");
    let mut names = Vec::new();
    let mut missing = Vec::new();
    for p in skill_paths {
        let src = Path::new(p);
        if !src.join("SKILL.md").is_file() {
            missing.push(p.clone());
            continue;
        }
        let base = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".into());
        let mut name = base.clone();
        let mut i = 2;
        while names.contains(&name) {
            name = format!("{base}-{i}");
            i += 1;
        }
        std::fs::create_dir_all(&skills_dir)?;
        let dst = skills_dir.join(&name);
        let _ = std::fs::remove_file(&dst);
        #[cfg(unix)]
        std::os::unix::fs::symlink(src, &dst)?;
        #[cfg(not(unix))]
        copy_dir(src, &dst)?;
        names.push(name);
    }
    if names.is_empty() {
        return Ok((None, missing));
    }
    std::fs::create_dir_all(plugin.join(".claude-plugin"))?;
    std::fs::write(
        plugin.join(".claude-plugin").join("plugin.json"),
        serde_json::json!({ "name": "boba", "description": "Skills assigned to this Boba Agent", "version": "0.0.0" })
            .to_string(),
    )?;
    Ok((Some(plugin), missing))
}

#[cfg(not(unix))]
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

/// MCP server names become part of tool names (`mcp__<name>__<tool>`): keep them tame.
pub fn mcp_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "mcp".into()
    } else {
        s
    }
}

/// `--mcp-config` JSON for the run plus the `--allowedTools` entries it implies.
/// Work and job runs get the agent's connectors and board tools; reviews and commands none.
pub fn mcp_config(spec: &RunSpec) -> Option<(Value, Vec<String>)> {
    if matches!(spec.kind, RunKind::Review | RunKind::Command) {
        return None;
    }
    let mut servers = serde_json::Map::new();
    let mut allowed = Vec::new();
    for m in &spec.mcp_servers {
        let name = mcp_name(&m.name);
        if name == "boba" {
            continue; // reserved for the board tools
        }
        let cfg = match &m.transport {
            McpTransport::Stdio { command, args } => serde_json::json!({
                "type": "stdio", "command": command, "args": args, "env": m.secrets,
            }),
            McpTransport::Http {
                url,
                header_secrets,
            } => {
                let headers: serde_json::Map<String, Value> = header_secrets
                    .iter()
                    .filter_map(|(h, key)| {
                        let v = m.secrets.get(key)?;
                        let v = if h.eq_ignore_ascii_case("authorization") && !v.contains(' ') {
                            format!("Bearer {v}")
                        } else {
                            v.clone()
                        };
                        Some((h.clone(), Value::String(v)))
                    })
                    .collect();
                serde_json::json!({ "type": "http", "url": url, "headers": headers })
            }
        };
        if m.allowed_tools.is_empty() {
            allowed.push(format!("mcp__{name}"));
        } else {
            allowed.extend(m.allowed_tools.iter().map(|t| format!("mcp__{name}__{t}")));
        }
        servers.insert(name, cfg);
    }
    if let Some(b) = &spec.board_tools {
        servers.insert(
            "boba".into(),
            serde_json::json!({ "type": "http", "url": b.url, "headers": { "Authorization": format!("Bearer {}", b.token) } }),
        );
        allowed.push("mcp__boba".into());
    }
    (!servers.is_empty()).then(|| (serde_json::json!({ "mcpServers": servers }), allowed))
}

/// Write `mcp.json` readable by the server user only.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    use std::io::Write;
    opts.open(path)?.write_all(contents.as_bytes())
}

fn mode_flag(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::BypassPermissions => "bypassPermissions",
    }
}

/// The `claude` argv for a run (the prompt itself goes on stdin).
pub fn claude_args(
    cfg: &ClaudeConfig,
    spec: &RunSpec,
    session_id: &str,
    plugin_dir: Option<&Path>,
    mcp: Option<(&Path, &[String])>,
) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
        "--setting-sources",
        "project",
        "--strict-mcp-config",
    ]
    .map(String::from)
    .to_vec();
    a.push("--append-system-prompt".into());
    a.push(prompt::system_prompt(spec));

    let read_only = matches!(spec.kind, RunKind::Review | RunKind::Command);
    let mode = match (read_only, spec.agent.permission_mode) {
        (true, _) => PermissionMode::Plan,
        (false, PermissionMode::BypassPermissions) if !cfg.allow_bypass => {
            PermissionMode::AcceptEdits
        }
        (false, m) => m,
    };
    a.push("--permission-mode".into());
    a.push(mode_flag(mode).into());
    if let Some(m) = spec.agent.model.as_deref().filter(|m| !m.is_empty()) {
        a.push("--model".into());
        a.push(m.into());
    }
    if let Some(n) = spec.agent.max_turns.filter(|n| *n > 0) {
        a.push("--max-turns".into());
        a.push(n.to_string());
    }
    match (&spec.resume_session_id, read_only) {
        (Some(id), false) => {
            a.push("--resume".into());
            a.push(id.clone());
        }
        _ => {
            a.push("--session-id".into());
            a.push(session_id.into());
        }
    }
    if spec.kind == RunKind::Command {
        // The concierge has no tools at all: it only reads the prompt and proposes a plan.
        a.push("--tools".into());
        a.push(String::new());
        return a;
    }
    if let Some(dir) = plugin_dir {
        a.push("--plugin-dir".into());
        a.push(dir.to_string_lossy().into_owned());
    }
    let mut allowed: Vec<String> = if spec.kind == RunKind::Review {
        REVIEW_ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect()
    } else {
        spec.agent
            .allowed_tools
            .iter()
            .filter(|t| !t.trim().is_empty())
            .cloned()
            .chain(WORK_ALLOWED_TOOLS.iter().map(|s| s.to_string()))
            .collect()
    };
    if let Some((path, tools)) = mcp {
        a.push("--mcp-config".into());
        a.push(path.to_string_lossy().into_owned());
        allowed.extend(tools.iter().cloned());
    }
    if !allowed.is_empty() {
        // One comma-joined value: the CLI splits it, and rules with spaces survive.
        a.push("--allowedTools".into());
        a.push(allowed.join(","));
    }
    let mut denied: Vec<&str> = ALWAYS_DISALLOWED_TOOLS.to_vec();
    if spec.kind == RunKind::Review {
        denied.extend(REVIEW_DISALLOWED_TOOLS);
    }
    a.push("--disallowedTools".into());
    a.push(denied.join(","));
    a
}

fn build_command(
    cfg: &ClaudeConfig,
    spec: &RunSpec,
    place: &Place,
    args: &[String],
) -> std::io::Result<Command> {
    let mut cmd = Command::new(&cfg.claude_bin);
    cmd.args(args)
        .current_dir(&place.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for k in ENV_WHITELIST
        .iter()
        .copied()
        .chain(cfg.env_passthrough.iter().map(String::as_str))
    {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    cmd.env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("BOBA_RUN_ID", spec.run_id.to_string());
    if let Some(t) = &spec.ticket {
        cmd.env("BOBA_TICKET", format!("T-{}", t.number));
    }
    if cfg.isolate_config_dir {
        let dir = cfg
            .data_dir
            .join("agents")
            .join(spec.agent.id.to_string())
            .join("claude");
        std::fs::create_dir_all(&dir)?;
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    Ok(cmd)
}

async fn write_line(stdin: &mut Option<ChildStdin>, line: &str) -> bool {
    let Some(s) = stdin.as_mut() else {
        return false;
    };
    let ok = s.write_all(line.as_bytes()).await.is_ok()
        && s.write_all(b"\n").await.is_ok()
        && s.flush().await.is_ok();
    if !ok {
        *stdin = None;
    }
    ok
}

/// Spawn Claude and pump its stream until it exits. Outer `Err` = stop (cancel/closed);
/// inner `Err` = a failure message for the run.
async fn drive(
    cfg: &ClaudeConfig,
    spec: &RunSpec,
    place: &Place,
    session_id: &str,
    run_dir: &Path,
    em: &Emitter,
    mut rx: ControlRx,
) -> Result<Result<ResultInfo, String>, Stop> {
    let read_only = matches!(spec.kind, RunKind::Review | RunKind::Command);
    let (plugin_dir, missing) = if spec.kind == RunKind::Command {
        (None, vec![])
    } else {
        match prepare_skills(run_dir, &spec.skill_paths) {
            Ok(v) => v,
            Err(e) => return Ok(Err(format!("Could not prepare skills: {e}"))),
        }
    };
    for m in &missing {
        em.system(format!("Skill not found on this runner, skipped: {m}"))
            .await?;
    }
    let mcp = mcp_config(spec);
    let mcp_path = run_dir.join("mcp.json");
    if let Some((json, _)) = &mcp {
        if let Err(e) = write_private(&mcp_path, &json.to_string()) {
            return Ok(Err(format!("Could not write the MCP config: {e}")));
        }
    }
    if !read_only
        && spec.agent.permission_mode == PermissionMode::BypassPermissions
        && !cfg.allow_bypass
    {
        em.system(
            "bypassPermissions is disabled on this server (BOBA_ALLOW_BYPASS); using acceptEdits",
        )
        .await?;
    }
    let args = claude_args(
        cfg,
        spec,
        session_id,
        plugin_dir.as_deref(),
        mcp.as_ref()
            .map(|(_, tools)| (mcp_path.as_path(), tools.as_slice())),
    );
    let mut child = match build_command(cfg, spec, place, &args).and_then(|mut c| c.spawn()) {
        Ok(c) => c,
        Err(e) => {
            return Ok(Err(format!(
                "Could not start Claude Code ({}): {e}. Is it installed and on PATH?",
                cfg.claude_bin.display()
            )))
        }
    };
    let guard = GroupGuard::new(&child);
    let mut stdin = child.stdin.take();
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        guard.terminate(&mut child, Duration::from_secs(1)).await;
        return Ok(Err("Claude Code started without stdio pipes".into()));
    };
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();

    write_line(&mut stdin, &stream::user_message(&place.prompt)).await;
    let mut sent: u32 = 1;
    let mut replayed: u32 = 0;

    let wt = place.dir.to_string_lossy().into_owned();
    let mut parser = StreamParser::new();
    let mut pending_perms: HashMap<String, (String, Value)> = HashMap::new();
    let mut result: Option<ResultInfo> = None;
    let mut last_text: Option<String> = None;
    let mut stderr_tail: Vec<String> = Vec::new();
    let mut stderr_open = true;
    let mut tool_calls = 0u32;
    let mut cost = 0.0f64;
    let mut timed_out = false;
    let cap = match spec.kind {
        RunKind::Work => 0.85,
        _ => 0.9,
    };
    let deadline = tokio::time::sleep(cfg.run_timeout);
    tokio::pin!(deadline);

    // Stops (cancel / closed) must kill the group before returning.
    macro_rules! stop {
        ($e:expr) => {{
            guard.terminate(&mut child, Duration::from_secs(5)).await;
            return Err($e);
        }};
    }
    macro_rules! emit {
        ($fut:expr) => {
            if $fut.await.is_err() {
                stop!(Stop::Closed);
            }
        };
    }

    emit!(em.progress(0.03, "Starting Claude Code"));
    loop {
        tokio::select! {
            line = out.next_line() => {
                let Ok(Some(line)) = line else { break };
                for ev in parser.parse_line(&line) {
                    match ev {
                        Event::Init { session_id: sid, model, skills, mcp_failed } => {
                            let n = skills.iter().filter(|s| s.starts_with("boba:")).count();
                            emit!(em.system(format!(
                                "Claude Code started · model {} · session {sid} · {n} agent skill(s)",
                                model.as_deref().unwrap_or("default")
                            )));
                            if !sid.is_empty() && sid != session_id {
                                emit!(em.system(format!("Claude Code continued as session {sid} (asked for {session_id})")));
                            }
                            for name in mcp_failed {
                                emit!(em.log(LogKind::Error, format!("MCP server \"{name}\" failed to start or connect"), None));
                            }
                        }
                        Event::Log { kind, text, data } => {
                            if kind == LogKind::Text {
                                last_text = Some(text.clone());
                            }
                            emit!(em.log(kind, text, data));
                        }
                        Event::ToolCall { name, input } => {
                            tool_calls += 1;
                            emit!(em.progress(prompt::progress_for(tool_calls, cap), prompt::activity(&name, &input, &wt)));
                        }
                        Event::Tokens { input, output } => emit!(em.usage(cost, input, output)),
                        Event::Permission { request_id, tool, input } => {
                            if read_only {
                                let line = stream::permission_response(&request_id, false, &input, "This run is read-only. Finish with your answer block.");
                                write_line(&mut stdin, &line).await;
                                emit!(em.system(format!("Denied {tool} (read-only run)")));
                            } else {
                                pending_perms.insert(request_id.clone(), (tool.clone(), input.clone()));
                                emit!(em.progress(prompt::progress_for(tool_calls, cap), format!("Waiting for permission: {tool}")));
                                emit!(em.send(RunUpdate::PermissionRequest { request_id, tool, input }));
                            }
                        }
                        Event::UnsupportedControl { request_id, subtype } => {
                            tracing::debug!(%subtype, "unsupported control request");
                            write_line(&mut stdin, &stream::control_error(&request_id, &format!("unsupported: {subtype}"))).await;
                        }
                        Event::Replay => replayed += 1,
                        Event::Result(r) => {
                            cost = cost.max(r.cost_usd);
                            emit!(em.usage(cost, r.input_tokens, r.output_tokens));
                            if !r.is_error {
                                if let Some(t) = r.text.as_ref().filter(|t| !t.trim().is_empty()) {
                                    last_text = Some(t.clone());
                                }
                            }
                            result = Some(r);
                            // Every message we sent was taken: end the conversation.
                            if replayed >= sent {
                                stdin = None;
                            }
                        }
                    }
                }
            }
            line = err.next_line(), if stderr_open => match line {
                Ok(Some(l)) if !l.trim().is_empty() => {
                    stderr_tail.push(l.clone());
                    if stderr_tail.len() > 20 {
                        stderr_tail.remove(0);
                    }
                    emit!(em.log(LogKind::Error, l, None));
                }
                Ok(Some(_)) => {}
                _ => stderr_open = false,
            },
            c = rx.recv() => match c {
                None | Some(Control::Cancel) => stop!(Stop::Cancelled),
                Some(Control::Message(text)) => {
                    if write_line(&mut stdin, &stream::user_message(&text)).await {
                        sent += 1;
                        emit!(em.user(text));
                    } else {
                        emit!(em.system(format!("Message not delivered, the run is finishing: {}", first_line(&text, 80))));
                    }
                }
                Some(Control::Permission { request_id, allow }) => match pending_perms.remove(&request_id) {
                    Some((tool, input)) => {
                        let line = stream::permission_response(&request_id, allow, &input, "A teammate denied this in Boba Agent. Continue without it or explain what you need.");
                        write_line(&mut stdin, &line).await;
                        emit!(em.system(format!("{} {tool}", if allow { "Allowed" } else { "Denied" })));
                    }
                    None => tracing::debug!(%request_id, "answer for an unknown permission request"),
                },
            },
            _ = &mut deadline => {
                timed_out = true;
                guard.terminate(&mut child, Duration::from_secs(5)).await;
                break;
            }
        }
    }
    drop(stdin);
    let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(s)) => Some(s),
        _ => {
            guard.terminate(&mut child, Duration::from_secs(2)).await;
            None
        }
    };
    guard.kill(); // background leftovers (dev servers started by Bash, …)

    if timed_out {
        return Ok(Err(format!(
            "Timed out after {} minutes",
            cfg.run_timeout.as_secs() / 60
        )));
    }
    let Some(mut r) = result else {
        let tail = stderr_tail.join("\n");
        return Ok(Err(format!(
            "Claude Code exited ({}) without a result{}",
            status.map_or_else(|| "killed".to_string(), |s| s.to_string()),
            if tail.is_empty() {
                String::new()
            } else {
                format!(": {}", first_line(&tail, 400))
            }
        )));
    };
    if r.is_error {
        return Ok(Err(describe_error(&r, spec)));
    }
    if r.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
        r.text = last_text;
    }
    Ok(Ok(r))
}

fn describe_error(r: &ResultInfo, spec: &RunSpec) -> String {
    match r.subtype.as_str() {
        "error_max_turns" => format!(
            "Reached the turn limit ({} turns){}",
            r.num_turns,
            spec.agent
                .max_turns
                .map(|m| format!(", max_turns = {m}"))
                .unwrap_or_default()
        ),
        "error_max_budget_usd" => "Reached the budget limit".into(),
        _ => {
            let detail = r
                .text
                .clone()
                .filter(|t| !t.trim().is_empty())
                .or_else(|| (!r.errors.is_empty()).then(|| r.errors.join("; ")))
                .unwrap_or_else(|| r.subtype.clone());
            format!("Claude Code failed: {}", first_line(&detail, 400))
        }
    }
}

async fn finish_pr(
    cfg: &ClaudeConfig,
    spec: &RunSpec,
    place: &Place,
    title: &str,
    label: &str,
    result: &ResultInfo,
    em: &Emitter,
) -> Result<RunOutcome, Stop> {
    let wt = &place.dir;
    let summary = result
        .text
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let fail = |msg: String| Ok(emit::failed(msg));
    let Some(repo_spec) = &spec.repo else {
        return fail("no repo to open a pull request in".into());
    };

    em.progress(0.9, "Committing").await?;
    match cfg.tools.commit_leftovers(wt, title).await {
        Ok(true) => em.system("Committed the remaining changes").await?,
        Ok(false) => {}
        Err(e) => return fail(format!("Could not commit: {e:#}")),
    }
    let ahead = cfg.tools.commits_ahead(wt, &place.base).await.unwrap_or(0);
    if ahead == 0 {
        return fail(format!(
            "The agent finished without any change on {}",
            place.branch
        ));
    }
    let repo = PathBuf::from(&repo_spec.path);
    if !cfg.open_prs || !cfg.tools.has_remote(&repo).await {
        em.system(format!(
            "{ahead} commit(s) on {} (no push: no origin remote or PRs disabled)",
            place.branch
        ))
        .await?;
        return Ok(RunOutcome {
            summary,
            ..emit::outcome(RunStatus::Succeeded)
        });
    }
    em.progress(0.94, "Pushing").await?;
    if let Err(e) = cfg.tools.push(wt, &place.branch).await {
        return fail(format!("Could not push {}: {e:#}", place.branch));
    }
    em.system(format!(
        "Pushed {ahead} commit(s) to origin/{}",
        place.branch
    ))
    .await?;
    em.progress(0.97, "Opening pull request").await?;
    let body = format!(
        "{}\n\n---\n{label} · opened by Boba Agent ({}, {})",
        summary.as_deref().unwrap_or("(no summary)"),
        spec.agent.name,
        spec.agent.role
    );
    match cfg
        .tools
        .open_pr(wt, &place.branch, &repo_spec.default_branch, title, &body)
        .await
    {
        Ok((number, url, created)) => {
            em.send(RunUpdate::PrOpened {
                number,
                url: url.clone(),
            })
            .await?;
            em.system(format!(
                "{} PR #{number}: {url}",
                if created { "Opened" } else { "Updated" }
            ))
            .await?;
        }
        Err(e) => {
            return fail(format!(
                "Pushed {} but could not open the PR: {e:#}",
                place.branch
            ))
        }
    }
    em.progress(1.0, "Done").await?;
    Ok(RunOutcome {
        summary,
        ..emit::outcome(RunStatus::Succeeded)
    })
}

fn finish_review(result: &ResultInfo) -> RunOutcome {
    let text = result.text.clone().unwrap_or_default();
    match prompt::parse_verdict(&text) {
        Ok(v) => {
            let summary = text
                .split("```boba-verdict")
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            RunOutcome {
                summary: Some(if summary.is_empty() {
                    v.feedback.clone()
                } else {
                    summary
                }),
                verdict: Some(v.verdict),
                feedback: (v.verdict == ReviewVerdict::ChangesRequested).then_some(v.feedback),
                ..emit::outcome(RunStatus::Succeeded)
            }
        }
        Err(e) => emit::failed(format!("Invalid review: {e}")),
    }
}

fn finish_command(result: &ResultInfo) -> RunOutcome {
    let text = result.text.clone().unwrap_or_default();
    match prompt::parse_plan(&text) {
        Ok(plan) => RunOutcome {
            summary: plan["question"]
                .as_str()
                .or_else(|| plan["reply"].as_str())
                .map(str::to_string),
            plan: Some(plan),
            ..emit::outcome(RunStatus::Succeeded)
        },
        Err(e) => emit::failed(format!("Invalid plan: {e}")),
    }
}

#[cfg(test)]
mod tests;

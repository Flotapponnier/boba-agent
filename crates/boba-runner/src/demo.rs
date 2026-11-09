//! `DemoRunner`: believable simulated runs, no Claude Code or API key needed.
//!
//! It walks a scripted run (explore, plan, edit, test, commit, PR) whose files, commands and
//! outputs are derived from the ticket, streaming the same [`RunUpdate`]s the real runner
//! does: every `LogKind`, `tool_use` / `tool_result` pairs with real Claude Code shapes,
//! progress + activity, cumulative usage, a PR, and for review runs a `boba-verdict`.
//!
//! Everything is seeded from the run id, so a given run replays identically.
//!
//! Env knobs (see [`DemoConfig::from_env`]):
//! * `BOBA_DEMO_SPEED` – time multiplier, `2` = twice as fast, `0` = no delays (default 1).
//! * `BOBA_DEMO_FAIL_RATE` – probability in 0..=1 that a work run fails midway (default 0).
//! * `BOBA_MAX_CONCURRENT_RUNS` – runs beyond this wait for a slot (default 4).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use boba_core::automation::TicketSource;
use boba_core::runner::{RunOutcome, RunSpec, RunUpdate, TicketSpec};
use boba_core::{Id, PermissionMode, ReviewVerdict, RunKind, RunStatus, Skill};
use serde_json::json;
use tokio::time::Instant;

use crate::emit::{self, Emitter};
use crate::table::{Control, ControlRx, RunTable, Stop};
use crate::util::{slugify, ticket_branch, Rng};
use crate::{Runner, UpdateTx};

#[derive(Debug, Clone)]
pub struct DemoConfig {
    pub id: Id,
    pub name: String,
    /// Used only to render believable worktree paths (`<data_dir>/worktrees/T-<n>`).
    pub data_dir: PathBuf,
    pub max_concurrent_runs: u32,
    /// Time multiplier: 1.0 = real-time pacing (~40 s per work run), 0 = instant.
    pub speed: f64,
    /// Probability that a work run fails midway (to exercise error paths in the UI).
    pub fail_rate: f64,
    /// First fake PR number handed out.
    pub first_pr_number: i64,
}

impl DemoConfig {
    pub fn new(id: Id, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            id,
            name: "demo".into(),
            data_dir: data_dir.into(),
            max_concurrent_runs: 4,
            speed: 1.0,
            fail_rate: 0.0,
            first_pr_number: 101,
        }
    }

    /// [`DemoConfig::new`] plus `BOBA_DEMO_SPEED`, `BOBA_DEMO_FAIL_RATE`,
    /// `BOBA_MAX_CONCURRENT_RUNS`. Invalid values are ignored with a warning.
    pub fn from_env(id: Id, data_dir: impl Into<PathBuf>) -> Self {
        fn var<T: std::str::FromStr>(key: &str) -> Option<T> {
            let raw = std::env::var(key).ok()?;
            let parsed = raw.trim().parse().ok();
            if parsed.is_none() {
                tracing::warn!("ignoring invalid {key}={raw:?}");
            }
            parsed
        }
        let mut cfg = Self::new(id, data_dir);
        if let Some(s) = var::<f64>("BOBA_DEMO_SPEED").filter(|s| s.is_finite() && *s >= 0.0) {
            cfg.speed = s;
        }
        if let Some(f) = var::<f64>("BOBA_DEMO_FAIL_RATE").filter(|f| (0.0..=1.0).contains(f)) {
            cfg.fail_rate = f;
        }
        if let Some(n) = var::<u32>("BOBA_MAX_CONCURRENT_RUNS").filter(|n| *n > 0) {
            cfg.max_concurrent_runs = n;
        }
        cfg
    }
}

struct Inner {
    cfg: DemoConfig,
    table: Arc<RunTable>,
    next_pr: AtomicI64,
}

/// Cheap to clone (shared state behind an `Arc`).
#[derive(Clone)]
pub struct DemoRunner {
    inner: Arc<Inner>,
}

impl DemoRunner {
    pub fn new(cfg: DemoConfig) -> Self {
        let table = Arc::new(RunTable::new(cfg.max_concurrent_runs));
        let next_pr = AtomicI64::new(cfg.first_pr_number);
        Self {
            inner: Arc::new(Inner {
                cfg,
                table,
                next_pr,
            }),
        }
    }

    /// Runs currently queued or executing.
    pub fn active_runs(&self) -> usize {
        self.inner.table.len()
    }
}

#[async_trait]
impl Runner for DemoRunner {
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
        self.inner.table.spawn(spec.run_id, em, move |em, rx| {
            run_script(inner, spec, em, rx)
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

// ------------------------------------------------------------------ script machinery

type Step<T = ()> = Result<T, Stop>;

struct Ctx {
    em: Emitter,
    rx: ControlRx,
    speed: f64,
    rng: Rng,
    tool_seq: u32,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    /// Answers that arrived before we started waiting for them.
    answers: HashMap<String, bool>,
}

impl Ctx {
    fn scaled(&self, ms: u64) -> Duration {
        if self.speed <= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(ms as f64 / 1000.0 / self.speed)
        }
    }

    /// Sleep `ms` (scaled, jittered ±25%), handling human messages and cancel meanwhile.
    async fn pause(&mut self, ms: u64) -> Step {
        let jittered = ms * self.rng.range(75, 125) / 100;
        let deadline = Instant::now() + self.scaled(jittered);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return Ok(()),
                c = self.rx.recv() => self.on_control(c).await?,
            }
        }
    }

    async fn on_control(&mut self, c: Option<Control>) -> Step {
        match c {
            None | Some(Control::Cancel) => Err(Stop::Cancelled),
            Some(Control::Message(text)) => {
                self.em.user(text.clone()).await?;
                let delay = self.scaled(900);
                tokio::time::sleep(delay).await;
                let reply = reply_to(&text, &mut self.rng);
                self.em.text(reply).await?;
                self.spend(1_800, 120).await
            }
            Some(Control::Permission { request_id, allow }) => {
                self.answers.insert(request_id, allow);
                Ok(())
            }
        }
    }

    async fn ask_permission(&mut self, tool: &str, input: serde_json::Value) -> Step<bool> {
        let request_id = format!("perm_{}", uuid::Uuid::now_v7().simple());
        self.em
            .send(RunUpdate::PermissionRequest {
                request_id: request_id.clone(),
                tool: tool.into(),
                input,
            })
            .await?;
        loop {
            if let Some(allow) = self.answers.remove(&request_id) {
                return Ok(allow);
            }
            let c = self.rx.recv().await;
            self.on_control(c).await?;
        }
    }

    fn next_tool_id(&mut self) -> String {
        self.tool_seq += 1;
        format!(
            "toolu_demo_{:02}{:08x}",
            self.tool_seq,
            self.rng.next_u64() as u32
        )
    }

    /// A tool call, a pause while it "runs", then its result.
    async fn tool(
        &mut self,
        name: &str,
        input: serde_json::Value,
        ms: u64,
        output: &str,
        is_error: bool,
    ) -> Step {
        let id = self.next_tool_id();
        self.em.tool_use(&id, name, input).await?;
        self.pause(ms).await?;
        self.em.tool_result(&id, output, is_error).await?;
        let out_tokens = self.rng.range(60, 400);
        self.spend((output.len() as u64 / 3).max(200) + 1_500, out_tokens)
            .await
    }

    /// Grow cumulative usage (Sonnet-like pricing, cache reads make input cheap).
    async fn spend(&mut self, input: u64, output: u64) -> Step {
        self.input_tokens += input;
        self.output_tokens += output;
        self.cost_usd = self.input_tokens as f64 * 1.2e-6 + self.output_tokens as f64 * 15e-6;
        self.em
            .usage(
                (self.cost_usd * 10_000.0).round() / 10_000.0,
                self.input_tokens,
                self.output_tokens,
            )
            .await
            .map_err(Into::into)
    }
}

fn reply_to(text: &str, rng: &mut Rng) -> String {
    let short = emit::first_line(text, 60);
    let replies = [
        format!("Got it: \"{short}\". I'll fold that into what I'm doing."),
        format!("Thanks, noted (\"{short}\"). Adjusting the plan accordingly."),
        format!("Understood. I'll take \"{short}\" into account before I commit."),
    ];
    rng.pick(&replies).clone()
}

async fn run_script(
    inner: Arc<Inner>,
    spec: RunSpec,
    em: Emitter,
    rx: ControlRx,
) -> Step<RunOutcome> {
    let mut ctx = Ctx {
        em,
        rx,
        speed: inner.cfg.speed,
        rng: Rng::new(spec.run_id.as_u128() as u64 ^ (spec.run_id.as_u128() >> 64) as u64),
        tool_seq: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        answers: HashMap::new(),
    };
    match spec.kind {
        RunKind::Work | RunKind::Review => {
            let Some(t) = spec.ticket.clone() else {
                return Ok(emit::failed(format!(
                    "{:?} run without a ticket",
                    spec.kind
                )));
            };
            let plan = Plan::new(&t.title, &t.tags, &t.body, &mut ctx.rng);
            if spec.kind == RunKind::Work {
                work(&inner, &spec, &t, &plan, &mut ctx).await
            } else {
                review(&inner, &spec, &t, &plan, &mut ctx).await
            }
        }
        RunKind::Job => job::run(&inner, &spec, &mut ctx).await,
        RunKind::Command => concierge::run(&spec, &mut ctx).await,
    }
}

fn default_branch(spec: &RunSpec) -> &str {
    spec.repo
        .as_ref()
        .map(|r| r.default_branch.as_str())
        .unwrap_or("main")
}

/// `T-42` for tickets, `job-<slug>` for the synthetic ticket of a PR job (number 0).
fn ticket_label(t: &TicketSpec) -> String {
    if t.number > 0 {
        format!("T-{}", t.number)
    } else {
        format!("job-{}", slugify(&t.title))
    }
}

// ------------------------------------------------------------------ fake project

#[derive(Clone, Copy, PartialEq, Debug)]
enum Flavour {
    Web,
    Rust,
    Python,
}

/// Files, commands and outputs for one run, derived from the ticket.
struct Plan {
    flavour: Flavour,
    /// e.g. "DarkModeToggle" / "dark_mode_toggle"
    camel: String,
    snake: String,
    keyword: String,
    explore: String,
    target: String,
    test_file: String,
    glob: &'static str,
    test_cmd: &'static str,
    lint_cmd: &'static str,
}

impl Plan {
    fn new(title: &str, tags: &[String], body: &str, rng: &mut Rng) -> Self {
        let hay = format!("{title} {} {body}", tags.join(" ")).to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| hay.contains(w));
        let flavour = if has(&[
            "ui", "front", "css", "react", "button", "page", "web", "design", "modal", "seo",
        ]) {
            Flavour::Web
        } else if has(&[
            "rust", "api", "server", "backend", "endpoint", "db", "database", "cli",
        ]) {
            Flavour::Rust
        } else if has(&["python", "script", "data", "ml", "notebook"]) {
            Flavour::Python
        } else {
            *rng.pick(&[Flavour::Web, Flavour::Rust, Flavour::Python])
        };
        let words: Vec<String> = slugify(title)
            .split('-')
            .filter(|w| !STOP_WORDS.contains(w))
            .take(3)
            .map(str::to_string)
            .collect();
        let words = if words.is_empty() {
            vec!["feature".to_string()]
        } else {
            words
        };
        let camel: String = words
            .iter()
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                    .unwrap_or_default()
            })
            .collect();
        let snake = words.join("_");
        let keyword = words[0].clone();
        let (explore, target, test_file, glob, test_cmd, lint_cmd) = match flavour {
            Flavour::Web => (
                "src/App.tsx".to_string(),
                format!("src/components/{camel}.tsx"),
                format!("src/components/{camel}.test.tsx"),
                "src/**/*.{ts,tsx}",
                "pnpm test --run",
                "pnpm lint && pnpm typecheck",
            ),
            Flavour::Rust => (
                "src/lib.rs".to_string(),
                format!("src/{snake}.rs"),
                format!("tests/{snake}.rs"),
                "**/*.rs",
                "cargo test",
                "cargo clippy --all-targets -- -D warnings",
            ),
            Flavour::Python => (
                "app/__init__.py".to_string(),
                format!("app/{snake}.py"),
                format!("tests/test_{snake}.py"),
                "**/*.py",
                "pytest -q",
                "ruff check . && mypy app",
            ),
        };
        Self {
            flavour,
            camel,
            snake,
            keyword,
            explore,
            target,
            test_file,
            glob,
            test_cmd,
            lint_cmd,
        }
    }

    fn file_list(&self) -> String {
        let mut files = vec![self.explore.clone(), self.target.clone()];
        files.extend(
            match self.flavour {
                Flavour::Web => [
                    "src/main.tsx",
                    "src/api/client.ts",
                    "src/hooks/useBoard.ts",
                    "src/styles/theme.css",
                ],
                Flavour::Rust => [
                    "src/main.rs",
                    "src/config.rs",
                    "src/error.rs",
                    "src/routes/mod.rs",
                ],
                Flavour::Python => [
                    "app/main.py",
                    "app/config.py",
                    "app/models.py",
                    "tests/conftest.py",
                ],
            }
            .map(String::from),
        );
        files.join("\n")
    }

    fn explore_content(&self) -> String {
        let lines: Vec<String> = match self.flavour {
            Flavour::Web => vec![
                "import { useState } from 'react'".into(),
                "import { Board } from './components/Board'".into(),
                "import { useBoard } from './hooks/useBoard'".into(),
                String::new(),
                "export function App() {".into(),
                "  const board = useBoard()".into(),
                "  const [filter, setFilter] = useState('')".into(),
                "  return <Board board={board} filter={filter} onFilter={setFilter} />".into(),
                "}".into(),
            ],
            Flavour::Rust => vec![
                "//! Crate root.".into(),
                "pub mod config;".into(),
                "pub mod error;".into(),
                "pub mod routes;".into(),
                String::new(),
                "pub use error::{Error, Result};".into(),
            ],
            Flavour::Python => vec![
                "\"\"\"Application package.\"\"\"".into(),
                "from .config import Settings".into(),
                "from .main import create_app".into(),
                String::new(),
                "__all__ = [\"Settings\", \"create_app\"]".into(),
            ],
        };
        numbered(&lines)
    }

    fn edit(&self) -> (String, String) {
        match self.flavour {
            Flavour::Web => (
                format!("export function {}() {{\n  return null\n}}", self.camel),
                format!(
                    "export function {c}({{ enabled, onChange }}: {c}Props) {{\n  return (\n    <button aria-pressed={{enabled}} onClick={{() => onChange(!enabled)}}>\n      {{enabled ? 'On' : 'Off'}}\n    </button>\n  )\n}}",
                    c = self.camel
                ),
            ),
            Flavour::Rust => (
                format!("pub fn {}() -> Result<()> {{\n    todo!()\n}}", self.snake),
                format!(
                    "pub fn {s}(cfg: &Config) -> Result<()> {{\n    let input = cfg.{k}.as_deref().ok_or(Error::Missing(\"{k}\"))?;\n    tracing::debug!(%input, \"{s}\");\n    Ok(())\n}}",
                    s = self.snake,
                    k = self.keyword
                ),
            ),
            Flavour::Python => (
                format!("def {}():\n    raise NotImplementedError", self.snake),
                format!(
                    "def {s}(settings: Settings) -> None:\n    value = settings.{k}\n    if value is None:\n        raise ValueError(\"{k} is required\")\n    log.debug(\"{s}: %s\", value)",
                    s = self.snake,
                    k = self.keyword
                ),
            ),
        }
    }

    fn test_content(&self) -> String {
        match self.flavour {
            Flavour::Web => format!(
                "import {{ render, screen, fireEvent }} from '@testing-library/react'\nimport {{ {c} }} from './{c}'\n\ntest('{c} toggles', () => {{\n  const onChange = vi.fn()\n  render(<{c} enabled={{false}} onChange={{onChange}} />)\n  fireEvent.click(screen.getByRole('button'))\n  expect(onChange).toHaveBeenCalledWith(true)\n}})\n",
                c = self.camel
            ),
            Flavour::Rust => format!(
                "use app::{{{s}, Config}};\n\n#[test]\nfn {s}_requires_{k}() {{\n    assert!({s}(&Config::default()).is_err());\n}}\n",
                s = self.snake,
                k = self.keyword
            ),
            Flavour::Python => format!(
                "import pytest\nfrom app.{s} import {s}\nfrom app import Settings\n\n\ndef test_{s}_requires_{k}():\n    with pytest.raises(ValueError):\n        {s}(Settings({k}=None))\n",
                s = self.snake,
                k = self.keyword
            ),
        }
    }

    fn test_output(&self, pass: bool) -> String {
        match (self.flavour, pass) {
            (Flavour::Web, true) => format!(
                " ✓ src/components/{c}.test.tsx (1 test) 18ms\n ✓ src/App.test.tsx (4 tests) 41ms\n\n Test Files  2 passed (2)\n      Tests  5 passed (5)\n   Duration  1.32s",
                c = self.camel
            ),
            (Flavour::Web, false) => format!(
                " ❯ src/components/{c}.test.tsx (1 test | 1 failed)\n   × {c} toggles\n     → expected \"spy\" to be called with arguments: [ true ]\n     Number of calls: 0\n\n Test Files  1 failed | 1 passed (2)\n      Tests  1 failed | 4 passed (5)",
                c = self.camel
            ),
            (Flavour::Rust, true) => format!(
                "   Compiling app v0.1.0\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 4.21s\n     Running tests/{s}.rs\n\nrunning 1 test\ntest {s}_requires_{k} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
                s = self.snake,
                k = self.keyword
            ),
            (Flavour::Rust, false) => format!(
                "   Compiling app v0.1.0\nerror[E0425]: cannot find value `Error` in this scope\n --> src/{s}.rs:3:44\n  |\n3 |     let input = cfg.{k}.as_deref().ok_or(Error::Missing(\"{k}\"))?;\n  |                                            ^^^^^ not found in this scope\n  |\nhelp: consider importing this enum\n  |\n1 + use crate::error::Error;\n  |\n\nerror: could not compile `app` (lib) due to 1 previous error",
                s = self.snake,
                k = self.keyword
            ),
            (Flavour::Python, true) => "....\n4 passed in 0.38s".to_string(),
            (Flavour::Python, false) => format!(
                "F...\n=================================== FAILURES ===================================\n_________________________ test_{s}_requires_{k} __________________________\nE   NameError: name 'log' is not defined\n\napp/{s}.py:5: NameError\n1 failed, 3 passed in 0.41s",
                s = self.snake,
                k = self.keyword
            ),
        }
    }

    fn fix(&self) -> (String, String) {
        match self.flavour {
            Flavour::Web => (
                format!("export function {c}({{ enabled, onChange }}: {c}Props) {{", c = self.camel),
                format!(
                    "export type {c}Props = {{ enabled: boolean; onChange: (next: boolean) => void }}\n\nexport function {c}({{ enabled, onChange }}: {c}Props) {{",
                    c = self.camel
                ),
            ),
            Flavour::Rust => ("use crate::config::Config;".into(), "use crate::config::Config;\nuse crate::error::Error;".into()),
            Flavour::Python => ("from app import Settings".into(), "import logging\n\nfrom app import Settings\n\nlog = logging.getLogger(__name__)".into()),
        }
    }
}

const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "for", "in", "on", "and", "or", "with", "add", "fix", "make",
    "update", "when", "is", "be", "should", "new", "support", "use",
];

fn numbered(lines: &[String]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>6}\t{l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pr_url(spec: &RunSpec, number: i64) -> String {
    let repo = spec.repo.as_ref();
    let base = repo
        .and_then(|r| r.remote_url.as_deref())
        .and_then(github_slug)
        .map(|s| format!("https://github.com/{s}"))
        .unwrap_or_else(|| {
            let name = repo
                .and_then(|r| std::path::Path::new(&r.path).file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".into());
            format!("https://github.com/boba-demo/{name}")
        });
    format!("{base}/pull/{number}")
}

/// `git@github.com:o/r.git` / `https://github.com/o/r(.git)` -> `o/r`.
pub(crate) fn github_slug(remote: &str) -> Option<String> {
    let rest = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.split_once("github.com/").map(|(_, r)| r))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (o, r) = (parts.next()?, parts.next()?);
    (!o.is_empty() && !r.is_empty()).then(|| format!("{o}/{r}"))
}

// ------------------------------------------------------------------ work run

async fn work(
    inner: &Inner,
    spec: &RunSpec,
    t: &TicketSpec,
    plan: &Plan,
    ctx: &mut Ctx,
) -> Step<RunOutcome> {
    let branch = t
        .branch
        .clone()
        .unwrap_or_else(|| ticket_branch(t.number, &t.title));
    let worktree = inner.cfg.data_dir.join("worktrees").join(ticket_label(t));
    let worktree = worktree.to_string_lossy().into_owned();
    let session_id = spec
        .resume_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let changes_round = t.review_rounds > 0 && t.review_feedback.is_some();

    ctx.em
        .send(RunUpdate::Started {
            worktree_path: worktree.clone(),
            branch: branch.clone(),
            session_id: Some(session_id.clone()),
        })
        .await?;
    if t.branch.is_some() {
        ctx.em
            .system(format!(
                "Worktree ready at {worktree} (reusing branch {branch})"
            ))
            .await?;
    } else {
        ctx.em
            .system(format!(
                "Worktree ready at {worktree} (new branch {branch} from {})",
                default_branch(spec)
            ))
            .await?;
    }
    ctx.em
        .system(format!(
            "Claude Code session {session_id}{} · model {} · permission mode {} · {} skill(s)",
            if spec.resume_session_id.is_some() {
                " (resumed)"
            } else {
                ""
            },
            spec.agent.model.as_deref().unwrap_or("default"),
            mode_name(spec.agent.permission_mode),
            spec.skill_paths.len()
        ))
        .await?;
    ctx.em.progress(0.03, "Reading the ticket").await?;
    ctx.pause(1_200).await?;

    if changes_round {
        let fb = t.review_feedback.as_deref().unwrap_or_default();
        ctx.em
            .text(format!(
                "Picking T-{} back up for review round {}. The reviewer asked for:\n\n> {}\n\nI'll address that first, then re-run the checks.",
                t.number,
                t.review_rounds + 1,
                emit::first_line(fb, 200)
            ))
            .await?;
    } else {
        ctx.em
            .text(format!(
                "I'll start by getting oriented in the codebase, then implement \"{}\" with tests.",
                t.title
            ))
            .await?;
    }
    if let Some(extra) = &spec.extra_instructions {
        ctx.em
            .text(format!(
                "Noted the extra instructions: {}",
                emit::first_line(extra, 160)
            ))
            .await?;
    }
    ctx.spend(9_000, 180).await?;
    ctx.pause(900).await?;

    ctx.em.progress(0.08, "Exploring the codebase").await?;
    let files = plan.file_list();
    ctx.tool("Glob", json!({ "pattern": plan.glob }), 600, &files, false)
        .await?;

    ctx.em
        .progress(0.14, format!("Reading {}", plan.explore))
        .await?;
    let content = plan.explore_content();
    ctx.tool(
        "Read",
        json!({ "file_path": abs(&worktree, &plan.explore) }),
        700,
        &content,
        false,
    )
    .await?;

    ctx.em
        .progress(0.2, format!("Searching for \"{}\"", plan.keyword))
        .await?;
    let grep_out = format!(
        "{}:3\n{}:12\n{}:1",
        plan.target, plan.explore, plan.test_file
    );
    ctx.tool(
        "Grep",
        json!({ "pattern": plan.keyword, "output_mode": "files_with_matches" }),
        500,
        &grep_out,
        false,
    )
    .await?;

    ctx.em
        .thinking(format!(
            "The entry point is {} and the natural home for this change is {}. I should keep the public API small, \
             cover the main path with a test in {}, and run `{}` before committing.",
            plan.explore, plan.target, plan.test_file, plan.test_cmd
        ))
        .await?;
    ctx.pause(1_400).await?;

    ctx.em.progress(0.26, "Planning").await?;
    let todos = json!({ "todos": [
        { "content": format!("Implement {} in {}", plan.camel, plan.target), "status": "in_progress", "activeForm": format!("Implementing {}", plan.camel) },
        { "content": format!("Add tests in {}", plan.test_file), "status": "pending", "activeForm": "Adding tests" },
        { "content": format!("Run {} and {}", plan.test_cmd, plan.lint_cmd), "status": "pending", "activeForm": "Running checks" },
    ]});
    ctx.tool(
        "TodoWrite",
        todos,
        300,
        "Todos have been modified successfully. Ensure that you continue to use the todo list to track your progress.",
        false,
    )
    .await?;

    ctx.em
        .progress(0.32, format!("Reading {}", plan.target))
        .await?;
    let (old, new) = plan.edit();
    let target_content = numbered(&old.lines().map(String::from).collect::<Vec<_>>());
    ctx.tool(
        "Read",
        json!({ "file_path": abs(&worktree, &plan.target) }),
        500,
        &target_content,
        false,
    )
    .await?;

    ctx.em
        .progress(0.42, format!("Editing {}", plan.target))
        .await?;
    let target_abs = abs(&worktree, &plan.target);
    ctx.tool(
        "Edit",
        json!({ "file_path": target_abs, "old_string": old, "new_string": new }),
        1_100,
        &format!("The file {target_abs} has been updated successfully."),
        false,
    )
    .await?;

    ctx.em.progress(0.52, "Writing tests").await?;
    let test_abs = abs(&worktree, &plan.test_file);
    ctx.tool(
        "Write",
        json!({ "file_path": test_abs, "content": plan.test_content() }),
        900,
        &format!("File created successfully at: {test_abs}"),
        false,
    )
    .await?;

    // Permission prompt in default mode, before the first shell command.
    let mut may_run = true;
    if spec.agent.permission_mode == PermissionMode::Default {
        ctx.em
            .progress(0.58, "Waiting for permission to run tests")
            .await?;
        may_run = ctx
            .ask_permission(
                "Bash",
                json!({ "command": plan.test_cmd, "description": "Run the test suite" }),
            )
            .await?;
        if !may_run {
            ctx.em.system("Permission denied: Bash").await?;
            ctx.em
                .text("Understood, I won't run shell commands. I'll leave the tests for CI and note it in the PR.")
                .await?;
        }
    }

    if may_run {
        ctx.em.progress(0.62, "Running tests").await?;
        let fails_first = ctx.rng.chance(0.4);
        let cmd_input = json!({ "command": plan.test_cmd, "description": "Run the test suite", "timeout": 120000 });
        ctx.tool(
            "Bash",
            cmd_input.clone(),
            2_400,
            &plan.test_output(!fails_first),
            fails_first,
        )
        .await?;

        if inner.cfg.fail_rate > 0.0 && ctx.rng.chance(inner.cfg.fail_rate) {
            ctx.pause(600).await?;
            let err = "API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}";
            ctx.em.error(err).await?;
            return Ok(emit::failed(err));
        }

        if fails_first {
            ctx.em
                .thinking("The test run failed; reading the output, the fix is small and local. Let me patch it and re-run.")
                .await?;
            ctx.pause(900).await?;
            ctx.em
                .progress(0.68, format!("Fixing {}", plan.target))
                .await?;
            let (o, n) = plan.fix();
            let fix_file = if plan.flavour == Flavour::Python {
                test_abs.clone()
            } else {
                target_abs.clone()
            };
            ctx.tool(
                "Edit",
                json!({ "file_path": fix_file, "old_string": o, "new_string": n }),
                700,
                &format!("The file {fix_file} has been updated successfully."),
                false,
            )
            .await?;
            ctx.em.progress(0.72, "Re-running tests").await?;
            ctx.tool("Bash", cmd_input, 2_000, &plan.test_output(true), false)
                .await?;
        }

        ctx.em.progress(0.8, "Linting").await?;
        ctx.tool(
            "Bash",
            json!({ "command": plan.lint_cmd, "description": "Lint and typecheck" }),
            1_600,
            "",
            false,
        )
        .await?;

        ctx.em.progress(0.85, "Reviewing the diff").await?;
        let stat = format!(
            " {} | 14 ++++++++++----\n {} | 11 +++++++++++\n 2 files changed, 21 insertions(+), 4 deletions(-)",
            plan.target, plan.test_file
        );
        ctx.tool(
            "Bash",
            json!({ "command": "git diff --stat", "description": "Show diff stat" }),
            400,
            &stat,
            false,
        )
        .await?;
    }

    let summary = format!(
        "{} `{}` in `{}`{}. Added `{}`.{}",
        if changes_round {
            "Addressed the review feedback on"
        } else {
            "Implemented"
        },
        plan.camel,
        plan.target,
        if changes_round {
            " and tightened the edge cases"
        } else {
            ""
        },
        plan.test_file,
        if may_run {
            format!(" `{}` and `{}` pass.", plan.test_cmd, plan.lint_cmd)
        } else {
            " Tests were not run (permission denied).".into()
        }
    );
    ctx.em.text(format!("Done. {summary}")).await?;
    ctx.spend(4_000, 260).await?;
    ctx.pause(700).await?;

    ctx.em.progress(0.92, "Committing").await?;
    ctx.em
        .system(format!(
            "Committed 2 files on {branch}: \"T-{}: {}\"",
            t.number, t.title
        ))
        .await?;
    ctx.pause(800).await?;
    ctx.em.progress(0.96, "Opening pull request").await?;
    ctx.em.system(format!("Pushed {branch} to origin")).await?;
    ctx.pause(900).await?;

    let (number, url, updated) = match (t.pr_number, &t.pr_url) {
        (Some(n), Some(u)) => (n, u.clone(), true),
        (Some(n), None) => (n, pr_url(spec, n), true),
        _ => {
            let n = inner.next_pr.fetch_add(1, Ordering::Relaxed);
            (n, pr_url(spec, n), false)
        }
    };
    ctx.em
        .send(RunUpdate::PrOpened {
            number,
            url: url.clone(),
        })
        .await?;
    ctx.em
        .system(format!(
            "{} PR #{number}: {url}",
            if updated { "Updated" } else { "Opened" }
        ))
        .await?;
    ctx.em.progress(1.0, "Done").await?;

    Ok(RunOutcome {
        summary: Some(summary),
        ..emit::outcome(RunStatus::Succeeded)
    })
}

fn abs(worktree: &str, rel: &str) -> String {
    format!("{}/{rel}", worktree.trim_end_matches('/'))
}

fn mode_name(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::BypassPermissions => "bypassPermissions",
    }
}

// ------------------------------------------------------------------ review run

/// Approval odds: 75% on the first review, then +10 points per round, capped at 97%.
pub fn approval_probability(review_rounds: u32) -> f64 {
    (0.75 + 0.10 * review_rounds as f64).min(0.97)
}

async fn review(
    inner: &Inner,
    spec: &RunSpec,
    t: &TicketSpec,
    plan: &Plan,
    ctx: &mut Ctx,
) -> Step<RunOutcome> {
    let branch = match &t.source {
        TicketSource::GithubPr { head_branch, .. } => head_branch.clone(),
        _ => t
            .branch
            .clone()
            .unwrap_or_else(|| ticket_branch(t.number, &t.title)),
    };
    let worktree = inner
        .cfg
        .data_dir
        .join("worktrees")
        .join(format!("{}-review", ticket_label(t)))
        .to_string_lossy()
        .into_owned();
    ctx.em
        .send(RunUpdate::Started {
            worktree_path: worktree.clone(),
            branch: branch.clone(),
            session_id: Some(uuid::Uuid::now_v7().to_string()),
        })
        .await?;
    let pr = t
        .pr_number
        .map(|n| format!("PR #{n}"))
        .unwrap_or_else(|| format!("branch {branch}"));
    ctx.em
        .system(format!(
            "Read-only review of {pr} in {worktree} · plan mode · read tools only"
        ))
        .await?;
    ctx.em.progress(0.05, format!("Opening {pr}")).await?;
    ctx.pause(900).await?;
    ctx.em
        .text(format!(
            "Reviewing {pr} for T-{}: \"{}\". Let me look at the change.",
            t.number, t.title
        ))
        .await?;
    ctx.spend(8_000, 90).await?;

    if let Some(n) = t.pr_number {
        let view = format!(
            "title:\tT-{}: {}\nstate:\tOPEN\nauthor:\tboba-agent\nlabels:\t{}\nadditions:\t21\ndeletions:\t4\n--\n{}",
            t.number,
            t.title,
            t.tags.join(", "),
            emit::first_line(&t.body, 200)
        );
        ctx.tool(
            "Bash",
            json!({ "command": format!("gh pr view {n}"), "description": "Show the PR" }),
            700,
            &view,
            false,
        )
        .await?;
    }
    ctx.em.progress(0.25, "Reading the diff").await?;
    let (old, new) = plan.edit();
    let diff = format!(
        "diff --git a/{f} b/{f}\n--- a/{f}\n+++ b/{f}\n@@ -1,3 +1,8 @@\n{}\n{}\ndiff --git a/{tf} b/{tf}\nnew file mode 100644\n--- /dev/null\n+++ b/{tf}\n@@ -0,0 +1,{} @@\n{}",
        old.lines().map(|l| format!("-{l}")).collect::<Vec<_>>().join("\n"),
        new.lines().map(|l| format!("+{l}")).collect::<Vec<_>>().join("\n"),
        plan.test_content().lines().count(),
        plan.test_content().lines().map(|l| format!("+{l}")).collect::<Vec<_>>().join("\n"),
        f = plan.target,
        tf = plan.test_file,
    );
    let diff_cmd = match t.pr_number {
        Some(n) => format!("gh pr diff {n}"),
        None => format!("git diff {}...{branch}", default_branch(spec)),
    };
    ctx.tool(
        "Bash",
        json!({ "command": diff_cmd, "description": "Show the diff" }),
        900,
        &diff,
        false,
    )
    .await?;

    ctx.em
        .progress(0.45, format!("Reading {}", plan.target))
        .await?;
    let content = numbered(&new.lines().map(String::from).collect::<Vec<_>>());
    ctx.tool(
        "Read",
        json!({ "file_path": abs(&worktree, &plan.target) }),
        700,
        &content,
        false,
    )
    .await?;
    ctx.em.progress(0.6, "Checking call sites").await?;
    ctx.tool(
        "Grep",
        json!({ "pattern": plan.camel.clone(), "output_mode": "content", "-n": true }),
        500,
        &format!(
            "{}:5:  {}\n{}:2:  {}",
            plan.explore, plan.camel, plan.test_file, plan.camel
        ),
        false,
    )
    .await?;
    ctx.em
        .progress(0.72, format!("Reading {}", plan.test_file))
        .await?;
    let tests = numbered(
        &plan
            .test_content()
            .lines()
            .map(String::from)
            .collect::<Vec<_>>(),
    );
    ctx.tool(
        "Read",
        json!({ "file_path": abs(&worktree, &plan.test_file) }),
        500,
        &tests,
        false,
    )
    .await?;

    let approved = ctx.rng.chance(approval_probability(t.review_rounds));
    ctx.em
        .thinking(if approved {
            "The change is scoped to the ticket, the test covers the main path and the error case, and nothing \
             outside the diff depends on the old behaviour. I don't see blockers."
        } else {
            "The happy path is covered, but there are gaps I would not merge as-is. I'll list them concretely \
             so the author can fix them in one round."
        })
        .await?;
    ctx.pause(1_500).await?;
    ctx.em.progress(0.9, "Writing the verdict").await?;

    let feedback = if approved {
        None
    } else {
        let options = [
            format!(
                "1. `{}` has no test for the error path; add one next to the existing case in `{}`.\n2. The new public item `{}` has no doc comment.",
                plan.target, plan.test_file, plan.camel
            ),
            format!(
                "1. `{}` swallows the failure case silently; surface it to the caller.\n2. Rename the test to describe the behaviour, not the function.",
                plan.target
            ),
            format!(
                "1. The change in `{}` is not wired into `{}`, so it is unreachable from the app.\n2. Please add a short note in the PR description on how to try it.",
                plan.target, plan.explore
            ),
        ];
        Some(ctx.rng.pick(&options).clone())
    };
    let verdict = if approved {
        ReviewVerdict::Approved
    } else {
        ReviewVerdict::ChangesRequested
    };
    let body = match &feedback {
        None => format!(
            "Looks good. The change in `{}` does what T-{} asks, it is covered by `{}`, and the diff stays in scope.",
            plan.target, t.number, plan.test_file
        ),
        Some(f) => format!("Close, but a few things need fixing before this can merge:\n\n{f}"),
    };
    let block = json!({
        "verdict": if approved { "approved" } else { "changes_requested" },
        "feedback": feedback.clone().unwrap_or_else(|| "LGTM".into()),
    });
    ctx.em
        .text(format!(
            "{body}\n\n```boba-verdict\n{}\n```",
            serde_json::to_string_pretty(&block).unwrap_or_default()
        ))
        .await?;
    ctx.spend(3_000, 320).await?;
    ctx.em.progress(1.0, "Review done").await?;

    Ok(RunOutcome {
        summary: Some(body),
        verdict: Some(verdict),
        feedback,
        ..emit::outcome(RunStatus::Succeeded)
    })
}

pub(crate) mod concierge;
mod job;

#[cfg(test)]
mod tests;

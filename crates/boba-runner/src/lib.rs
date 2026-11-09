//! # boba-runner
//!
//! Executes agent runs. Owned by the runner workstream (see CONTRIBUTING.md).
//!
//! Implementations:
//! * [`ClaudeCodeRunner`] – spawns the `claude` CLI headless (stream-json in and out) inside a
//!   dedicated git worktree, parses its event stream into [`RunUpdate`]s, answers permission
//!   prompts, commits/pushes and opens PRs. Covers work, review, job and command runs.
//! * [`DemoRunner`] – no Claude Code needed: simulates believable runs of every kind (log lines,
//!   progress, PR, review verdict, job reports, command plans) so the UI is playable out of the box.
//!
//! Both share [`table::RunTable`] (control channels, concurrency limit, exactly one `Finished`).
//!
//! The trait below is the contract the server codes against. Keep it stable.

pub mod claude;
pub mod demo;
pub mod emit;
pub mod git;
pub mod process;
pub mod remote;
pub mod skills;
pub mod table;
pub mod util;

pub use claude::{AuthMode, ClaudeCodeRunner, ClaudeConfig};
pub use demo::{DemoConfig, DemoRunner};

use async_trait::async_trait;
use boba_core::runner::{RunSpec, RunUpdate};
use boba_core::{Id, Skill};
use tokio::sync::mpsc;

/// Sender half handed to a runner for one run. The runner must send exactly one
/// `RunUpdate::Finished` and then drop it.
pub type UpdateTx = mpsc::Sender<RunUpdate>;

#[async_trait]
pub trait Runner: Send + Sync + 'static {
    /// Stable runner id (local runner: a fixed UUID persisted by the server).
    fn id(&self) -> Id;
    fn name(&self) -> &str;
    fn max_concurrent_runs(&self) -> u32;

    /// Start a run in the background and return immediately.
    async fn start(&self, spec: RunSpec, updates: UpdateTx) -> anyhow::Result<()>;
    /// Cancel a running run. Must still emit `Finished { status: Cancelled }`.
    async fn cancel(&self, run_id: Id) -> anyhow::Result<()>;
    /// Send a follow-up user message into a live run.
    async fn send_message(&self, run_id: Id, text: String) -> anyhow::Result<()>;
    /// Answer a `RunUpdate::PermissionRequest`.
    async fn answer_permission(
        &self,
        run_id: Id,
        request_id: String,
        allow: bool,
    ) -> anyhow::Result<()>;
    /// Skills visible on this machine (~/.claude/skills, repo .claude/skills, plugins,
    /// and Boba-managed skills under `boba_skills_dir`).
    async fn discover_skills(
        &self,
        repo_paths: &[String],
        boba_skills_dir: &str,
    ) -> anyhow::Result<Vec<Skill>>;
}

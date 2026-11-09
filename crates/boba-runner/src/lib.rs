//! # boba-runner
//!
//! Executes agent runs. Owned by the runner workstream (see CONTRIBUTING.md).
//!
//! Implementations planned:
//! * `ClaudeCodeRunner` – spawns the `claude` CLI in headless stream-json mode inside a
//!   dedicated git worktree, parses its event stream into [`RunUpdate`]s, opens PRs.
//! * `DemoRunner` – no Claude Code needed: simulates a believable run (log lines,
//!   progress, PR, review verdict) so the UI is fully playable out of the box.
//!
//! The trait below is the contract the server codes against. Keep it stable.

pub mod demo;
pub mod emit;
pub mod skills;
pub mod util;

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

//! Print one simulated run to the terminal:
//! `BOBA_DEMO_SPEED=4 cargo run -p boba-runner --example demo_run -- [work|review]`

use boba_core::runner::{AgentSpec, RepoSpec, RunSpec, RunUpdate, TicketSpec};
use boba_core::{PermissionMode, RunKind};
use boba_runner::{DemoConfig, DemoRunner, Runner};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let kind = match std::env::args().nth(1).as_deref() {
        Some("review") => RunKind::Review,
        _ => RunKind::Work,
    };
    let runner = DemoRunner::new(DemoConfig::from_env(Uuid::now_v7(), "./data"));
    let spec = RunSpec {
        run_id: Uuid::now_v7(),
        kind,
        agent: AgentSpec {
            id: Uuid::now_v7(),
            name: "Pinky".into(),
            role: "Dev".into(),
            system_prompt: String::new(),
            model: None,
            permission_mode: PermissionMode::AcceptEdits,
            allowed_tools: vec![],
            max_turns: None,
        },
        ticket: Some(TicketSpec {
            id: Uuid::now_v7(),
            number: 7,
            title: "Add rate limiting to the public API".into(),
            body: "Return 429 after 100 requests per minute per key.".into(),
            tags: vec!["backend".into()],
            branch: None,
            pr_number: (kind == RunKind::Review).then_some(12),
            pr_url: None,
            review_feedback: None,
            review_rounds: 0,
            source: Default::default(),
        }),
        job: None,
        command: None,
        repo: Some(RepoSpec {
            path: "/srv/repos/app".into(),
            default_branch: "main".into(),
            remote_url: Some("https://github.com/acme/app".into()),
        }),
        mcp_servers: vec![],
        board_tools: None,
        skill_paths: vec![],
        resume_session_id: None,
        extra_instructions: None,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    runner.start(spec, tx).await?;
    while let Some(u) = rx.recv().await {
        match u {
            RunUpdate::Log { line } => println!("[{:?}] {}", line.kind, line.text),
            other => println!("  <{}>", serde_json::to_string(&other)?),
        }
    }
    Ok(())
}

//! Run a real Claude Code work run (then a review of it) against a throwaway repo:
//! `cargo run -p boba-runner --example claude_run -- [model]`
//! Uses `tests/fixtures/fake-gh.sh` instead of GitHub; spends a few cents of tokens.

use std::path::Path;

use boba_core::runner::{AgentSpec, RepoSpec, RunSpec, RunUpdate, TicketSpec};
use boba_core::{PermissionMode, RunKind};
use boba_runner::{ClaudeCodeRunner, ClaudeConfig, Runner};
use uuid::Uuid;

async fn sh(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    boba_runner::git::run(Path::new("git"), args, cwd)
        .await
        .map(|_| ())
}

async fn go(r: &ClaudeCodeRunner, spec: RunSpec) -> anyhow::Result<Option<String>> {
    let run_id = spec.run_id;
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    r.start(spec, tx).await?;
    let mut branch = None;
    while let Some(u) = rx.recv().await {
        if let RunUpdate::PermissionRequest {
            request_id,
            tool,
            input,
        } = &u
        {
            println!("  ? permission {tool} {input} -> allowing");
            r.answer_permission(run_id, request_id.clone(), true)
                .await?;
        }
        match &u {
            RunUpdate::Log { line } => println!(
                "[{:?}] {}",
                line.kind,
                boba_runner::emit::first_line(&line.text, 160)
            ),
            RunUpdate::Started { branch: b, .. } => {
                branch = Some(b.clone());
                println!("  <{}>", serde_json::to_string(&u)?)
            }
            RunUpdate::Progress { .. } | RunUpdate::Usage { .. } => {}
            _ => println!("  <{}>", serde_json::to_string(&u)?),
        }
    }
    Ok(branch)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = std::env::args().nth(1).unwrap_or_else(|| "haiku".into());
    let root = std::env::temp_dir().join(format!("boba-e2e-{}", Uuid::now_v7()));
    let (origin, repo) = (root.join("origin.git"), root.join("repo"));
    std::fs::create_dir_all(&origin)?;
    std::fs::create_dir_all(&repo)?;
    sh(&origin, &["init", "-q", "--bare", "-b", "main"]).await?;
    sh(&repo, &["init", "-q", "-b", "main"]).await?;
    std::fs::write(repo.join("README.md"), "# Demo\n")?;
    std::fs::write(repo.join("add.py"), "def add(a, b):\n    return a - b\n")?;
    sh(&repo, &["add", "."]).await?;
    sh(
        &repo,
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@e.com",
            "commit",
            "-qm",
            "init",
        ],
    )
    .await?;
    sh(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    )
    .await?;
    sh(&repo, &["push", "-q", "-u", "origin", "main"]).await?;

    let mut cfg = ClaudeConfig::from_env(Uuid::now_v7(), root.join("data"));
    cfg.tools.gh = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-gh.sh");
    let r = ClaudeCodeRunner::new(cfg)?;
    println!(
        "claude {} · isolated config: {}",
        r.claude_version().await?,
        boba_runner::claude::api_key_auth_available()
    );

    let agent = AgentSpec {
        id: Uuid::now_v7(),
        name: "Pinky".into(),
        role: "Dev".into(),
        system_prompt: "Keep changes minimal.".into(),
        model: Some(model),
        permission_mode: PermissionMode::AcceptEdits,
        allowed_tools: vec![],
        max_turns: Some(12),
    };
    let ticket = TicketSpec {
        id: Uuid::now_v7(),
        number: 1,
        title: "Fix add()".into(),
        body: "`add` in add.py subtracts. Make it add. No tests needed.".into(),
        tags: vec![],
        branch: None,
        pr_number: None,
        pr_url: None,
        review_feedback: None,
        review_rounds: 0,
        source: Default::default(),
    };
    let repo_spec = RepoSpec {
        path: repo.to_string_lossy().into_owned(),
        default_branch: "main".into(),
        remote_url: None,
        ai_attribution: true,
    };
    let spec = RunSpec {
        run_id: Uuid::now_v7(),
        kind: RunKind::Work,
        agent: agent.clone(),
        ticket: Some(ticket.clone()),
        job: None,
        command: None,
        repo: Some(repo_spec.clone()),
        mcp_servers: vec![],
        board_tools: None,
        skill_paths: vec![],
        skill_names: vec![],
        resume_session_id: None,
        extra_instructions: None,
    };
    println!("===== WORK");
    let branch = go(&r, spec.clone()).await?;
    println!("===== REVIEW");
    let mut review = spec;
    review.run_id = Uuid::now_v7();
    review.kind = RunKind::Review;
    review.agent.name = "Uncle".into();
    review.agent.role = "Reviewer".into();
    review.ticket = Some(TicketSpec { branch, ..ticket });
    go(&r, review).await?;
    println!("(left {} for inspection)", root.display());
    Ok(())
}

//! Concurrency check for subscription mode: N real work runs at once sharing the machine
//! user's ~/.claude, then a `--resume` round on the first one.
//! `BOBA_CLAUDE_AUTH=subscription cargo run -p boba-runner --example claude_parallel -- [n] [model]`

use std::path::Path;
use std::time::Instant;

use boba_core::runner::{AgentSpec, RepoSpec, RunOutcome, RunSpec, RunUpdate, TicketSpec};
use boba_core::{PermissionMode, RunKind};
use boba_runner::{ClaudeCodeRunner, ClaudeConfig, Runner};
use uuid::Uuid;

async fn sh(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    boba_runner::git::run(Path::new("git"), args, cwd)
        .await
        .map(|_| ())
}

async fn go(
    r: ClaudeCodeRunner,
    spec: RunSpec,
) -> anyhow::Result<(Option<String>, Option<String>, RunOutcome, f64)> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let n = spec.ticket.as_ref().map(|t| t.number).unwrap_or(0);
    let run_id = spec.run_id;
    r.start(spec, tx).await?;
    let (mut session, mut branch, mut cost) = (None, None, 0.0);
    while let Some(u) = rx.recv().await {
        match u {
            RunUpdate::Started {
                session_id,
                branch: b,
                ..
            } => {
                session = session_id;
                branch = Some(b);
            }
            RunUpdate::Log { line } if matches!(line.kind, boba_core::LogKind::Error) => {
                println!("  T-{n} ERROR {}", line.text)
            }
            RunUpdate::Usage { cost_usd, .. } => cost = cost_usd,
            RunUpdate::PermissionRequest {
                request_id,
                tool,
                input,
            } => {
                println!("  T-{n} asked permission: {tool} {input} -> allowing");
                r.answer_permission(run_id, request_id, true).await?;
            }
            RunUpdate::Finished { outcome } => return Ok((session, branch, outcome, cost)),
            _ => {}
        }
    }
    anyhow::bail!("no Finished")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let n: i64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let model = std::env::args().nth(2).unwrap_or_else(|| "haiku".into());
    let root = std::env::temp_dir().join(format!("boba-par-{}", Uuid::now_v7()));
    let (origin, repo) = (root.join("origin.git"), root.join("repo"));
    std::fs::create_dir_all(&origin)?;
    std::fs::create_dir_all(&repo)?;
    sh(&origin, &["init", "-q", "--bare", "-b", "main"]).await?;
    sh(&repo, &["init", "-q", "-b", "main"]).await?;
    for i in 1..=n {
        std::fs::write(
            repo.join(format!("f{i}.py")),
            format!("def f{i}():\n    return {}\n", -i),
        )?;
    }
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
    cfg.max_concurrent_runs = n as u32;
    let r = ClaudeCodeRunner::connect(cfg).await?;
    println!("auth: {:?} (mode {:?})", r.auth(), r.auth_mode());

    let agent = AgentSpec {
        id: Uuid::now_v7(),
        name: "Pinky".into(),
        role: "Dev".into(),
        system_prompt: "Keep changes minimal.".into(),
        model: Some(model),
        permission_mode: PermissionMode::AcceptEdits,
        allowed_tools: vec![],
        max_turns: Some(10),
    };
    let spec = |i: i64| {
        RunSpec {
        run_id: Uuid::now_v7(),
        kind: RunKind::Work,
        agent: agent.clone(),
        ticket: Some(TicketSpec {
            id: Uuid::now_v7(),
            number: i,
            title: format!("Make f{i} positive"),
            body: format!("In f{i}.py, f{i}() must return {i} instead of {}. Edit the file and commit. No tests.", -i),
            tags: vec![],
            branch: None,
            pr_number: None,
            pr_url: None,
            review_feedback: None,
            review_rounds: 0,
            source: Default::default(),
        }),
        job: None,
        command: None,
        repo: Some(RepoSpec { path: repo.to_string_lossy().into_owned(), default_branch: "main".into(), remote_url: None, ai_attribution: true }),
        mcp_servers: vec![],
        board_tools: None,
        skill_paths: vec![],
        skill_names: vec![],
        resume_session_id: None,
        extra_instructions: None,
    }
    };
    let t0 = Instant::now();
    let handles: Vec<_> = (1..=n)
        .map(|i| tokio::spawn(go(r.clone(), spec(i))))
        .collect();
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await??);
    }
    println!("parallel round: {:.1}s", t0.elapsed().as_secs_f64());
    for (i, (s, b, o, c)) in results.iter().enumerate() {
        println!(
            "  T-{} {:?} session={} branch={} cost=${c:.4} err={:?}",
            i + 1,
            o.status,
            s.as_deref().unwrap_or("-"),
            b.as_deref().unwrap_or("-"),
            o.error
        );
    }
    let sessions: std::collections::HashSet<_> =
        results.iter().filter_map(|r| r.0.clone()).collect();
    println!("distinct sessions: {} of {n}", sessions.len());

    // Resume round on T-1 (changes requested), same shared config dir.
    let mut again = spec(1);
    let t = again.ticket.as_mut().unwrap();
    t.branch = results[0].1.clone();
    t.pr_number = Some(77);
    t.review_feedback = Some("Also add a one-line docstring to f1.".into());
    t.review_rounds = 1;
    again.resume_session_id = results[0].0.clone();
    let (s, _, o, c) = go(r.clone(), again).await?;
    println!(
        "resume round: {:?} session={} (asked {}) cost=${c:.4} err={:?}",
        o.status,
        s.unwrap_or_default(),
        results[0].0.clone().unwrap_or_default(),
        o.error
    );
    println!(
        "  summary: {}",
        o.summary
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
    );
    println!("auth after: {:?}", r.detect_auth().await?);
    println!("(left {})", root.display());
    Ok(())
}

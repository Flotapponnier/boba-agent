//! Process tests against `tests/fixtures/fake-claude.sh` and `fake-gh.sh` in a real git repo.

use std::path::{Path, PathBuf};
use std::time::Duration;

use boba_core::automation::{JobOutput, McpTransport};
use boba_core::runner::{
    AgentSpec, BoardToolsSpec, CommandSpec, JobSpec, McpServerSpec, RepoSpec, RunSpec, RunUpdate,
    TicketSpec,
};
use boba_core::{LogKind, PermissionMode, ReviewVerdict, RunKind, RunStatus};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::*;
use crate::git::tests::fixture_repo;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

struct Env {
    root: PathBuf,
    repo: PathBuf,
    out: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn env() -> Env {
    let root = std::env::temp_dir().join(format!("boba-cc-{}", Uuid::now_v7()));
    let repo = fixture_repo(&root).await;
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    Env { root, repo, out }
}

fn runner(e: &Env, mode: &str) -> ClaudeCodeRunner {
    let mut cfg = ClaudeConfig::new(Uuid::now_v7(), e.root.join("data"));
    cfg.claude_bin = fixtures().join("fake-claude.sh");
    cfg.tools.gh = fixtures().join("fake-gh.sh");
    cfg.isolate_config_dir = true;
    cfg.run_timeout = Duration::from_secs(30);
    cfg.env_passthrough = vec!["FAKE_MODE".into(), "FAKE_OUT".into()];
    // Each test sets its own mode through a wrapper script so tests can run in parallel.
    let wrapper = e.root.join(format!("claude-{mode}.sh"));
    std::fs::write(
        &wrapper,
        format!(
            "#!/usr/bin/env bash\nexport FAKE_MODE={mode} FAKE_OUT='{}'\nexec '{}' \"$@\"\n",
            e.out.display(),
            cfg.claude_bin.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    cfg.claude_bin = wrapper;
    ClaudeCodeRunner::new(cfg).unwrap()
}

fn spec(e: &Env, kind: RunKind) -> RunSpec {
    RunSpec {
        run_id: Uuid::now_v7(),
        kind,
        agent: AgentSpec {
            id: Uuid::now_v7(),
            name: "Pinky".into(),
            role: "Dev".into(),
            system_prompt: "Be brief.".into(),
            model: Some("claude-sonnet-5-5".into()),
            permission_mode: PermissionMode::AcceptEdits,
            allowed_tools: vec!["Bash(cargo test:*)".into()],
            max_turns: Some(30),
        },
        ticket: Some(TicketSpec {
            id: Uuid::now_v7(),
            number: 5,
            title: "Add feature".into(),
            body: "Please add the feature.".into(),
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
        repo: Some(RepoSpec {
            path: e.repo.to_string_lossy().into_owned(),
            default_branch: "main".into(),
            remote_url: None,
        }),
        mcp_servers: vec![],
        board_tools: None,
        skill_paths: vec![],
        resume_session_id: None,
        extra_instructions: None,
    }
}

async fn run(r: &ClaudeCodeRunner, s: RunSpec) -> Vec<RunUpdate> {
    let (tx, mut rx) = mpsc::channel(256);
    r.start(s, tx).await.unwrap();
    let mut out = Vec::new();
    while let Some(u) = tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("run stalled")
    {
        out.push(u);
    }
    out
}

fn outcome(updates: &[RunUpdate]) -> &RunOutcome {
    let fins: Vec<_> = updates
        .iter()
        .filter_map(|u| match u {
            RunUpdate::Finished { outcome } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(fins.len(), 1, "exactly one Finished: {updates:#?}");
    assert!(matches!(updates.last(), Some(RunUpdate::Finished { .. })));
    fins[0]
}

fn texts(updates: &[RunUpdate], kind: LogKind) -> Vec<String> {
    updates
        .iter()
        .filter_map(|u| match u {
            RunUpdate::Log { line } if line.kind == kind => Some(line.text.clone()),
            _ => None,
        })
        .collect()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[tokio::test]
async fn work_run_commits_pushes_and_opens_a_pr() {
    let e = env().await;
    let r = runner(&e, "work");
    let mut s = spec(&e, RunKind::Work);
    let skill = e.root.join("skills/demo");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: d\n---\n",
    )
    .unwrap();
    s.skill_paths = vec![
        skill.to_string_lossy().into_owned(),
        "/nonexistent/skill".into(),
    ];
    s.mcp_servers = vec![McpServerSpec {
        name: "search console".into(),
        transport: McpTransport::Http {
            url: "https://mcp.example.com".into(),
            header_secrets: [("Authorization".to_string(), "TOKEN".to_string())].into(),
        },
        secrets: [("TOKEN".to_string(), "s3cret".to_string())].into(),
        allowed_tools: vec!["query".into()],
    }];
    s.board_tools = Some(BoardToolsSpec {
        url: "http://127.0.0.1:7777/api/mcp".into(),
        token: "run-token".into(),
    });
    let run_dir = e.root.join("data/runs").join(s.run_id.to_string());
    let updates = run(&r, s).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Succeeded, "{o:?}");
    assert_eq!(o.summary.as_deref(), Some("Implemented the feature."));

    let RunUpdate::Started {
        worktree_path,
        branch,
        session_id,
    } = &updates[0]
    else {
        panic!("Started first")
    };
    assert!(worktree_path.ends_with("worktrees/T-5"));
    assert_eq!(branch, "boba/T-5-add-feature");
    assert!(updates
        .iter()
        .any(|u| matches!(u, RunUpdate::PrOpened { number: 77, .. })));
    assert!(updates.iter().any(|u| matches!(u, RunUpdate::Usage { cost_usd, input_tokens: 1000, output_tokens: 50 } if *cost_usd > 0.01)));
    assert!(texts(&updates, LogKind::System)
        .iter()
        .any(|t| t.contains("Skill not found")));
    assert!(texts(&updates, LogKind::Error)
        .iter()
        .any(|t| t.contains("\"broken\"")));
    assert!(updates.iter().any(
        |u| matches!(u, RunUpdate::Progress { activity, .. } if activity == "Editing feature.txt")
    ));

    // Pushed to origin; worktree and run dir (secrets) removed.
    let pushed = crate::git::run(
        Path::new("git"),
        &["rev-parse", "--verify", "refs/heads/boba/T-5-add-feature"],
        &e.root.join("origin.git"),
    )
    .await;
    assert!(pushed.is_ok());
    assert!(!Path::new(worktree_path).exists());
    assert!(!run_dir.exists());

    // CLI flags, MCP config (0600), env whitelist and cwd.
    let args = read(&e.out.join("args.txt"));
    for flag in [
        "--permission-prompt-tool",
        "stdio",
        "--replay-user-messages",
        "--strict-mcp-config",
        "--plugin-dir",
        "--mcp-config",
        "acceptEdits",
        "claude-sonnet-5-5",
    ] {
        assert!(args.lines().any(|l| l == flag), "missing {flag} in {args}");
    }
    assert!(args.lines().any(|l| l == session_id.as_deref().unwrap()));
    assert!(args.contains("Bash(cargo test:*),Bash(git status:*)"));
    assert!(args.contains("Bash(git commit:*),Bash(git rm:*),Bash(git mv:*),Bash(git restore:*),mcp__search_console__query,mcp__boba"));
    assert!(!args.contains("git push"));
    assert!(args.contains("SendMessage,ListAgents"));
    assert_eq!(read(&e.out.join("mcp.perms")).trim(), "-rw-------");
    let mcp: serde_json::Value = serde_json::from_str(&read(&e.out.join("mcp.json"))).unwrap();
    assert_eq!(
        mcp["mcpServers"]["search_console"]["headers"]["Authorization"],
        "Bearer s3cret"
    );
    assert_eq!(
        mcp["mcpServers"]["boba"]["headers"]["Authorization"],
        "Bearer run-token"
    );
    let env_dump = read(&e.out.join("env.txt"));
    assert!(env_dump.contains("CLAUDE_CODE_DISABLE_AUTO_MEMORY=1"));
    assert!(env_dump.contains("CLAUDE_CONFIG_DIR="));
    assert!(!env_dump.contains("CARGO_PKG_NAME"), "env is whitelisted");
}

#[tokio::test]
async fn no_change_fails_and_keeps_the_worktree() {
    let e = env().await;
    let r = runner(&e, "noop");
    let updates = run(&r, spec(&e, RunKind::Work)).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Failed);
    assert!(o.error.as_deref().unwrap().contains("without any change"));
    assert!(e.root.join("data/worktrees/T-5").exists());
}

#[tokio::test]
async fn permission_allow_and_deny() {
    for allow in [true, false] {
        let e = env().await;
        let r = runner(&e, "perm");
        let mut s = spec(&e, RunKind::Work);
        s.agent.permission_mode = PermissionMode::Default;
        let id = s.run_id;
        let (tx, mut rx) = mpsc::channel(256);
        r.start(s, tx).await.unwrap();
        let mut updates = Vec::new();
        while let Some(u) = rx.recv().await {
            if let RunUpdate::PermissionRequest {
                request_id, tool, ..
            } = &u
            {
                assert_eq!(tool, "Bash");
                r.answer_permission(id, request_id.clone(), allow)
                    .await
                    .unwrap();
            }
            updates.push(u);
        }
        assert_eq!(outcome(&updates).status, RunStatus::Succeeded);
        let answer = read(&e.out.join("answer.json"));
        assert!(
            answer.contains(if allow { "\"allow\"" } else { "\"deny\"" }),
            "{answer}"
        );
        assert!(answer.contains("req-1"));
    }
}

#[tokio::test]
async fn messages_reach_claude() {
    let e = env().await;
    let r = runner(&e, "message");
    let s = spec(&e, RunKind::Work);
    let id = s.run_id;
    let (tx, mut rx) = mpsc::channel(256);
    r.start(s, tx).await.unwrap();
    let mut updates = Vec::new();
    while let Some(u) = rx.recv().await {
        if matches!(&u, RunUpdate::Log { line } if line.text.starts_with("Claude Code started")) {
            r.send_message(id, "also update the docs".into())
                .await
                .unwrap();
        }
        updates.push(u);
    }
    assert_eq!(outcome(&updates).status, RunStatus::Succeeded);
    assert!(texts(&updates, LogKind::User).contains(&"also update the docs".to_string()));
    assert!(read(&e.out.join("message.json")).contains("also update the docs"));
}

#[tokio::test]
async fn review_verdicts() {
    let e = env().await;
    // Something to review on origin.
    let tools = crate::git::Tools::default();
    tools
        .git(&e.repo, &["checkout", "-q", "-b", "boba/T-5-add-feature"])
        .await
        .unwrap();
    std::fs::write(e.repo.join("f.txt"), "x").unwrap();
    tools.git(&e.repo, &["add", "."]).await.unwrap();
    tools.git(&e.repo, &["commit", "-qm", "f"]).await.unwrap();
    tools
        .git(&e.repo, &["push", "-q", "origin", "boba/T-5-add-feature"])
        .await
        .unwrap();
    tools
        .git(&e.repo, &["checkout", "-q", "main"])
        .await
        .unwrap();

    let r = runner(&e, "review_ok");
    let mut s = spec(&e, RunKind::Review);
    s.agent.allowed_tools = vec!["Write".into(), "Bash(rm:*)".into()];
    s.ticket.as_mut().unwrap().branch = Some("boba/T-5-add-feature".into());
    let updates = run(&r, s.clone()).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Succeeded, "{o:?}");
    assert_eq!(o.verdict, Some(ReviewVerdict::ChangesRequested));
    assert_eq!(o.feedback.as_deref(), Some("Add a test for x."));
    assert_eq!(o.summary.as_deref(), Some("Looks good."));
    let args = read(&e.out.join("args.txt"));
    assert!(args.lines().any(|l| l == "plan"));
    assert!(
        !args.contains("Bash(rm:*)") && args.contains("Bash(gh pr diff:*)"),
        "review tools are clamped"
    );
    assert!(args.contains("Edit,MultiEdit,Write"));
    assert!(!args.contains("--mcp-config"));

    let r = runner(&e, "review_bad");
    s.run_id = Uuid::now_v7();
    let updates = run(&r, s).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Failed);
    assert!(o.error.as_deref().unwrap().contains("boba-verdict"));
    assert!(
        e.root.join("data/worktrees/T-5-review").exists(),
        "kept for debugging"
    );
}

#[tokio::test]
async fn command_runs_have_no_tools_and_return_a_plan() {
    let e = env().await;
    let r = runner(&e, "command");
    let mut s = spec(&e, RunKind::Command);
    s.ticket = None;
    s.repo = None;
    s.command = Some(CommandSpec {
        text: "run the weekly audit".into(),
        voice: false,
        user_name: "Florent".into(),
        tab_id: None,
        ticket_id: None,
        agent_id: None,
        board_digest: serde_json::json!({ "jobs": [] }),
    });
    let updates = run(&r, s).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Succeeded, "{o:?}");
    let plan = o.plan.as_ref().unwrap();
    assert_eq!(
        plan["actions"].as_array().unwrap().len(),
        1,
        "invalid action dropped"
    );
    assert!(plan["reply"].as_str().unwrap().contains("skipped invalid"));
    let args = read(&e.out.join("args.txt"));
    let lines: Vec<&str> = args.lines().collect();
    let i = lines.iter().position(|l| *l == "--tools").expect("--tools");
    assert_eq!(lines[i + 1], "");
    assert!(!args.contains("--allowedTools"));
}

#[tokio::test]
async fn job_report_runs_in_a_scratch_dir() {
    let e = env().await;
    let r = runner(&e, "report");
    let mut s = spec(&e, RunKind::Job);
    s.ticket = None;
    s.repo = None;
    s.job = Some(JobSpec {
        id: Uuid::now_v7(),
        name: "Weekly SEO audit".into(),
        prompt: "Audit".into(),
        output: JobOutput::Report,
        previous_result: None,
    });
    let updates = run(&r, s).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Succeeded, "{o:?}");
    assert!(o.summary.as_deref().unwrap().starts_with("## Report"));
    assert!(read(&e.out.join("cwd.txt"))
        .trim()
        .ends_with("scratch/job-weekly-seo-audit"));
}

#[tokio::test]
async fn cancel_kills_the_whole_process_group() {
    let e = env().await;
    let r = runner(&e, "hang");
    let s = spec(&e, RunKind::Work);
    let id = s.run_id;
    let (tx, mut rx) = mpsc::channel(256);
    r.start(s, tx).await.unwrap();
    let pid_file = e.out.join("child.pid");
    for _ in 0..200 {
        if pid_file.exists() && !read(&pid_file).trim().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let pid: i32 = read(&pid_file)
        .trim()
        .parse()
        .expect("fake claude started its child");
    r.cancel(id).await.unwrap();
    let mut updates = Vec::new();
    while let Some(u) = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap()
    {
        updates.push(u);
    }
    assert_eq!(outcome(&updates).status, RunStatus::Cancelled);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // SAFETY: signal 0 only checks existence.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "grandchild {pid} survived the cancel");
    assert_eq!(r.active_runs(), 0);
}

#[tokio::test]
async fn crashes_and_error_results_fail_clearly() {
    let e = env().await;
    let updates = run(&runner(&e, "crash"), spec(&e, RunKind::Work)).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Failed);
    assert!(
        o.error
            .as_deref()
            .unwrap()
            .contains("boom: something broke"),
        "{o:?}"
    );

    let mut s = spec(&e, RunKind::Work);
    s.ticket.as_mut().unwrap().number = 6;
    let updates = run(&runner(&e, "error"), s).await;
    let o = outcome(&updates);
    assert_eq!(o.status, RunStatus::Failed);
    assert!(
        o.error.as_deref().unwrap().contains("Not logged in"),
        "{o:?}"
    );

    // Missing binary.
    let mut cfg = ClaudeConfig::new(Uuid::now_v7(), e.root.join("data2"));
    cfg.claude_bin = "/nonexistent/claude".into();
    let r = ClaudeCodeRunner::new(cfg).unwrap();
    let mut s = spec(&e, RunKind::Work);
    s.ticket.as_mut().unwrap().number = 7;
    let o = outcome(&run(&r, s).await).clone();
    assert!(o.error.unwrap().contains("Could not start Claude Code"));

    // Work run without a repo.
    let mut s = spec(&e, RunKind::Work);
    s.repo = None;
    let o = outcome(&run(&runner(&e, "work"), s).await).clone();
    assert!(o.error.unwrap().contains("without a repo"));
}

#[test]
fn mcp_names_and_args() {
    assert_eq!(mcp_name("search console!"), "search_console_");
    let e_root = std::env::temp_dir();
    let cfg = ClaudeConfig::new(Uuid::now_v7(), e_root);
    let s = RunSpec {
        kind: RunKind::Work,
        ..spec(
            &Env {
                root: PathBuf::from("/nonexistent-root"),
                repo: PathBuf::from("/r"),
                out: PathBuf::new(),
            },
            RunKind::Work,
        )
    };
    let mut s2 = s.clone();
    s2.agent.permission_mode = PermissionMode::BypassPermissions;
    let a = claude_args(&cfg, &s2, "sid", None, None);
    assert!(
        a.windows(2)
            .any(|w| w[0] == "--permission-mode" && w[1] == "acceptEdits"),
        "bypass clamped"
    );
    let mut s3 = s.clone();
    s3.resume_session_id = Some("old".into());
    let a = claude_args(&cfg, &s3, "sid", None, None);
    assert!(a.windows(2).any(|w| w[0] == "--resume" && w[1] == "old"));
    assert!(!a.contains(&"--session-id".to_string()));
}

#[test]
fn auth_status_mapping() {
    let st = |m: &str, p: &str, li: bool| {
        format!(r#"{{"loggedIn": {li}, "authMethod": "{m}", "apiProvider": "{p}"}}"#)
    };
    use AuthMode::*;
    assert_eq!(
        parse_auth_status(&st("claude.ai", "firstParty", true), Subscription),
        Ok(RunnerAuth::Subscription)
    );
    assert_eq!(
        parse_auth_status(&st("api_key", "firstParty", true), ApiKey),
        Ok(RunnerAuth::ApiKey)
    );
    assert_eq!(
        parse_auth_status(&st("api_key", "bedrock", true), ApiKey),
        Ok(RunnerAuth::Bedrock)
    );
    assert_eq!(
        parse_auth_status(&st("api_key", "vertex", true), ApiKey),
        Ok(RunnerAuth::Vertex)
    );
    assert!(
        parse_auth_status(&st("none", "firstParty", false), Subscription)
            .unwrap_err()
            .contains("claude auth login")
    );
    assert!(parse_auth_status(&st("none", "firstParty", false), ApiKey)
        .unwrap_err()
        .contains("ANTHROPIC_API_KEY"));
    assert!(parse_auth_status(&st("api_key", "firstParty", true), Subscription).is_err());
    assert!(
        parse_auth_status(&st("claude.ai", "firstParty", true), ApiKey)
            .unwrap_err()
            .contains("BOBA_CLAUDE_AUTH=subscription")
    );
    assert!(parse_auth_status("Usage: …", Subscription).is_err());
}

#[tokio::test]
async fn subscription_mode_uses_the_default_config_dir_and_no_api_key() {
    let e = env().await;
    // A key in the server env must not reach Claude in subscription mode.
    std::env::set_var("ANTHROPIC_AUTH_TOKEN", "should-not-leak");
    let r = {
        let base = runner(&e, "work");
        let mut cfg = base.inner.cfg.clone();
        cfg.auth_mode = AuthMode::Subscription;
        cfg.isolate_config_dir = true; // ignored in subscription mode
        cfg.data_dir = e.root.join("data-sub");
        ClaudeCodeRunner::connect(cfg).await.unwrap()
    };
    assert_eq!(r.auth(), RunnerAuth::Subscription);
    let auth_env = read(&e.out.join("auth_env.txt"));
    assert!(!auth_env.contains("should-not-leak"));
    let updates = run(&r, spec(&e, RunKind::Work)).await;
    assert_eq!(outcome(&updates).status, RunStatus::Succeeded);
    let run_env = read(&e.out.join("env.txt"));
    assert!(!run_env.contains("should-not-leak"), "API key vars dropped");
    assert!(
        !run_env.contains("data-sub/agents"),
        "no per-agent config dir"
    );
    std::env::remove_var("ANTHROPIC_AUTH_TOKEN");

    // Not logged in: connect fails loudly.
    let mut cfg = r.inner.cfg.clone();
    cfg.data_dir = e.root.join("data-sub2");
    let wrapper = e.root.join("claude-loggedout.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/usr/bin/env bash\nexport FAKE_OUT='{}' FAKE_AUTH='{{\"loggedIn\": false, \"authMethod\": \"none\"}}'\nexec '{}' \"$@\"\n",
            e.out.display(),
            fixtures().join("fake-claude.sh").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    cfg.claude_bin = wrapper;
    let err = ClaudeCodeRunner::connect(cfg)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("claude auth login"), "{err}");
}

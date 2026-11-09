use std::collections::HashSet;
use std::time::Duration;

use boba_core::runner::{AgentSpec, RepoSpec, RunSpec, RunUpdate, TicketSpec};
use boba_core::{LogKind, PermissionMode, ReviewVerdict, RunKind, RunStatus};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::*;

fn spec(kind: RunKind, mode: PermissionMode) -> RunSpec {
    RunSpec {
        run_id: Uuid::now_v7(),
        kind,
        agent: AgentSpec {
            id: Uuid::now_v7(),
            name: "Pinky".into(),
            role: "Dev".into(),
            system_prompt: "You are Pinky.".into(),
            model: Some("claude-sonnet-5-5".into()),
            permission_mode: mode,
            allowed_tools: vec![],
            max_turns: None,
        },
        ticket: Some(TicketSpec {
            id: Uuid::now_v7(),
            number: 42,
            title: "Add dark mode toggle to settings page".into(),
            body: "Users want a dark mode switch.".into(),
            tags: vec!["ui".into()],
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
            path: "/srv/repos/app".into(),
            default_branch: "main".into(),
            remote_url: Some("git@github.com:acme/app.git".into()),
            ai_attribution: true,
        }),
        mcp_servers: vec![],
        board_tools: None,
        skill_paths: vec![],
        skill_names: vec![],
        resume_session_id: None,
        extra_instructions: None,
    }
}

fn tk(s: &mut RunSpec) -> &mut TicketSpec {
    s.ticket.as_mut().unwrap()
}

fn runner(speed: f64) -> DemoRunner {
    let mut cfg = DemoConfig::new(Uuid::now_v7(), "/data");
    cfg.speed = speed;
    DemoRunner::new(cfg)
}

async fn collect(mut rx: mpsc::Receiver<RunUpdate>) -> Vec<RunUpdate> {
    let mut out = Vec::new();
    while let Some(u) = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("run stalled")
    {
        out.push(u);
    }
    out
}

async fn run_to_end(r: &DemoRunner, s: RunSpec) -> Vec<RunUpdate> {
    let (tx, rx) = mpsc::channel(64);
    r.start(s, tx).await.unwrap();
    collect(rx).await
}

fn finished(updates: &[RunUpdate]) -> &RunOutcome {
    let fins: Vec<_> = updates
        .iter()
        .filter_map(|u| match u {
            RunUpdate::Finished { outcome } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(fins.len(), 1, "exactly one Finished");
    assert!(
        matches!(updates.last(), Some(RunUpdate::Finished { .. })),
        "Finished is last"
    );
    fins[0]
}

/// Invariants every run must satisfy.
fn check_stream(updates: &[RunUpdate]) {
    finished(updates);
    let mut last_progress = 0.0f32;
    let mut last_cost = 0.0f64;
    let mut tool_ids = HashSet::new();
    for u in updates {
        match u {
            RunUpdate::Progress { progress, activity } => {
                assert!(*progress >= last_progress, "progress is monotonic");
                assert!(!activity.is_empty());
                last_progress = *progress;
            }
            RunUpdate::Usage { cost_usd, .. } => {
                assert!(*cost_usd >= last_cost, "usage is cumulative");
                last_cost = *cost_usd;
            }
            RunUpdate::Log { line } if line.kind == LogKind::ToolUse => {
                let d = line.data.as_ref().unwrap();
                assert!(d["name"].is_string() && d["input"].is_object());
                tool_ids.insert(d["id"].as_str().unwrap().to_string());
            }
            RunUpdate::Log { line } if line.kind == LogKind::ToolResult => {
                let d = line.data.as_ref().unwrap();
                assert!(d["output"].is_string() && d["is_error"].is_boolean());
                assert!(
                    tool_ids.contains(d["tool_use_id"].as_str().unwrap()),
                    "result pairs with a prior use"
                );
            }
            _ => {}
        }
    }
}

fn log_kinds(updates: &[RunUpdate]) -> HashSet<String> {
    updates
        .iter()
        .filter_map(|u| match u {
            RunUpdate::Log { line } => Some(format!("{:?}", line.kind)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn work_run_is_complete_and_believable() {
    let r = runner(0.0);
    let updates = run_to_end(&r, spec(RunKind::Work, PermissionMode::AcceptEdits)).await;
    check_stream(&updates);
    match &updates[0] {
        RunUpdate::Started {
            worktree_path,
            branch,
            session_id,
        } => {
            assert_eq!(worktree_path, "/data/worktrees/T-42");
            assert_eq!(branch, "boba/T-42-add-dark-mode-toggle-to-settings-page");
            assert!(session_id.is_some());
        }
        other => panic!("first update must be Started, got {other:?}"),
    }
    let kinds = log_kinds(&updates);
    for k in ["Text", "Thinking", "ToolUse", "ToolResult", "System"] {
        assert!(kinds.contains(k), "missing {k}");
    }
    let pr = updates.iter().find_map(|u| match u {
        RunUpdate::PrOpened { number, url } => Some((*number, url.clone())),
        _ => None,
    });
    assert_eq!(
        pr,
        Some((101, "https://github.com/acme/app/pull/101".into()))
    );
    let out = finished(&updates);
    assert_eq!(out.status, RunStatus::Succeeded);
    assert!(out.summary.is_some());

    // PR numbers increment; an existing PR is reused.
    let updates = run_to_end(&r, spec(RunKind::Work, PermissionMode::AcceptEdits)).await;
    assert!(updates
        .iter()
        .any(|u| matches!(u, RunUpdate::PrOpened { number: 102, .. })));
    let mut s = spec(RunKind::Work, PermissionMode::AcceptEdits);
    tk(&mut s).branch = Some("boba/T-42-x".into());
    tk(&mut s).pr_number = Some(7);
    tk(&mut s).pr_url = Some("https://github.com/acme/app/pull/7".into());
    tk(&mut s).review_feedback = Some("Add a test for the error path".into());
    tk(&mut s).review_rounds = 1;
    s.resume_session_id = Some("sess-1".into());
    let updates = run_to_end(&r, s).await;
    check_stream(&updates);
    assert!(
        matches!(&updates[0], RunUpdate::Started { branch, session_id: Some(sid), .. } if branch == "boba/T-42-x" && sid == "sess-1")
    );
    assert!(updates
        .iter()
        .any(|u| matches!(u, RunUpdate::PrOpened { number: 7, .. })));
    assert_eq!(r.active_runs(), 0);
}

#[tokio::test]
async fn review_verdicts_match_the_block_and_rates() {
    let r = runner(0.0);
    let mut approved = [0u32; 2];
    let n = 300;
    for (i, rounds) in [0u32, 2].into_iter().enumerate() {
        for _ in 0..n {
            let mut s = spec(RunKind::Review, PermissionMode::Plan);
            tk(&mut s).pr_number = Some(9);
            tk(&mut s).branch = Some("boba/T-42-x".into());
            tk(&mut s).review_rounds = rounds;
            let updates = run_to_end(&r, s).await;
            check_stream(&updates);
            let out = finished(&updates).clone();
            assert_eq!(out.status, RunStatus::Succeeded);
            let last_text = updates
                .iter()
                .rev()
                .find_map(|u| match u {
                    RunUpdate::Log { line } if line.kind == LogKind::Text => {
                        Some(line.text.clone())
                    }
                    _ => None,
                })
                .unwrap();
            let block = last_text
                .split("```boba-verdict\n")
                .nth(1)
                .unwrap()
                .split("\n```")
                .next()
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(block).unwrap();
            match out.verdict {
                Some(ReviewVerdict::Approved) => {
                    approved[i] += 1;
                    assert_eq!(v["verdict"], "approved");
                    assert!(out.feedback.is_none());
                }
                Some(ReviewVerdict::ChangesRequested) => {
                    assert_eq!(v["verdict"], "changes_requested");
                    assert_eq!(v["feedback"].as_str(), out.feedback.as_deref());
                }
                None => panic!("review without verdict"),
            }
        }
    }
    let rate0 = approved[0] as f64 / n as f64;
    let rate2 = approved[1] as f64 / n as f64;
    assert!(
        (0.65..0.85).contains(&rate0),
        "first-round approval {rate0}"
    );
    assert!(
        rate2 > rate0,
        "later rounds approve more ({rate2} vs {rate0})"
    );
}

#[tokio::test]
async fn cancel_ends_with_a_single_cancelled() {
    let r = runner(1.0);
    let s = spec(RunKind::Work, PermissionMode::AcceptEdits);
    let id = s.run_id;
    let (tx, rx) = mpsc::channel(64);
    r.start(s, tx).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    r.cancel(id).await.unwrap();
    let updates = collect(rx).await;
    assert_eq!(finished(&updates).status, RunStatus::Cancelled);
    assert_eq!(r.active_runs(), 0);
    assert!(
        r.cancel(id).await.is_err(),
        "cancel after the end is an error"
    );
}

#[tokio::test]
async fn messages_get_a_reply() {
    let r = runner(5.0);
    let s = spec(RunKind::Work, PermissionMode::AcceptEdits);
    let id = s.run_id;
    let (tx, mut rx) = mpsc::channel(64);
    r.start(s, tx).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.send_message(id, "please also update the README".into())
        .await
        .unwrap();
    let mut saw_user = false;
    let mut saw_reply = false;
    while let Some(u) = rx.recv().await {
        if let RunUpdate::Log { line } = &u {
            if line.kind == LogKind::User {
                assert_eq!(line.text, "please also update the README");
                saw_user = true;
            } else if saw_user
                && line.kind == LogKind::Text
                && line.text.contains("update the README")
            {
                saw_reply = true;
                r.cancel(id).await.unwrap();
            }
        }
        if let RunUpdate::Finished { outcome } = &u {
            assert_eq!(outcome.status, RunStatus::Cancelled);
        }
    }
    assert!(saw_user && saw_reply);
}

#[tokio::test]
async fn permission_prompt_allow_and_deny() {
    for allow in [true, false] {
        let r = runner(0.0);
        let s = spec(RunKind::Work, PermissionMode::Default);
        let id = s.run_id;
        let (tx, mut rx) = mpsc::channel(64);
        r.start(s, tx).await.unwrap();
        let mut updates = Vec::new();
        while let Some(u) = rx.recv().await {
            if let RunUpdate::PermissionRequest {
                request_id,
                tool,
                input,
            } = &u
            {
                assert_eq!(tool, "Bash");
                assert!(input["command"].is_string());
                r.answer_permission(id, request_id.clone(), allow)
                    .await
                    .unwrap();
            }
            updates.push(u);
        }
        check_stream(&updates);
        let ran_bash = updates.iter().any(|u| {
            matches!(u, RunUpdate::Log { line } if line.kind == LogKind::ToolUse && line.data.as_ref().unwrap()["name"] == "Bash")
        });
        assert_eq!(ran_bash, allow);
        assert_eq!(finished(&updates).status, RunStatus::Succeeded);
    }
}

#[tokio::test]
async fn concurrency_limit_queues_runs() {
    let mut cfg = DemoConfig::new(Uuid::now_v7(), "/data");
    cfg.speed = 50.0;
    cfg.max_concurrent_runs = 1;
    let r = DemoRunner::new(cfg);
    // One shared channel keeps the global send order.
    let (tx, mut rx) = mpsc::channel(256);
    for _ in 0..3 {
        r.start(spec(RunKind::Work, PermissionMode::AcceptEdits), tx.clone())
            .await
            .unwrap();
    }
    drop(tx);
    let mut order = String::new();
    while let Some(u) = rx.recv().await {
        match u {
            RunUpdate::Started { .. } => order.push('S'),
            RunUpdate::Finished { .. } => order.push('F'),
            _ => {}
        }
    }
    assert_eq!(order, "SFSFSF");
}

#[tokio::test]
async fn failure_injection_and_errors() {
    let mut cfg = DemoConfig::new(Uuid::now_v7(), "/data");
    cfg.speed = 0.0;
    cfg.fail_rate = 1.0;
    let r = DemoRunner::new(cfg);
    let updates = run_to_end(&r, spec(RunKind::Work, PermissionMode::AcceptEdits)).await;
    check_stream(&updates);
    let out = finished(&updates);
    assert_eq!(out.status, RunStatus::Failed);
    assert!(out.error.as_deref().unwrap().contains("Overloaded"));
    assert!(log_kinds(&updates).contains("Error"));

    let r = runner(1.0);
    let s = spec(RunKind::Work, PermissionMode::AcceptEdits);
    let (tx, _rx) = mpsc::channel(64);
    r.start(s.clone(), tx.clone()).await.unwrap();
    assert!(r.start(s.clone(), tx).await.is_err(), "duplicate run id");
    assert!(r.send_message(Uuid::now_v7(), "x".into()).await.is_err());
    r.cancel(s.run_id).await.unwrap();
}

#[tokio::test]
async fn dropped_receiver_stops_the_run() {
    let r = runner(1.0);
    let (tx, rx) = mpsc::channel(1);
    r.start(spec(RunKind::Work, PermissionMode::AcceptEdits), tx)
        .await
        .unwrap();
    drop(rx);
    for _ in 0..100 {
        if r.active_runs() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run kept going after the server dropped its receiver");
}

#[test]
fn helpers() {
    assert_eq!(
        github_slug("git@github.com:acme/app.git").as_deref(),
        Some("acme/app")
    );
    assert_eq!(
        github_slug("https://github.com/acme/app").as_deref(),
        Some("acme/app")
    );
    assert_eq!(github_slug("https://gitlab.com/acme/app"), None);
    assert_eq!(approval_probability(0), 0.75);
    assert!(approval_probability(10) <= 0.97);
}

#[tokio::test]
async fn job_runs_report_and_file_tickets() {
    use boba_core::automation::JobOutput;
    use boba_core::runner::{BoardToolsSpec, JobSpec};
    let r = runner(0.0);
    for output in [
        JobOutput::Report,
        JobOutput::Tickets,
        JobOutput::PullRequest,
    ] {
        let mut s = spec(RunKind::Job, PermissionMode::AcceptEdits);
        s.ticket = None;
        if output != JobOutput::PullRequest {
            s.repo = None;
        }
        s.job = Some(JobSpec {
            id: Uuid::now_v7(),
            name: "Weekly SEO audit".into(),
            prompt: "Find pages losing clicks and file tickets".into(),
            output,
            previous_result: Some("Last week: 2 pages flagged".into()),
        });
        s.board_tools = Some(BoardToolsSpec {
            url: "http://127.0.0.1:7777/api/mcp".into(),
            token: "t".into(),
        });
        let updates = run_to_end(&r, s).await;
        check_stream(&updates);
        let out = finished(&updates);
        assert_eq!(out.status, RunStatus::Succeeded, "{output:?}: {out:?}");
        let tools: Vec<String> = updates
            .iter()
            .filter_map(|u| match u {
                RunUpdate::Log { line } if line.kind == LogKind::ToolUse => Some(
                    line.data.as_ref().unwrap()["name"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                ),
                _ => None,
            })
            .collect();
        match output {
            JobOutput::Report => {
                assert!(tools.iter().any(|t| t.starts_with("mcp__search_console__")));
                assert!(out.summary.as_deref().unwrap().contains("Weekly SEO audit"));
            }
            JobOutput::Tickets => assert!(tools.iter().any(|t| t == "mcp__boba__create_ticket")),
            JobOutput::PullRequest => {
                assert!(updates
                    .iter()
                    .any(|u| matches!(u, RunUpdate::PrOpened { .. })));
            }
        }
    }
    // A PR job without a repo fails cleanly.
    let mut s = spec(RunKind::Job, PermissionMode::AcceptEdits);
    s.ticket = None;
    s.repo = None;
    s.job = Some(JobSpec {
        id: Uuid::now_v7(),
        name: "Bump deps".into(),
        prompt: "Bump".into(),
        output: JobOutput::PullRequest,
        previous_result: None,
    });
    let updates = run_to_end(&r, s).await;
    assert_eq!(finished(&updates).status, RunStatus::Failed);
}

#[tokio::test]
async fn command_runs_return_a_plan() {
    use boba_core::runner::CommandSpec;
    let r = runner(0.0);
    let mut s = spec(RunKind::Command, PermissionMode::Plan);
    s.ticket = None;
    s.repo = None;
    s.command = Some(CommandSpec {
        text: "create an agent named Mochi to review PRs".into(),
        voice: true,
        user_name: "Florent".into(),
        tab_id: None,
        ticket_id: None,
        agent_id: None,
        board_digest: serde_json::json!({ "agents": [], "tabs": [] }),
    });
    let updates = run_to_end(&r, s).await;
    check_stream(&updates);
    let out = finished(&updates);
    assert_eq!(out.status, RunStatus::Succeeded);
    let plan = out.plan.as_ref().unwrap();
    assert_eq!(plan["actions"][0]["action"]["action"], "create_agent");
    assert_eq!(plan["actions"][0]["action"]["agent"]["name"], "Mochi");
    let text = updates
        .iter()
        .rev()
        .find_map(|u| match u {
            RunUpdate::Log { line } if line.kind == LogKind::Text => Some(line.text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("```boba-plan"));
}

#[tokio::test]
async fn imported_pr_review_uses_head_branch() {
    use boba_core::automation::TicketSource;
    let r = runner(0.0);
    let mut s = spec(RunKind::Review, PermissionMode::Plan);
    tk(&mut s).pr_number = Some(31);
    tk(&mut s).source = TicketSource::GithubPr {
        repo_id: Uuid::now_v7(),
        number: 31,
        url: "https://github.com/acme/app/pull/31".into(),
        author: "outsider".into(),
        head_branch: "outsider/fix-typo".into(),
    };
    let updates = run_to_end(&r, s).await;
    assert!(
        matches!(&updates[0], RunUpdate::Started { branch, .. } if branch == "outsider/fix-typo")
    );
}

//! End-to-end tests: a real server on an ephemeral port, a scripted fake runner.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use boba_core::api::*;
use boba_core::event::ServerMessage;
use boba_core::runner::{RunOutcome, RunSpec, RunUpdate};
use boba_core::*;
use boba_runner::{Runner, UpdateTx};
use boba_server::router::{RunnerSlot, Runners};
use boba_server::{build, Config, SharedState};
use futures::StreamExt;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

// ---------------------------------------------------------------- fake runner

#[derive(Default)]
struct Script {
    verdicts: VecDeque<ReviewVerdict>,
    /// Never finish work runs (to test cancel / restart recovery).
    hang: bool,
    /// Accept runs but never send anything (they stay Queued).
    silent: bool,
    specs: Vec<RunSpec>,
    cancelled: Vec<Id>,
    /// What command runs answer (the parsed boba-plan block).
    plan: Option<Value>,
}

#[derive(Clone)]
struct FakeRunner {
    id: Id,
    script: Arc<Mutex<Script>>,
    live: Arc<Mutex<Vec<(Id, UpdateTx)>>>,
}

impl FakeRunner {
    fn new() -> Self {
        Self {
            id: Id::now_v7(),
            script: Arc::default(),
            live: Arc::default(),
        }
    }
    fn verdicts(&self, v: &[ReviewVerdict]) {
        self.script
            .lock()
            .unwrap()
            .verdicts
            .extend(v.iter().copied());
    }
    fn hang(&self, h: bool) {
        self.script.lock().unwrap().hang = h;
    }
    fn silent(&self, v: bool) {
        self.script.lock().unwrap().silent = v;
    }
    fn specs(&self) -> Vec<RunSpec> {
        self.script.lock().unwrap().specs.clone()
    }
}

fn log(run_id: Id, text: &str) -> RunUpdate {
    RunUpdate::Log {
        line: RunLogLine {
            run_id,
            seq: 999,
            ts: chrono::Utc::now(),
            kind: LogKind::Text,
            text: text.into(),
            data: None,
        },
    }
}

#[async_trait]
impl Runner for FakeRunner {
    fn id(&self) -> Id {
        self.id
    }
    fn name(&self) -> &str {
        "fake"
    }
    fn max_concurrent_runs(&self) -> u32 {
        4
    }
    async fn start(&self, spec: RunSpec, tx: UpdateTx) -> anyhow::Result<()> {
        let (verdict, hang, plan) = {
            let mut s = self.script.lock().unwrap();
            s.specs.push(spec.clone());
            if s.silent {
                drop(s);
                self.live.lock().unwrap().push((spec.run_id, tx));
                return Ok(());
            }
            let v = if spec.kind == RunKind::Review {
                s.verdicts.pop_front().or(Some(ReviewVerdict::Approved))
            } else {
                None
            };
            (v, s.hang, s.plan.clone())
        };
        self.live.lock().unwrap().push((spec.run_id, tx.clone()));
        tokio::spawn(async move {
            let id = spec.run_id;
            let session = spec
                .resume_session_id
                .clone()
                .unwrap_or_else(|| format!("sess-{id}"));
            let branch = match &spec.ticket {
                Some(t) => t
                    .branch
                    .clone()
                    .unwrap_or_else(|| format!("boba/T-{}", t.number)),
                None => String::new(), // job runs work outside a ticket branch
            };
            let _ = tx
                .send(RunUpdate::Started {
                    worktree_path: format!("/tmp/wt/{id}"),
                    branch,
                    session_id: Some(session),
                })
                .await;
            for i in 0..5 {
                let _ = tx.send(log(id, &format!("line {i}"))).await;
            }
            let _ = tx
                .send(RunUpdate::Progress {
                    progress: 0.5,
                    activity: "Thinking".into(),
                })
                .await;
            let _ = tx
                .send(RunUpdate::Usage {
                    cost_usd: 0.25,
                    input_tokens: 1000,
                    output_tokens: 200,
                })
                .await;
            if hang && matches!(spec.kind, RunKind::Work | RunKind::Job) {
                return; // tx kept alive in `live`
            }
            if spec.kind == RunKind::Work
                && spec.ticket.as_ref().is_some_and(|t| t.pr_number.is_none())
            {
                let _ = tx
                    .send(RunUpdate::PrOpened {
                        number: 7,
                        url: "https://example.test/pr/7".into(),
                    })
                    .await;
            }
            let feedback = match verdict {
                Some(ReviewVerdict::ChangesRequested) => Some("Please add tests".to_string()),
                Some(ReviewVerdict::Approved) => Some("Looks good".to_string()),
                None => None,
            };
            let _ = tx
                .send(RunUpdate::Finished {
                    outcome: RunOutcome {
                        status: RunStatus::Succeeded,
                        summary: Some("done".into()),
                        verdict,
                        feedback,
                        error: None,
                        plan: if spec.kind == RunKind::Command {
                            plan
                        } else {
                            None
                        },
                    },
                })
                .await;
        });
        Ok(())
    }
    async fn cancel(&self, run_id: Id) -> anyhow::Result<()> {
        self.script.lock().unwrap().cancelled.push(run_id);
        let tx = self
            .live
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == run_id)
            .map(|(_, tx)| tx.clone());
        if let Some(tx) = tx {
            let _ = tx
                .send(RunUpdate::Finished {
                    outcome: RunOutcome {
                        status: RunStatus::Cancelled,
                        summary: None,
                        verdict: None,
                        feedback: None,
                        error: Some("cancelled".into()),
                        plan: None,
                    },
                })
                .await;
        }
        Ok(())
    }
    async fn send_message(&self, _run_id: Id, _text: String) -> anyhow::Result<()> {
        Ok(())
    }
    async fn answer_permission(&self, _: Id, _: String, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
    async fn discover_skills(&self, _: &[String], _: &str) -> anyhow::Result<Vec<Skill>> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------- harness

struct Srv {
    base: String,
    runner: FakeRunner,
    state: SharedState,
    dir: PathBuf,
}

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("boba-test-{}", Id::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_in(dir: PathBuf, runner: FakeRunner) -> Srv {
    let r: Arc<dyn Runner> = Arc::new(runner.clone());
    start_with(dir, Runners::from(r), runner).await
}

async fn start_with(dir: PathBuf, runners: Runners, runner: FakeRunner) -> Srv {
    let cfg = Config::for_tests(dir.clone());
    start_cfg(cfg, runners, runner).await
}

async fn start_cfg(cfg: Config, runners: Runners, runner: FakeRunner) -> Srv {
    let dir = cfg.data_dir.clone();
    let (state, app) = build(cfg, runners).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Srv {
        base: format!("http://{addr}/api"),
        runner,
        state,
        dir,
    }
}

async fn start() -> Srv {
    start_in(tmp_dir(), FakeRunner::new()).await
}

fn client() -> Client {
    Client::builder().cookie_store(true).build().unwrap()
}

impl Srv {
    async fn req(
        &self,
        c: &Client,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let url = format!("{}{path}", self.base);
        let mut rb = c.request(method.parse().unwrap(), url);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let res = rb.send().await.unwrap();
        let status = res.status();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    async fn get(&self, c: &Client, path: &str) -> (StatusCode, Value) {
        self.req(c, "GET", path, None).await
    }
    async fn post(&self, c: &Client, path: &str, body: Value) -> (StatusCode, Value) {
        self.req(c, "POST", path, Some(body)).await
    }
    async fn snapshot(&self, c: &Client) -> Snapshot {
        let (s, v) = self.get(c, "/snapshot").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        serde_json::from_value(v).unwrap()
    }
    /// Owner client, workspace seeded.
    async fn owner(&self) -> Client {
        let c = client();
        let (s, v) = self
            .post(
                &c,
                "/auth/setup",
                json!({"workspace_name": "Test Tea", "name": "Owner", "email": "owner@test.io", "password": "correct horse"}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        c
    }
    async fn invite(&self, owner: &Client, email: &str, role: &str) -> Client {
        let (s, v) = self
            .post(
                owner,
                "/users/invite",
                json!({"email": email, "name": email, "role": role}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let url = v["invite_url"].as_str().unwrap();
        let token = url.rsplit('/').next().unwrap();
        let c = client();
        let (s, v) = self
            .post(
                &c,
                "/auth/accept-invite",
                json!({"token": token, "password": "password123"}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        c
    }
    /// Poll until `f(snapshot)` holds.
    async fn until(&self, c: &Client, what: &str, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        // Generous: CI and `cargo test` across the workspace run these in parallel.
        for _ in 0..600 {
            let s = self.snapshot(c).await;
            if f(&s) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let s = self.snapshot(c).await;
        let runs: Vec<_> = s
            .runs
            .iter()
            .map(|r| (r.kind, r.status, r.error.clone(), r.ticket_id))
            .collect();
        let agents: Vec<_> = s
            .agents
            .iter()
            .map(|a| (a.name.clone(), a.status))
            .collect();
        let tickets: Vec<_> = s
            .tickets
            .iter()
            .filter(|t| t.agent_id.is_some() || t.review_rounds > 0)
            .map(|t| {
                (
                    t.id,
                    t.number,
                    column_kind(&s, t),
                    t.review_feedback.clone(),
                )
            })
            .collect();
        panic!("timed out waiting for {what}: runs={runs:?} agents={agents:?} tickets={tickets:?}");
    }
}

fn ticket(s: &Snapshot, id: Id) -> &Ticket {
    s.tickets.iter().find(|t| t.id == id).unwrap()
}
fn agent<'a>(s: &'a Snapshot, name: &str) -> &'a Agent {
    s.agents.iter().find(|a| a.name == name).unwrap()
}
fn column_kind(s: &Snapshot, t: &Ticket) -> ColumnKind {
    s.tabs
        .iter()
        .flat_map(|tab| tab.columns.iter())
        .find(|c| c.id == t.column_id)
        .unwrap()
        .kind
}
fn all_idle(s: &Snapshot) -> bool {
    s.agents.iter().all(|a| a.status == AgentStatus::Idle)
        && s.runs.iter().all(|r| r.status.is_terminal())
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn auth_flow() {
    let srv = start().await;
    let c = client();
    let (_, v) = srv.get(&c, "/auth/status").await;
    assert_eq!(v["setup_done"], false);
    assert_eq!(srv.get(&c, "/snapshot").await.0, StatusCode::UNAUTHORIZED);

    let owner = srv.owner().await;
    let (_, v) = srv.get(&owner, "/auth/status").await;
    assert_eq!(v["setup_done"], true);
    assert_eq!(v["me"]["role"], "owner");

    // Setup only once.
    let (s, v) = srv
        .post(&c, "/auth/setup", json!({"workspace_name": "x", "name": "x", "email": "x@y.io", "password": "password123"}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["code"], "conflict");

    // Wrong password, then right one.
    let (s, _) = srv
        .post(
            &c,
            "/auth/login",
            json!({"email": "owner@test.io", "password": "nope nope"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = srv
        .post(
            &c,
            "/auth/login",
            json!({"email": "OWNER@test.io", "password": "correct horse"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(srv.get(&c, "/snapshot").await.0, StatusCode::OK);

    // Logout kills the session.
    assert_eq!(
        srv.post(&c, "/auth/logout", json!({})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(srv.get(&c, "/snapshot").await.0, StatusCode::UNAUTHORIZED);

    // The seeded demo workspace.
    let snap = srv.snapshot(&owner).await;
    assert_eq!(snap.tabs.len(), 3);
    assert_eq!(snap.agents.len(), 7); // 6 bobas + the built-in Concierge
    assert_eq!(snap.tickets.len(), 8);
    assert!(snap.skills.iter().any(|s| s.name == "review"));
    assert_eq!(snap.settings.name, "Test Tea");
    let review = snap.tabs[0]
        .columns
        .iter()
        .find(|c| c.kind == ColumnKind::Review)
        .unwrap();
    assert_eq!(
        review.auto_reviewer_agent_id,
        Some(agent(&snap, "Uncle Review").id)
    );
}

#[tokio::test]
async fn rbac() {
    let srv = start().await;
    let owner = srv.owner().await;
    let snap = srv.snapshot(&owner).await;
    let tab = snap.tabs[0].id;
    let t = snap.tickets[0].id;
    let a = snap.agents[0].id;

    let viewer = srv.invite(&owner, "viewer@test.io", "viewer").await;
    let member = srv.invite(&owner, "member@test.io", "member").await;

    // Viewer: read only.
    assert_eq!(srv.get(&viewer, "/snapshot").await.0, StatusCode::OK);
    let (s, v) = srv
        .post(&viewer, "/tickets", json!({"tab_id": tab, "title": "nope"}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["code"], "forbidden");
    assert_eq!(
        srv.post(
            &viewer,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": a})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // Member: tickets yes, agents / workspace / users no.
    let (s, v) = srv
        .post(
            &member,
            "/tickets",
            json!({"tab_id": tab, "title": "Member ticket"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let agent_body =
        json!({"name": "X", "role": "Dev", "sprite": "boba", "system_prompt": "", "skill_ids": []});
    assert_eq!(
        srv.post(&member, "/agents", agent_body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.post(&member, "/tabs", json!({"name": "T"})).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.post(
            &member,
            "/users/invite",
            json!({"email": "a@b.io", "name": "a", "role": "viewer"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // Owner can; admins cannot promote admins; nobody edits the owner.
    assert_eq!(
        srv.post(&owner, "/agents", agent_body).await.0,
        StatusCode::OK
    );
    let admin = srv.invite(&owner, "admin@test.io", "admin").await;
    let snap = srv.snapshot(&owner).await;
    let member_id = snap
        .users
        .iter()
        .find(|u| u.email == "member@test.io")
        .unwrap()
        .id;
    let owner_id = snap
        .users
        .iter()
        .find(|u| u.role == Role::Owner)
        .unwrap()
        .id;
    let (s, _) = srv
        .req(
            &admin,
            "PATCH",
            &format!("/users/{member_id}"),
            Some(json!({"role": "admin"})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = srv
        .req(
            &admin,
            "PATCH",
            &format!("/users/{member_id}"),
            Some(json!({"role": "viewer"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = srv
        .req(
            &admin,
            "PATCH",
            &format!("/users/{owner_id}"),
            Some(json!({"role": "viewer"})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The demoted member lost write access immediately.
    assert_eq!(
        srv.post(
            &member,
            "/tickets",
            json!({"tab_id": tab, "title": "again"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn work_then_auto_review_approved() {
    let srv = start().await;
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    // "Write tests for the date formatting helpers", tagged "testing": Boba Dev has that skill.
    let t = snap
        .tickets
        .iter()
        .find(|t| t.tags == vec!["testing".to_string()])
        .unwrap()
        .id;
    let dev = agent(&snap, "Boba Dev").id;

    let (s, v) = srv
        .post(
            &c,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": dev, "instructions": "Be quick"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["kind"], "work");

    let snap = srv
        .until(&c, "approval", |s| {
            column_kind(s, ticket(s, t)) == ColumnKind::Done && all_idle(s)
        })
        .await;
    let tk = ticket(&snap, t);
    assert_eq!(tk.pr_number, Some(7));
    assert_eq!(
        tk.branch.as_deref(),
        Some(&*format!("boba/T-{}", tk.number))
    );
    assert_eq!(tk.agent_id, Some(dev));
    let d = agent(&snap, "Boba Dev");
    // 60 (skill match) + 50 first-round bonus = 110 -> level 2 with 10 XP.
    assert_eq!(
        (d.level, d.xp, d.prs_approved, d.runs_completed),
        (2, 10, 1, 1)
    );
    let r = agent(&snap, "Uncle Review");
    assert_eq!((r.xp, r.runs_completed), (30, 1));
    // pearls = 10 + 5 (first round) + 2 * author level (2)
    assert_eq!(snap.stats.pearls, 19);
    assert_eq!(snap.stats.prs_approved, 1);
    assert_eq!(snap.stats.runs_completed, 2);
    assert!((snap.stats.total_cost_usd - 0.5).abs() < 1e-9);

    // The review spec is read-only and carries the PR.
    let specs = srv.runner.specs();
    let review = specs.iter().find(|s| s.kind == RunKind::Review).unwrap();
    assert_eq!(review.agent.permission_mode, PermissionMode::Plan);
    assert_eq!(review.ticket.as_ref().unwrap().pr_number, Some(7));
    assert_eq!(specs[0].extra_instructions.as_deref(), Some("Be quick"));
    assert!(!specs[0].skill_paths.is_empty());

    // Runs, logs (server-assigned seq, including the injected instruction line), history.
    let (_, runs) = srv.get(&c, &format!("/tickets/{t}/runs")).await;
    let runs: Vec<Run> = serde_json::from_value(runs).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].kind, RunKind::Review);
    assert_eq!(runs[0].verdict, Some(ReviewVerdict::Approved));
    let work = &runs[1];
    assert_eq!((work.input_tokens, work.progress), (1000, 1.0));
    let (_, logs) = srv.get(&c, &format!("/runs/{}/logs", work.id)).await;
    let logs: Vec<RunLogLine> = serde_json::from_value(logs).unwrap();
    let seqs: Vec<u64> = logs.iter().map(|l| l.seq).collect();
    assert_eq!(seqs, (0..logs.len() as u64).collect::<Vec<_>>());
    assert_eq!(logs[0].kind, LogKind::User);
    let (_, logs) = srv
        .get(&c, &format!("/runs/{}/logs?after_seq=3&limit=2", work.id))
        .await;
    assert_eq!(logs.as_array().unwrap().len(), 2);
    assert_eq!(logs[0]["seq"], 4);

    let (_, h) = srv.get(&c, &format!("/history?ticket_id={t}")).await;
    let kinds: Vec<&str> = h
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    for k in [
        "run_started",
        "pr_opened",
        "run_succeeded",
        "review_approved",
        "agent_level_up",
    ] {
        assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
    }
}

#[tokio::test]
async fn changes_requested_then_fixed() {
    let srv = start().await;
    srv.runner
        .verdicts(&[ReviewVerdict::ChangesRequested, ReviewVerdict::Approved]);
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    let t = snap.tickets[0].id;
    let dev = agent(&snap, "Boba Dev").id;

    srv.post(
        &c,
        &format!("/tickets/{t}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    let snap = srv
        .until(&c, "changes requested", |s| {
            ticket(s, t).review_rounds == 1 && all_idle(s)
        })
        .await;
    let tk = ticket(&snap, t);
    assert_eq!(column_kind(&snap, tk), ColumnKind::Todo);
    assert_eq!(tk.review_feedback.as_deref(), Some("Please add tests"));

    // Second round resumes the first session on the same branch/PR.
    let (s, v) = srv
        .post(
            &c,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": dev}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let snap = srv
        .until(&c, "approval", |s| {
            column_kind(s, ticket(s, t)) == ColumnKind::Done && all_idle(s)
        })
        .await;
    let specs = srv.runner.specs();
    let second = &specs[2];
    assert_eq!(second.kind, RunKind::Work);
    assert_eq!(
        second.resume_session_id.as_deref(),
        Some(&*format!("sess-{}", specs[0].run_id))
    );
    assert_eq!(
        second.ticket.as_ref().unwrap().review_feedback.as_deref(),
        Some("Please add tests")
    );
    assert_eq!(second.ticket.as_ref().unwrap().review_rounds, 1);
    assert_eq!(second.ticket.as_ref().unwrap().pr_number, Some(7));
    let tk = ticket(&snap, t);
    assert_eq!(tk.review_feedback, None);
    // Ticket tagged "frontend" (a Boba Dev skill): 60 + 60 XP, no first-round bonus
    // -> level 2 with 20 XP; pearls = 10 + 2 * 2.
    let d = agent(&snap, "Boba Dev");
    assert_eq!((d.level, d.xp, d.prs_approved), (2, 20, 1));
    assert_eq!(snap.stats.pearls, 14);
}

#[tokio::test]
async fn reviewer_queue_is_fifo() {
    let srv = start().await;
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    let tab = &snap.tabs[0];
    let review_col = tab
        .columns
        .iter()
        .find(|c| c.kind == ColumnKind::Review)
        .unwrap()
        .id;
    let ids: Vec<Id> = snap
        .tickets
        .iter()
        .filter(|t| t.tab_id == tab.id)
        .map(|t| t.id)
        .take(3)
        .collect();
    // Hold the reviewer: hang mode doesn't block reviews, so make it busy with a manual run.
    srv.runner.hang(true);
    let reviewer = agent(&snap, "Uncle Review").id;
    let other = snap.tickets.iter().find(|t| t.tab_id != tab.id).unwrap().id;
    let (s, v) = srv
        .post(
            &c,
            &format!("/tickets/{other}/assign"),
            json!({"agent_id": reviewer}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for (i, id) in ids.iter().enumerate() {
        let (s, v) = srv
            .post(
                &c,
                &format!("/tickets/{id}/move"),
                json!({"column_id": review_col, "position": i as f64}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["column_id"], json!(review_col));
    }
    // Nothing started for them: the reviewer is busy.
    let snap = srv.snapshot(&c).await;
    assert!(snap
        .runs
        .iter()
        .all(|r| r.ticket_id.is_none_or(|t| !ids.contains(&t))));
    // Free the reviewer by cancelling its run; the queue drains in order.
    let run_id = agent(&snap, "Uncle Review").current_run_id.unwrap();
    let (s, v) = srv
        .post(&c, &format!("/runs/{run_id}/cancel"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    srv.until(&c, "all reviewed", |s| {
        ids.iter()
            .all(|id| column_kind(s, ticket(s, *id)) == ColumnKind::Done)
            && all_idle(s)
    })
    .await;
    let order: Vec<Id> = srv
        .runner
        .specs()
        .iter()
        .filter(|s| s.kind == RunKind::Review)
        .map(|s| s.ticket.as_ref().unwrap().id)
        .collect();
    assert_eq!(&order[order.len() - 3..], &ids[..]);
}

#[tokio::test]
async fn cancel_and_conflicts() {
    let srv = start().await;
    srv.runner.hang(true);
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    let (t1, t2) = (snap.tickets[0].id, snap.tickets[1].id);
    let dev = agent(&snap, "Boba Dev").id;
    let qa = agent(&snap, "Golden QA").id;

    let (_, run) = srv
        .post(
            &c,
            &format!("/tickets/{t1}/assign"),
            json!({"agent_id": dev}),
        )
        .await;
    let run_id = run["id"].as_str().unwrap().to_string();
    let snap = srv
        .until(&c, "doing", |s| {
            column_kind(s, ticket(s, t1)) == ColumnKind::Doing
        })
        .await;
    assert_eq!(agent(&snap, "Boba Dev").status, AgentStatus::Working);
    // Busy agent, busy ticket.
    assert_eq!(
        srv.post(
            &c,
            &format!("/tickets/{t2}/assign"),
            json!({"agent_id": dev})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        srv.post(
            &c,
            &format!("/tickets/{t1}/assign"),
            json!({"agent_id": qa})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        srv.req(&c, "DELETE", &format!("/agents/{dev}"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );

    let (s, _) = srv
        .post(&c, &format!("/runs/{run_id}/cancel"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK);
    let snap = srv.until(&c, "cancelled", all_idle).await;
    assert_eq!(column_kind(&snap, ticket(&snap, t1)), ColumnKind::Todo);
    assert_eq!(
        srv.post(&c, &format!("/runs/{run_id}/cancel"), json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );

    // Deleting a ticket with a live run cancels it.
    srv.post(
        &c,
        &format!("/tickets/{t2}/assign"),
        json!({"agent_id": qa}),
    )
    .await;
    srv.until(&c, "qa working", |s| {
        agent(s, "Golden QA").status == AgentStatus::Working
    })
    .await;
    assert_eq!(
        srv.req(&c, "DELETE", &format!("/tickets/{t2}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let snap = srv.until(&c, "qa idle", all_idle).await;
    assert!(!snap.tickets.iter().any(|t| t.id == t2));
}

#[tokio::test]
async fn restart_recovery() {
    let dir = tmp_dir();
    let runner = FakeRunner::new();
    runner.hang(true);
    let srv = start_in(dir.clone(), runner).await;
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    let t = snap.tickets[0].id;
    let dev = agent(&snap, "Boba Dev").id;
    srv.post(
        &c,
        &format!("/tickets/{t}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    srv.until(&c, "running", |s| {
        s.runs.iter().any(|r| r.status == RunStatus::Running)
    })
    .await;
    // A second run the runner accepted but never started: still Queued at "crash" time.
    srv.runner.silent(true);
    let t2 = snap.tickets[1].id;
    let qa = agent(&snap, "Golden QA").id;
    let (s, v) = srv
        .post(
            &c,
            &format!("/tickets/{t2}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "queued");

    // A second server on the same data dir = a restart.
    let srv2 = start_in(srv.dir.clone(), FakeRunner::new()).await;
    let c2 = client();
    srv2.post(
        &c2,
        "/auth/login",
        json!({"email": "owner@test.io", "password": "correct horse"}),
    )
    .await;
    let snap = srv2.snapshot(&c2).await;
    let run = snap.runs.iter().find(|r| r.ticket_id == Some(t)).unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.as_deref(), Some("server restarted"));
    assert!(all_idle(&snap));
    let tk = ticket(&snap, t);
    assert_eq!(column_kind(&snap, tk), ColumnKind::Todo);
    assert_eq!(
        tk.review_feedback.as_deref(),
        Some("Run failed: server restarted")
    );
    // The queued run was failed too, and recovery handed nothing to the new runner
    // (no run is started and then marked failed behind the runner's back).
    let queued = snap.runs.iter().find(|r| r.ticket_id == Some(t2)).unwrap();
    assert_eq!(queued.status, RunStatus::Failed);
    assert!(srv2.runner.specs().is_empty(), "recovery dispatched runs");
}

#[tokio::test]
async fn approval_routes_into_a_second_review_column() {
    let srv = start().await;
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    let uncle = agent(&snap, "Uncle Review").id;
    let qa = agent(&snap, "Golden QA").id;
    let dev = agent(&snap, "Boba Dev").id;
    // Todo, Doing, Code review (Uncle) -> QA review (Golden QA) -> Done.
    let (s, tab) = srv
        .post(
            &c,
            "/tabs",
            json!({"name": "Two step", "columns": [
                {"name": "Todo", "kind": "todo"},
                {"name": "Doing", "kind": "doing"},
                {"name": "Code review", "kind": "review", "auto_reviewer_agent_id": uncle},
                {"name": "QA review", "kind": "review", "auto_reviewer_agent_id": qa},
                {"name": "Done", "kind": "done"}
            ]}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{tab}");
    let tab: Tab = serde_json::from_value(tab).unwrap();
    let mut cols: Vec<ColumnSpec> = tab
        .columns
        .iter()
        .map(|c| ColumnSpec {
            id: Some(c.id),
            name: c.name.clone(),
            kind: c.kind,
            color: None,
            auto_reviewer_agent_id: c.auto_reviewer_agent_id,
            on_approve_column_id: None,
            on_changes_column_id: None,
        })
        .collect();
    cols[2].on_approve_column_id = Some(tab.columns[3].id);
    let (s, v) = srv
        .req(
            &c,
            "PATCH",
            &format!("/tabs/{}", tab.id),
            Some(json!({"columns": cols})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, tk) = srv
        .post(
            &c,
            "/tickets",
            json!({"tab_id": tab.id, "title": "Two reviews"}),
        )
        .await;
    let t: Id = serde_json::from_value(tk["id"].clone()).unwrap();
    srv.post(
        &c,
        &format!("/tickets/{t}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    srv.until(&c, "both reviews", |s| {
        column_kind(s, ticket(s, t)) == ColumnKind::Done && all_idle(s)
    })
    .await;
    let reviewers: Vec<Id> = srv
        .runner
        .specs()
        .iter()
        .filter(|s| s.kind == RunKind::Review)
        .map(|s| s.agent.id)
        .collect();
    assert_eq!(reviewers, vec![uncle, qa]);
}

#[tokio::test]
async fn invites_are_single_use_and_logins_do_not_leak_them() {
    let srv = start().await;
    let owner = srv.owner().await;
    let (_, v) = srv
        .post(
            &owner,
            "/users/invite",
            json!({"email": "new@test.io", "name": "New", "role": "member"}),
        )
        .await;
    let token = v["invite_url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    let c = client();
    // Pending invite: same answer as a wrong password.
    let (s1, pending) = srv
        .post(
            &c,
            "/auth/login",
            json!({"email": "new@test.io", "password": "whatever1"}),
        )
        .await;
    let (s2, wrong) = srv
        .post(
            &c,
            "/auth/login",
            json!({"email": "owner@test.io", "password": "whatever1"}),
        )
        .await;
    assert_eq!((s1, &pending), (s2, &wrong));
    let accept = json!({"token": token, "password": "password123"});
    assert_eq!(
        srv.post(&c, "/auth/accept-invite", accept.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        srv.post(&client(), "/auth/accept-invite", accept).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ticket_numbers_are_never_reused() {
    let srv = start().await;
    let c = srv.owner().await;
    let snap = srv.snapshot(&c).await;
    let last = snap.tickets.iter().max_by_key(|t| t.number).unwrap();
    assert_eq!(
        srv.req(&c, "DELETE", &format!("/tickets/{}", last.id), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, v) = srv
        .post(
            &c,
            "/tickets",
            json!({"tab_id": last.tab_id, "title": "New one"}),
        )
        .await;
    assert_eq!(v["number"], json!(last.number + 1));
}

#[tokio::test]
async fn websocket_closes_on_logout() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    let srv = start().await;
    let _owner = srv.owner().await;
    let res = reqwest::Client::new()
        .post(format!("{}/auth/login", srv.base))
        .json(&json!({"email": "owner@test.io", "password": "correct horse"}))
        .send()
        .await
        .unwrap();
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let ws_url = srv.base.replace("http://", "ws://") + "/ws";
    let mut req = ws_url.as_str().into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    // Snapshot first.
    let first = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();
    assert!(matches!(first, Some(Ok(Message::Text(_)))));

    let res = reqwest::Client::new()
        .post(format!("{}/auth/logout", srv.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert_eq!(closed, Ok(true), "socket stayed open after logout");
}

#[tokio::test]
async fn tabs_tickets_skills_crud() {
    let srv = start().await;
    let c = srv.owner().await;
    // Default columns + auto reviewer.
    let (s, tab) = srv
        .post(&c, "/tabs", json!({"name": "Docs", "icon": "📚"}))
        .await;
    assert_eq!(s, StatusCode::OK, "{tab}");
    let tab: Tab = serde_json::from_value(tab).unwrap();
    assert_eq!(tab.columns.len(), 4);
    assert!(tab.columns[2].auto_reviewer_agent_id.is_some());

    // Replace columns: keep Todo, add Backlog; tickets in removed columns move.
    let (_, tk) = srv
        .post(
            &c,
            "/tickets",
            json!({"tab_id": tab.id, "title": "  Write docs  ", "tags": ["seo", "SEO", " "]}),
        )
        .await;
    assert_eq!(tk["title"], "Write docs");
    assert_eq!(tk["tags"], json!(["seo"]));
    let (s, v) = srv
        .req(
            &c,
            "PATCH",
            &format!("/tabs/{}", tab.id),
            Some(json!({"name": "Docs!", "columns": [
                {"name": "Backlog", "kind": "backlog"},
                {"id": tab.columns[0].id, "name": "Todo", "kind": "todo"}
            ]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["columns"].as_array().unwrap().len(), 2);
    assert_eq!(v["name"], "Docs!");

    // Edit, comment, history.
    let tid = tk["id"].as_str().unwrap();
    let (s, v) = srv
        .req(
            &c,
            "PATCH",
            &format!("/tickets/{tid}"),
            Some(json!({"priority": "urgent"})),
        )
        .await;
    assert_eq!(
        (s, v["priority"].clone()),
        (StatusCode::OK, json!("urgent"))
    );
    let (s, v) = srv
        .post(
            &c,
            "/comments",
            json!({"ticket_id": tid, "anchor": {"on": "ticket"}, "body": "Nice"}),
        )
        .await;
    assert_eq!((s, v["body"].clone()), (StatusCode::OK, json!("Nice")));
    let (_, h) = srv
        .get(&c, &format!("/history?tab_id={}&limit=2", tab.id))
        .await;
    assert_eq!(h.as_array().unwrap().len(), 2);
    assert_eq!(h[0]["kind"], "comment");
    let before = h[1]["ts"].as_str().unwrap();
    let (_, older) = srv
        .get(&c, &format!("/history?tab_id={}&before={before}", tab.id))
        .await;
    assert!(older
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["ts"].as_str().unwrap() < before));

    // Skills: create, read, edit, conflict.
    let body =
        json!({"name": "Docs Writer", "description": "Writes docs", "body": "# Write\nClearly."});
    let (s, sk) = srv.post(&c, "/skills", body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{sk}");
    assert_eq!(srv.post(&c, "/skills", body).await.0, StatusCode::CONFLICT);
    let sid = sk["id"].as_str().unwrap();
    let (s, v) = srv
        .req(
            &c,
            "PUT",
            &format!("/skills/{sid}"),
            Some(json!({"name": "Docs Writer", "description": "Better", "body": "New body"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, d) = srv.get(&c, &format!("/skills/{sid}")).await;
    assert_eq!(d["body"], "New body");
    assert_eq!(d["skill"]["description"], "Better");
    let (s, list) = srv.post(&c, "/skills/rescan", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(list.as_array().unwrap().iter().any(|s| s["id"] == sid));

    // Delete tab.
    assert_eq!(
        srv.req(&c, "DELETE", &format!("/tabs/{}", tab.id), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let snap = srv.snapshot(&c).await;
    assert!(!snap.tickets.iter().any(|t| t.tab_id == tab.id));

    // Unknown routes are JSON 404s, other paths serve the SPA.
    assert_eq!(srv.get(&c, "/nope").await.0, StatusCode::NOT_FOUND);
    let res = c
        .get(srv.base.replace("/api", "/invite/abc"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn websocket_snapshot_and_live_logs() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    let srv = start().await;
    let c = srv.owner().await;
    let ws_url = srv.base.replace("http://", "ws://") + "/ws";

    // No cookie = refused.
    assert!(tokio_tungstenite::connect_async(ws_url.as_str())
        .await
        .is_err());

    // Log in with a fresh client to read the session cookie value.
    let res = reqwest::Client::new()
        .post(format!("{}/auth/login", srv.base))
        .json(&json!({"email": "owner@test.io", "password": "correct horse"}))
        .send()
        .await
        .unwrap();
    let cookie = res
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let mut req = ws_url.as_str().into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    async fn next(
        ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    ) -> ServerMessage {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("ws timeout")
                .unwrap()
                .unwrap();
            if let Message::Text(t) = m {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }
    let first = next(&mut ws).await;
    let ServerMessage::Snapshot { state } = first else {
        panic!("expected snapshot first")
    };
    assert_eq!(state.me.unwrap().email, "owner@test.io");

    // Hang a run, subscribe, and get the replay of its lines.
    srv.runner.hang(true);
    let t = state.tickets[0].id;
    let dev = state.agents[0].id;
    let (_, run) = srv
        .post(
            &c,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": dev}),
        )
        .await;
    let run_id: Id = serde_json::from_value(run["id"].clone()).unwrap();
    srv.until(&c, "logs", |s| {
        s.runs
            .iter()
            .any(|r| r.id == run_id && r.input_tokens == 1000)
    })
    .await;
    use futures::SinkExt;
    ws.send(Message::Text(
        serde_json::to_string(&json!({"type": "subscribe", "run_ids": [run_id]}))
            .unwrap()
            .into(),
    ))
    .await
    .unwrap();
    let mut seqs = Vec::new();
    let mut saw_upsert = false;
    while seqs.len() < 5 {
        match next(&mut ws).await {
            ServerMessage::RunLog { line } => {
                assert_eq!(line.run_id, run_id);
                seqs.push(line.seq);
            }
            ServerMessage::Upsert { .. } => saw_upsert = true,
            _ => {}
        }
    }
    assert!(saw_upsert, "board updates are broadcast");
    assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
}

#[tokio::test]
async fn profile_updates() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let (_, me) = srv.get(&member, "/auth/status").await;
    assert!(boba_core::team::USER_AVATARS.contains(&me["me"]["avatar"].as_str().unwrap()));

    let (s, v) = srv
        .req(
            &member,
            "PATCH",
            "/me",
            Some(json!({"name": "  Mia ", "avatar": "golden_boba_avatar"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["name"].clone(), v["avatar"].clone()),
        (json!("Mia"), json!("golden_boba_avatar"))
    );
    assert_eq!(
        srv.req(&member, "PATCH", "/me", Some(json!({"avatar": "boba"})))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // Password change: needs the current one, signs out other devices only.
    let other = client();
    srv.post(
        &other,
        "/auth/login",
        json!({"email": "m@test.io", "password": "password123"}),
    )
    .await;
    assert_eq!(srv.get(&other, "/snapshot").await.0, StatusCode::OK);
    let (s, _) = srv
        .req(
            &member,
            "PATCH",
            "/me",
            Some(json!({"new_password": "brand new pw", "current_password": "nope nope"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = srv
        .req(
            &member,
            "PATCH",
            "/me",
            Some(json!({"new_password": "brand new pw", "current_password": "password123"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(srv.get(&member, "/snapshot").await.0, StatusCode::OK);
    assert_eq!(
        srv.get(&other, "/snapshot").await.0,
        StatusCode::UNAUTHORIZED
    );
    let (s, _) = srv
        .post(
            &client(),
            "/auth/login",
            json!({"email": "m@test.io", "password": "brand new pw"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // Unauthenticated.
    assert_eq!(
        srv.req(&client(), "PATCH", "/me", Some(json!({}))).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn notifications_reach_only_their_owner() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let tab = snap.tabs[0].id;
    let dev = agent(&snap, "Boba Dev").id;
    // Owner creates the ticket, the member runs it: member gets RunFinished,
    // both get the review verdict (creator + runner), nobody gets their own action.
    let (_, tk) = srv
        .post(
            &owner,
            "/tickets",
            json!({"tab_id": tab, "title": "Notify me"}),
        )
        .await;
    let t: Id = serde_json::from_value(tk["id"].clone()).unwrap();
    srv.post(
        &member,
        &format!("/tickets/{t}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    srv.until(&member, "done", |s| {
        column_kind(s, ticket(s, t)) == ColumnKind::Done && all_idle(s)
    })
    .await;
    let kinds = |s: &Snapshot| -> Vec<String> {
        let mut k: Vec<String> = s
            .notifications
            .iter()
            .map(|n| boba_server::db::enc(&n.kind))
            .collect();
        k.sort();
        k
    };
    let m = srv.snapshot(&member).await;
    assert_eq!(kinds(&m), vec!["review_verdict", "run_finished"]);
    assert!(m
        .notifications
        .iter()
        .all(|n| !n.read && n.ticket_id == Some(t)));
    let o = srv.snapshot(&owner).await;
    assert_eq!(kinds(&o), vec!["review_verdict"]);

    // Role change -> account notification for the target only.
    let member_id = m.me.as_ref().unwrap().id;
    srv.req(
        &owner,
        "PATCH",
        &format!("/users/{member_id}"),
        Some(json!({"role": "viewer"})),
    )
    .await;
    let (_, list) = srv.get(&member, "/notifications?limit=10").await;
    assert_eq!(list[0]["kind"], "account");
    assert_eq!(list.as_array().unwrap().len(), 3);

    // Mark one read, then all.
    let first: Id = serde_json::from_value(list[0]["id"].clone()).unwrap();
    let (s, _) = srv
        .post(&member, "/notifications/read", json!({"ids": [first]}))
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let m = srv.snapshot(&member).await;
    assert_eq!(m.notifications.iter().filter(|n| !n.read).count(), 2);
    srv.post(&member, "/notifications/read", json!({})).await;
    let m = srv.snapshot(&member).await;
    assert_eq!(m.notifications.iter().filter(|n| !n.read).count(), 0);
    // Someone else's ids are ignored.
    let o = srv.snapshot(&owner).await;
    let theirs = o.notifications[0].id;
    srv.post(&member, "/notifications/read", json!({"ids": [theirs]}))
        .await;
    assert!(!srv.snapshot(&owner).await.notifications[0].read);
}

#[tokio::test]
async fn subscription_runner_only_runs_its_owners_work() {
    use boba_core::team::{RunnerAuth, RunnerScope};
    // The owner's Claude subscription (personal) + a simulated shared runner.
    let personal = FakeRunner::new();
    let shared = FakeRunner::new();
    let mut slot = RunnerSlot::shared(Arc::new(personal.clone()), RunnerAuth::Subscription);
    slot.scope = RunnerScope::Personal;
    slot.owner_email = Some("owner@test.io".into());
    let runners = Runners::new(vec![
        slot,
        RunnerSlot::shared(Arc::new(shared.clone()), RunnerAuth::Demo),
    ]);
    let srv = start_with(tmp_dir(), runners, personal.clone()).await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let owner_id = snap.me.as_ref().unwrap().id;
    assert_eq!(snap.runners.len(), 2);
    let card = snap.runners.iter().find(|r| r.id == personal.id).unwrap();
    assert_eq!(
        (card.scope, card.auth, card.owner_user_id),
        (
            RunnerScope::Personal,
            RunnerAuth::Subscription,
            Some(owner_id)
        )
    );

    let tab = &snap.tabs[0];
    // Real (non-simulated) runs need a repository.
    let (st, repo) = srv
        .post(&owner, "/repos", json!({"name": "app", "path": "/srv/app"}))
        .await;
    assert_eq!(st, StatusCode::OK, "{repo}");
    let (st, v) = srv
        .req(
            &owner,
            "PATCH",
            &format!("/tabs/{}", tab.id),
            Some(json!({"repo_id": repo["id"]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let mine = snap.tickets.iter().find(|t| t.tab_id == tab.id).unwrap().id;
    let theirs = snap
        .tickets
        .iter()
        .filter(|t| t.tab_id == tab.id)
        .nth(1)
        .unwrap()
        .id;
    let dev = agent(&snap, "Boba Dev").id;
    let qa = agent(&snap, "Golden QA").id;
    let (st, v) = srv
        .post(
            &owner,
            &format!("/tickets/{mine}/assign"),
            json!({"agent_id": dev}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = srv
        .post(
            &member,
            &format!("/tickets/{theirs}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    srv.until(&owner, "both approved", |s| {
        [mine, theirs]
            .iter()
            .all(|t| column_kind(s, ticket(s, *t)) == ColumnKind::Done)
            && all_idle(s)
    })
    .await;
    let by = |r: &FakeRunner| -> Vec<(RunKind, Id)> {
        r.specs()
            .iter()
            .map(|s| (s.kind, s.ticket.as_ref().unwrap().id))
            .collect()
    };
    // Owner's work and its auto review: the subscription. Member's: the shared runner.
    let p = by(&personal);
    assert!(
        p.contains(&(RunKind::Work, mine)) && p.contains(&(RunKind::Review, mine)),
        "{p:?}"
    );
    assert!(
        !p.iter().any(|(_, t)| *t == theirs),
        "member work reached the subscription"
    );
    let sh = by(&shared);
    assert!(
        sh.contains(&(RunKind::Work, theirs)) && sh.contains(&(RunKind::Review, theirs)),
        "{sh:?}"
    );

    // The member's runs say they were simulated and point at the shared runner.
    let (_, runs) = srv.get(&member, &format!("/tickets/{theirs}/runs")).await;
    let runs: Vec<Run> = serde_json::from_value(runs).unwrap();
    assert!(runs.iter().all(|r| r.runner_id == Some(shared.id)));
    let (_, logs) = srv
        .get(&member, &format!("/runs/{}/logs", runs[1].id))
        .await;
    assert!(
        logs[0]["text"].as_str().unwrap().starts_with("Simulated"),
        "{logs}"
    );
    let (_, runs) = srv.get(&owner, &format!("/tickets/{mine}/runs")).await;
    let runs: Vec<Run> = serde_json::from_value(runs).unwrap();
    assert!(runs.iter().all(|r| r.runner_id == Some(personal.id)));
    let (_, logs) = srv.get(&owner, &format!("/runs/{}/logs", runs[1].id)).await;
    assert!(!logs[0]["text"].as_str().unwrap().starts_with("Simulated"));

    // Cancels go to the runner that owns the run.
    shared.hang(true);
    let third = snap
        .tickets
        .iter()
        .filter(|t| t.tab_id == tab.id)
        .nth(2)
        .unwrap()
        .id;
    let (_, run) = srv
        .post(
            &member,
            &format!("/tickets/{third}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    let run_id: Id = serde_json::from_value(run["id"].clone()).unwrap();
    srv.until(&member, "running", |s| {
        s.runs
            .iter()
            .any(|r| r.id == run_id && r.status == RunStatus::Running)
    })
    .await;
    srv.post(&member, &format!("/runs/{run_id}/cancel"), json!({}))
        .await;
    srv.until(&member, "cancelled", all_idle).await;
    assert!(shared.script.lock().unwrap().cancelled.contains(&run_id));
    assert!(personal.script.lock().unwrap().cancelled.is_empty());
}

#[tokio::test]
async fn comment_threads_mentions_and_agent_replies() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let viewer = srv.invite(&owner, "v@test.io", "viewer").await;
    let snap = srv.snapshot(&owner).await;
    let owner_id = snap.me.as_ref().unwrap().id;
    let member_id = snap
        .users
        .iter()
        .find(|u| u.email == "m@test.io")
        .unwrap()
        .id;
    let t = snap.tickets[0].id;
    let qa = agent(&snap, "Golden QA");

    // Owner opens a thread mentioning the member.
    let (s, root) = srv
        .post(
            &owner,
            "/comments",
            json!({"ticket_id": t, "anchor": {"on": "ticket_body", "quote": "dark theme"},
            "body": format!("@[Mia](user:{member_id}) can you check this?")}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{root}");
    assert_eq!(root["mentioned_user_ids"], json!([member_id]));
    let root_id: Id = serde_json::from_value(root["id"].clone()).unwrap();
    let tk = ticket(&srv.snapshot(&owner).await, t).clone();
    assert_eq!((tk.comment_count, tk.open_thread_count), (1, 1));
    let m = srv.snapshot(&member).await;
    assert!(m
        .notifications
        .iter()
        .any(|n| boba_server::db::enc(&n.kind) == "mention" && n.comment_id == Some(root_id)));

    // Member replies and asks Golden QA to do it: a run starts with the comment as instructions.
    let (s, reply) = srv
        .post(
            &member,
            "/comments",
            json!({"ticket_id": t, "anchor": {"on": "ticket"}, "parent_id": root_id,
            "body": format!("Sure. @[Golden QA](agent:{}) please add tests", qa.id)}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{reply}");
    assert_eq!(reply["parent_id"], json!(root_id));
    assert_eq!(
        reply["anchor"],
        json!({"on": "ticket_body", "quote": "dark theme"}),
        "replies inherit the root anchor"
    );
    let runs = reply["triggered_run_ids"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    let run_id: Id = serde_json::from_value(runs[0].clone()).unwrap();
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.run_id == run_id)
        .unwrap();
    let instr = spec.extra_instructions.unwrap();
    assert!(
        instr.contains("@Golden QA please add tests") && !instr.contains("agent:"),
        "{instr}"
    );
    // Owner got a Reply notification (thread participant).
    let o = srv.snapshot(&owner).await;
    assert!(o
        .notifications
        .iter()
        .any(|n| boba_server::db::enc(&n.kind) == "reply"));

    // When the run finishes, the agent answers in the thread.
    srv.until(&owner, "agent reply", |s| {
        ticket(s, t).comment_count == 3 && all_idle(s)
    })
    .await;
    let (_, list) = srv.get(&viewer, &format!("/tickets/{t}/comments")).await;
    let list: Vec<boba_core::team::Comment> = serde_json::from_value(list).unwrap();
    let answer = list
        .iter()
        .find(|c| c.author_kind == boba_core::team::CommentAuthorKind::Agent)
        .unwrap();
    assert_eq!((answer.author_id, answer.parent_id), (qa.id, Some(root_id)));
    assert_eq!(answer.body, "done");
    assert_eq!(answer.triggered_run_ids, vec![run_id]);

    // A viewer cannot comment and an agent mention by a viewer is impossible anyway.
    assert_eq!(
        srv.post(
            &viewer,
            "/comments",
            json!({"ticket_id": t, "anchor": {"on": "ticket"}, "body": "hi"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // Reactions toggle; viewers may react.
    let (s, v) = srv
        .post(
            &viewer,
            &format!("/comments/{root_id}/react"),
            json!({"emoji": "🧋"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["reactions"][0]["user_ids"].as_array().unwrap().len(), 1);
    let (_, v) = srv
        .post(
            &viewer,
            &format!("/comments/{root_id}/react"),
            json!({"emoji": "🧋"}),
        )
        .await;
    assert_eq!(v["reactions"], json!([]));
    assert_eq!(
        srv.post(
            &viewer,
            &format!("/comments/{root_id}/react"),
            json!({"emoji": "lol"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // Edit: author only. Resolve: EditTickets, roots only.
    assert_eq!(
        srv.req(
            &member,
            "PATCH",
            &format!("/comments/{root_id}"),
            Some(json!({"body": "hijack"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (s, v) = srv
        .req(
            &owner,
            "PATCH",
            &format!("/comments/{root_id}"),
            Some(json!({"body": "edited"})),
        )
        .await;
    assert_eq!((s, v["body"].clone()), (StatusCode::OK, json!("edited")));
    assert!(v["edited_at"].is_string());
    let reply_id = reply["id"].as_str().unwrap();
    assert_eq!(
        srv.req(
            &member,
            "PATCH",
            &format!("/comments/{reply_id}"),
            Some(json!({"resolved": true}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (s, v) = srv
        .req(
            &member,
            "PATCH",
            &format!("/comments/{root_id}"),
            Some(json!({"resolved": true})),
        )
        .await;
    assert_eq!(
        (s, v["resolved"].clone(), v["resolved_by"].clone()),
        (StatusCode::OK, json!(true), json!(member_id))
    );
    assert_eq!(ticket(&srv.snapshot(&owner).await, t).open_thread_count, 0);

    // Soft delete: author or admin; the thread stays.
    assert_eq!(
        srv.req(&viewer, "DELETE", &format!("/comments/{reply_id}"), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.req(&owner, "DELETE", &format!("/comments/{reply_id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, list) = srv.get(&owner, &format!("/tickets/{t}/comments")).await;
    let gone = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(reply_id))
        .unwrap();
    assert_eq!(
        (gone["deleted"].clone(), gone["body"].clone()),
        (json!(true), json!(""))
    );
    assert_eq!(ticket(&srv.snapshot(&owner).await, t).comment_count, 2);
    let _ = owner_id;
}

#[tokio::test]
async fn busy_agent_answers_why_it_cannot_start() {
    let srv = start().await;
    srv.runner.hang(true);
    let owner = srv.owner().await;
    let snap = srv.snapshot(&owner).await;
    let (t1, t2) = (snap.tickets[0].id, snap.tickets[1].id);
    let dev = agent(&snap, "Boba Dev").id;
    srv.post(
        &owner,
        &format!("/tickets/{t1}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    let (s, c) = srv
        .post(
            &owner,
            "/comments",
            json!({"ticket_id": t2, "anchor": {"on": "ticket"},
            "body": format!("@[Boba Dev](agent:{dev}) and this one?")}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["triggered_run_ids"], json!([]));
    let (_, list) = srv.get(&owner, &format!("/tickets/{t2}/comments")).await;
    let answer = &list.as_array().unwrap()[1];
    assert_eq!(answer["author_kind"], "agent");
    assert!(
        answer["body"].as_str().unwrap().contains("busy"),
        "{answer}"
    );
}

#[tokio::test]
async fn personal_runner_is_never_a_fallback_for_others() {
    use boba_core::team::{RunnerAuth, RunnerScope};
    // Only the owner's subscription, no shared runner at all.
    let personal = FakeRunner::new();
    let mut slot = RunnerSlot::shared(Arc::new(personal.clone()), RunnerAuth::Subscription);
    slot.scope = RunnerScope::Personal;
    slot.owner_email = Some("owner@test.io".into());
    let srv = start_with(tmp_dir(), Runners::new(vec![slot]), personal.clone()).await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let t = snap.tickets[0].id;
    let qa = agent(&snap, "Golden QA").id;
    let (s, v) = srv
        .post(
            &member,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("connect your own Claude Code"));
    // Mentioning an agent is refused the same way (the agent explains in the thread).
    srv.post(
        &member,
        "/comments",
        json!({"ticket_id": t, "anchor": {"on": "ticket"},
        "body": format!("@[Golden QA](agent:{qa}) go")}),
    )
    .await;
    assert!(
        personal.specs().is_empty(),
        "a non-owner's work reached the personal runner"
    );
    let snap = srv.snapshot(&owner).await;
    assert!(snap.runs.is_empty());
    assert!(all_idle(&snap));
}

// ---------------------------------------------------------------- remote runners

type RunnerWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn runner_connect(
    srv: &Srv,
    token: &str,
) -> Result<RunnerWs, Box<tokio_tungstenite::tungstenite::Error>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = srv.base.replace("http://", "ws://") + "/runners/ws";
    let mut req = url.as_str().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
        .map_err(Box::new)
}

async fn runner_send(ws: &mut RunnerWs, v: Value) {
    use futures::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        v.to_string().into(),
    ))
    .await
    .unwrap();
}

/// Next server -> runner JSON frame (None if the socket closed).
async fn runner_recv(ws: &mut RunnerWs) -> Option<Value> {
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("runner ws timeout")
        {
            Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(t.as_str()).unwrap()),
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
            _ => continue,
        }
    }
}

fn hello(auth: &str) -> Value {
    json!({"type": "hello", "name": "mbp", "max_concurrent_runs": 2, "version": "0.1.0",
           "auth": auth, "host": "mia-mbp", "claude_version": "2.1.0"})
}

/// Play a run to completion from the runner side.
async fn runner_finish(ws: &mut RunnerWs, run_id: &Value, verdict: Option<&str>) {
    runner_send(
        ws,
        json!({"type": "update", "run_id": run_id, "update":
        {"type": "started", "worktree_path": "/w", "branch": "boba/x", "session_id": "s1"}}),
    )
    .await;
    runner_send(ws, json!({"type": "update", "run_id": run_id, "update": {"type": "log", "line":
        {"run_id": run_id, "seq": 0, "ts": chrono::Utc::now(), "kind": "text", "text": "hi from the remote", "data": null}}})).await;
    runner_send(ws, json!({"type": "update", "run_id": run_id, "update": {"type": "finished", "outcome":
        {"status": "succeeded", "summary": "remote done", "verdict": verdict, "feedback": "ok", "error": null, "plan": null}}})).await;
}

#[tokio::test]
async fn remote_personal_runner_end_to_end() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let member_id = snap
        .users
        .iter()
        .find(|u| u.email == "m@test.io")
        .unwrap()
        .id;

    // Register (personal, by the member) -> one-time token + command.
    let (s, v) = srv
        .post(
            &member,
            "/runners",
            json!({"name": "mbp", "scope": "personal"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["token"].as_str().unwrap().to_string();
    assert!(v["command"].as_str().unwrap().contains(&token));
    let runner_id: Id = serde_json::from_value(v["runner"]["id"].clone()).unwrap();
    assert_eq!(
        (
            v["runner"]["online"].clone(),
            v["runner"]["owner_user_id"].clone()
        ),
        (json!(false), json!(member_id))
    );
    // Viewers cannot register; members cannot register shared runners.
    assert_eq!(
        srv.post(&member, "/runners", json!({"name": "x", "scope": "shared"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        runner_connect(&srv, "nope").await.is_err(),
        "bad token must be refused"
    );

    // Connect: Hello with a subscription is fine for a personal runner.
    let mut ws = runner_connect(&srv, &token).await.unwrap();
    runner_send(&mut ws, hello("subscription")).await;
    let snap = srv
        .until(&owner, "online", |s| {
            s.runners.iter().any(|r| r.id == runner_id && r.online)
        })
        .await;
    let card = snap.runners.iter().find(|r| r.id == runner_id).unwrap();
    assert_eq!(boba_server::db::enc(&card.auth), "subscription");
    assert_eq!(card.host.as_deref(), Some("mia-mbp"));

    // Real runs need a repository (remote runners clone it from remote_url).
    let (_, repo) = srv
        .post(
            &owner,
            "/repos",
            json!({"name": "app", "path": "/srv/app", "remote_url": "https://github.com/acme/app"}),
        )
        .await;
    srv.req(
        &owner,
        "PATCH",
        &format!("/tabs/{}", snap.tabs[0].id),
        Some(json!({"repo_id": repo["id"]})),
    )
    .await;
    // The member's work goes to their machine; the owner's does not.
    let t = snap.tickets[0].id;
    let t_owner = snap.tickets[1].id;
    let qa = agent(&snap, "Golden QA").id;
    let dev = agent(&snap, "Boba Dev").id;
    let (s, run) = srv
        .post(
            &member,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(run["runner_id"], json!(runner_id));
    let start_msg = runner_recv(&mut ws).await.unwrap();
    assert_eq!(start_msg["type"], "start");
    assert_eq!(start_msg["spec"]["run_id"], run["id"]);
    srv.post(
        &owner,
        &format!("/tickets/{t_owner}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    assert!(srv
        .runner
        .specs()
        .iter()
        .any(|s| s.ticket.as_ref().unwrap().id == t_owner));

    // The remote plays the work run; the auto review (member is responsible) comes back to it.
    runner_finish(&mut ws, &run["id"], None).await;
    let review = runner_recv(&mut ws).await.unwrap();
    assert_eq!(
        (review["type"].clone(), review["spec"]["kind"].clone()),
        (json!("start"), json!("review"))
    );
    runner_finish(&mut ws, &review["spec"]["run_id"], Some("approved")).await;
    srv.until(&owner, "approved", |s| {
        column_kind(s, ticket(s, t)) == ColumnKind::Done && all_idle(s)
    })
    .await;
    let (_, logs) = srv
        .get(
            &member,
            &format!("/runs/{}/logs", run["id"].as_str().unwrap()),
        )
        .await;
    assert!(logs
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["text"] == "hi from the remote"));

    // Disconnect mid-run: the run survives the drop...
    let t2 = snap.tickets[2].id;
    let (_, run2) = srv
        .post(
            &member,
            &format!("/tickets/{t2}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    let start2 = runner_recv(&mut ws).await.unwrap();
    assert_eq!(start2["spec"]["run_id"], run2["id"]);
    runner_send(
        &mut ws,
        json!({"type": "update", "run_id": run2["id"], "update":
        {"type": "started", "worktree_path": "/w", "branch": "b", "session_id": null}}),
    )
    .await;
    srv.until(&member, "running", |s| {
        s.runs
            .iter()
            .any(|r| json!(r.id) == run2["id"] && r.status == RunStatus::Running)
    })
    .await;
    drop(ws);
    srv.until(&owner, "offline", |s| {
        s.runners.iter().any(|r| r.id == runner_id && !r.online)
    })
    .await;
    let s_now = srv.snapshot(&owner).await;
    assert!(
        s_now
            .runs
            .iter()
            .any(|r| json!(r.id) == run2["id"] && r.status == RunStatus::Running),
        "failed too early"
    );
    // ...and reconnecting without that run in Resume means it was lost -> failed.
    let mut ws = runner_connect(&srv, &token).await.unwrap();
    runner_send(&mut ws, hello("subscription")).await;
    runner_send(&mut ws, json!({"type": "resume", "run_ids": []})).await;
    let snap = srv
        .until(&member, "lost run failed", |s| {
            s.runs
                .iter()
                .any(|r| json!(r.id) == run2["id"] && r.status == RunStatus::Failed)
        })
        .await;
    let lost = snap
        .runs
        .iter()
        .find(|r| json!(r.id) == run2["id"])
        .unwrap();
    assert!(
        lost.error.as_deref().unwrap().contains("lost"),
        "{:?}",
        lost.error
    );

    // Removing the runner closes its socket and drops it from the board.
    assert_eq!(
        srv.req(&owner, "DELETE", &format!("/runners/{runner_id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(
        runner_recv(&mut ws).await.is_none(),
        "socket stays open after removal"
    );
    assert!(!srv
        .snapshot(&owner)
        .await
        .runners
        .iter()
        .any(|r| r.id == runner_id));
    let (_, h) = srv.get(&owner, "/history?limit=50").await;
    let kinds: Vec<&str> = h
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"runner_connected") && kinds.contains(&"runner_disconnected"),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn shared_runner_with_a_subscription_is_refused() {
    let srv = start().await;
    let owner = srv.owner().await;
    let (s, v) = srv
        .post(
            &owner,
            "/runners",
            json!({"name": "vps", "scope": "shared"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["token"].as_str().unwrap();
    let runner_id: Id = serde_json::from_value(v["runner"]["id"].clone()).unwrap();
    let mut ws = runner_connect(&srv, token).await.unwrap();
    runner_send(&mut ws, hello("subscription")).await;
    assert!(runner_recv(&mut ws).await.is_none(), "must be closed");
    assert!(!srv
        .snapshot(&owner)
        .await
        .runners
        .iter()
        .any(|r| r.id == runner_id && r.online));
    // With an API key it is accepted.
    let mut ws = runner_connect(&srv, token).await.unwrap();
    runner_send(&mut ws, hello("api_key")).await;
    srv.until(&owner, "online", |s| {
        s.runners.iter().any(|r| r.id == runner_id && r.online)
    })
    .await;
}

#[tokio::test]
async fn connector_secrets_never_reach_a_members_personal_runner() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let (s, conn) = srv
        .post(
            &owner,
            "/connectors",
            json!({"name": "sentry", "kind": "sentry",
            "transport": {"type": "stdio", "command": "npx", "args": ["sentry-mcp"]},
            "secrets": {"SENTRY_TOKEN": "s3cr3t-token-value"}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{conn}");
    let qa = agent(&snap, "Golden QA").id;
    let dev = agent(&snap, "Boba Dev").id;
    for a in [qa, dev] {
        let (s, v) = srv
            .req(
                &owner,
                "PATCH",
                &format!("/agents/{a}"),
                Some(json!({"connector_ids": [conn["id"]]})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (_, repo) = srv
        .post(
            &owner,
            "/repos",
            json!({"name": "app", "path": "/srv/app", "remote_url": "https://github.com/acme/app"}),
        )
        .await;
    srv.req(
        &owner,
        "PATCH",
        &format!("/tabs/{}", snap.tabs[0].id),
        Some(json!({"repo_id": repo["id"]})),
    )
    .await;

    // The member's own machine: the run starts, without the connector.
    let (_, v) = srv
        .post(
            &member,
            "/runners",
            json!({"name": "mbp", "scope": "personal"}),
        )
        .await;
    let mut ws = runner_connect(&srv, v["token"].as_str().unwrap())
        .await
        .unwrap();
    runner_send(&mut ws, hello("subscription")).await;
    let rid = v["runner"]["id"].clone();
    srv.until(&owner, "online", |s| {
        s.runners.iter().any(|r| json!(r.id) == rid && r.online)
    })
    .await;
    let t = snap.tickets[0].id;
    let (_, run) = srv
        .post(
            &member,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": qa}),
        )
        .await;
    let start = runner_recv(&mut ws).await.unwrap();
    assert_eq!(start["spec"]["mcp_servers"], json!([]));
    assert!(
        !start.to_string().contains("s3cr3t"),
        "secret leaked to a personal runner"
    );
    let (_, logs) = srv
        .get(
            &member,
            &format!("/runs/{}/logs", run["id"].as_str().unwrap()),
        )
        .await;
    assert!(logs.as_array().unwrap().iter().any(|l| l["text"]
        .as_str()
        .unwrap()
        .contains("Connector sentry is not available")));

    // The built-in runner (server side) still gets it.
    let t2 = snap.tickets[1].id;
    srv.post(
        &owner,
        &format!("/tickets/{t2}/assign"),
        json!({"agent_id": dev}),
    )
    .await;
    srv.until(&owner, "dev ran", |s| {
        s.runs.iter().any(|r| r.ticket_id == Some(t2))
    })
    .await;
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.ticket.as_ref().is_some_and(|x| x.id == t2))
        .unwrap();
    assert_eq!(spec.mcp_servers.len(), 1);
    assert_eq!(
        spec.mcp_servers[0]
            .secrets
            .get("SENTRY_TOKEN")
            .map(String::as_str),
        Some("s3cr3t-token-value")
    );
}

// ---------------------------------------------------------------- quests

#[tokio::test]
async fn quests_run_now_schedule_and_results() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let pinky = agent(&snap, "Pinky SEO").id;

    // Preview: valid cron with a description, invalid one explained.
    let (_, p) = srv
        .post(
            &member,
            "/jobs/preview-schedule",
            json!({"type": "cron", "expr": "0 9 * * 1", "timezone": "Europe/Paris"}),
        )
        .await;
    assert_eq!(
        (p["valid"].clone(), p["next"].as_array().unwrap().len()),
        (json!(true), 5)
    );
    let (_, p) = srv
        .post(
            &member,
            "/jobs/preview-schedule",
            json!({"type": "interval", "minutes": 1}),
        )
        .await;
    assert_eq!(p["valid"], false);
    assert!(p["error"].as_str().unwrap().contains("5 minutes"));

    // Only ManageAutomations creates quests.
    let body = json!({"name": "Weekly SEO audit", "agent_id": pinky, "prompt": "Audit the docs pages",
        "schedule": {"type": "interval", "minutes": 60}, "output": "report"});
    assert_eq!(
        srv.post(&member, "/jobs", body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let (s, job) = srv.post(&owner, "/jobs", body).await;
    assert_eq!(s, StatusCode::OK, "{job}");
    let job_id: Id = serde_json::from_value(job["id"].clone()).unwrap();
    let next: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(job["next_run_at"].clone()).unwrap();
    assert!((next - chrono::Utc::now()).num_minutes() >= 59);
    assert_eq!(srv.snapshot(&member).await.jobs.len(), 1);

    // Run now (RunAgents is enough): a job run without a ticket.
    let (s, run) = srv
        .post(&member, &format!("/jobs/{job_id}/run"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(
        (
            run["kind"].clone(),
            run["job_id"].clone(),
            run["ticket_id"].clone()
        ),
        (json!("job"), json!(job_id), Value::Null)
    );
    let snap = srv
        .until(&owner, "quest done", |s| {
            s.jobs[0].last_result.is_some() && all_idle(s)
        })
        .await;
    let j = &snap.jobs[0];
    assert_eq!((j.runs_count, j.last_result.as_deref()), (1, Some("done")));
    assert_eq!(agent(&snap, "Pinky SEO").xp, 25);
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.kind == RunKind::Job)
        .unwrap();
    let js = spec.job.unwrap();
    assert_eq!(
        (js.prompt.as_str(), js.previous_result.clone()),
        ("Audit the docs pages", None)
    );
    assert!(spec.ticket.is_none() && spec.repo.is_none());
    // Creator and the member who pressed Run now are told.
    for c in [&owner, &member] {
        let n = srv.snapshot(c).await.notifications;
        assert!(n
            .iter()
            .any(|n| boba_server::db::enc(&n.kind) == "job_result" && n.job_id == Some(job_id)));
    }
    let (_, runs) = srv.get(&member, &format!("/jobs/{job_id}/runs")).await;
    assert_eq!(runs.as_array().unwrap().len(), 1);

    // The next run gets the previous report.
    srv.post(&owner, &format!("/jobs/{job_id}/run"), json!({}))
        .await;
    srv.until(&owner, "second run", |s| {
        s.jobs[0].runs_count == 2 && all_idle(s)
    })
    .await;
    let second = srv
        .runner
        .specs()
        .into_iter()
        .filter(|s| s.kind == RunKind::Job)
        .nth(1)
        .unwrap();
    assert_eq!(second.job.unwrap().previous_result.as_deref(), Some("done"));

    // Scheduled firing: a tick after next_run_at starts it and moves next_run_at on.
    let later = chrono::Utc::now() + chrono::Duration::minutes(61);
    boba_server::jobs::run_due(&srv.state, later).await.unwrap();
    let snap = srv
        .until(&owner, "scheduled run", |s| {
            s.jobs[0].runs_count == 3 && all_idle(s)
        })
        .await;
    let next2 = snap.jobs[0].next_run_at.unwrap();
    assert!(
        next2 > later + chrono::Duration::minutes(59),
        "next fire is computed from the tick"
    );

    // skip_if_running: a second Run now while it runs is refused.
    srv.runner.hang(true);
    srv.post(&owner, &format!("/jobs/{job_id}/run"), json!({}))
        .await;
    let (s, v) = srv
        .post(&owner, &format!("/jobs/{job_id}/run"), json!({}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(
        srv.req(&owner, "DELETE", &format!("/jobs/{job_id}"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );

    // Disable: no next fire time. Bad schedule on update: 400.
    let (s, v) = srv
        .req(
            &owner,
            "PATCH",
            &format!("/jobs/{job_id}"),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!((s, v["next_run_at"].clone()), (StatusCode::OK, Value::Null));
    let (s, _) = srv
        .req(
            &owner,
            "PATCH",
            &format!("/jobs/{job_id}"),
            Some(json!({"schedule": {"type": "cron", "expr": "nope", "timezone": "UTC"}})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn repo_settings_persist_and_reach_the_runner() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let (_, repo) = srv
        .post(
            &owner,
            "/repos",
            json!({"name": "app", "path": "/srv/app", "remote_url": "https://github.com/acme/app"}),
        )
        .await;
    assert_eq!(repo["ai_attribution"], true);
    let rid = repo["id"].as_str().unwrap();
    assert_eq!(
        srv.req(
            &member,
            "PATCH",
            &format!("/repos/{rid}"),
            Some(json!({"ai_attribution": false}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (s, v) = srv
        .req(
            &owner,
            "PATCH",
            &format!("/repos/{rid}"),
            Some(json!({"ai_attribution": false, "default_branch": "trunk"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let snap2 = srv.snapshot(&owner).await;
    let r = snap2
        .repos
        .iter()
        .find(|r| r.id.to_string() == rid)
        .unwrap();
    assert_eq!(
        (r.ai_attribution, r.default_branch.as_str()),
        (false, "trunk")
    );
    // A run in a tab using that repo carries the flag.
    srv.req(
        &owner,
        "PATCH",
        &format!("/tabs/{}", snap.tabs[0].id),
        Some(json!({"repo_id": rid})),
    )
    .await;
    let t = snap
        .tickets
        .iter()
        .find(|t| t.tab_id == snap.tabs[0].id)
        .unwrap()
        .id;
    srv.post(
        &owner,
        &format!("/tickets/{t}/assign"),
        json!({"agent_id": agent(&snap, "Boba Dev").id}),
    )
    .await;
    srv.until(&owner, "ran", |s| {
        s.runs.iter().any(|r| r.ticket_id == Some(t))
    })
    .await;
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.ticket.as_ref().is_some_and(|x| x.id == t))
        .unwrap();
    let repo = spec.repo.unwrap();
    assert_eq!(
        (repo.ai_attribution, repo.default_branch.as_str()),
        (false, "trunk")
    );
}

// ---------------------------------------------------------------- board tools (MCP)

async fn mcp(srv: &Srv, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
    let res = reqwest::Client::new()
        .post(format!("{}/mcp", srv.base))
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap();
    let s = res.status();
    (s, res.json().await.unwrap_or(Value::Null))
}

fn tool_text(v: &Value) -> (bool, Value) {
    let r = &v["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("");
    (
        r["isError"].as_bool().unwrap_or(true),
        serde_json::from_str(text).unwrap_or(json!(text)),
    )
}

#[tokio::test]
async fn board_tools_let_a_job_file_tickets_until_the_run_ends() {
    let srv = start().await;
    srv.runner.hang(true);
    let owner = srv.owner().await;
    let snap = srv.snapshot(&owner).await;
    let seo_tab = snap.tabs.iter().find(|t| t.name == "SEO").unwrap().id;
    let pinky = agent(&snap, "Pinky SEO").id;
    srv.req(
        &owner,
        "PATCH",
        &format!("/agents/{pinky}"),
        Some(json!({"board_tools": true})),
    )
    .await;
    let (_, job) = srv
        .post(
            &owner,
            "/jobs",
            json!({"name": "Triage", "agent_id": pinky, "prompt": "File tickets",
            "schedule": {"type": "manual"}, "output": "tickets", "tab_id": seo_tab}),
        )
        .await;
    let (_, run) = srv
        .post(
            &owner,
            &format!("/jobs/{}/run", job["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    let run_id = run["id"].as_str().unwrap().to_string();
    srv.until(&owner, "job running", |s| {
        s.runs
            .iter()
            .any(|r| r.id.to_string() == run_id && r.status == RunStatus::Running)
    })
    .await;
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.run_id.to_string() == run_id)
        .unwrap();
    let bt = spec.board_tools.expect("board tools in the spec");
    assert!(bt.url.ends_with("/api/mcp"));
    let token = bt.token;

    assert_eq!(
        mcp(&srv, "wrong", "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (s, init) = mcp(&srv, &token, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude"}})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(init["result"]["serverInfo"]["name"], "boba-board");
    let res = reqwest::Client::new()
        .post(format!("{}/mcp", srv.base))
        .bearer_auth(&token)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let (_, list) = mcp(&srv, &token, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"create_ticket") && names.contains(&"search_history"),
        "{names:?}"
    );

    // File a ticket: lands in the job's tab, Todo, source = job, attributed to the agent.
    let (_, r) = mcp(&srv, &token, "tools/call", json!({"name": "create_ticket", "arguments":
        {"title": "Fix title on /pricing", "body": "Too long", "tags": ["seo", "SEO"], "priority": "high"}})).await;
    let (err, created) = tool_text(&r);
    assert!(!err, "{created}");
    let tid: Id = serde_json::from_value(created["id"].clone()).unwrap();
    let snap = srv.snapshot(&owner).await;
    let t = ticket(&snap, tid);
    assert_eq!(
        (t.tab_id, column_kind(&snap, t), t.tags.clone(), t.priority),
        (
            seo_tab,
            ColumnKind::Todo,
            vec!["seo".to_string()],
            Priority::High
        )
    );
    assert!(matches!(
        t.source,
        boba_core::automation::TicketSource::Job { .. }
    ));
    let created_ev = snap
        .history
        .iter()
        .find(|e| e.ticket_id == Some(tid))
        .unwrap();
    assert_eq!(
        (created_ev.actor_kind, created_ev.actor_id),
        (boba_core::event::ActorKind::Agent, Some(pinky))
    );

    // Read, search, comment, update; bad input is a tool error, not a crash.
    let (_, r) = mcp(
        &srv,
        &token,
        "tools/call",
        json!({"name": "list_tickets", "arguments": {"text": "/pricing"}}),
    )
    .await;
    assert_eq!(tool_text(&r).1.as_array().unwrap().len(), 1);
    let (_, r) = mcp(&srv, &token, "tools/call", json!({"name": "comment_ticket", "arguments": {"ticket_id": tid, "body": "Found by the weekly audit"}})).await;
    assert!(!tool_text(&r).0);
    let (_, r) = mcp(
        &srv,
        &token,
        "tools/call",
        json!({"name": "update_ticket", "arguments": {"ticket_id": tid, "priority": "urgent"}}),
    )
    .await;
    assert!(!tool_text(&r).0);
    let (_, r) = mcp(
        &srv,
        &token,
        "tools/call",
        json!({"name": "get_ticket", "arguments": {"ticket_id": tid}}),
    )
    .await;
    let (_, got) = tool_text(&r);
    assert_eq!(
        (
            got["priority"].clone(),
            got["comments"].as_array().unwrap().len()
        ),
        (json!("urgent"), 1)
    );
    let (_, r) = mcp(
        &srv,
        &token,
        "tools/call",
        json!({"name": "create_ticket", "arguments": {}}),
    )
    .await;
    assert!(tool_text(&r).0, "missing title must be an error");
    let (_, r) = mcp(
        &srv,
        &token,
        "tools/call",
        json!({"name": "delete_everything", "arguments": {}}),
    )
    .await;
    assert!(tool_text(&r).0);

    // The token dies with the run.
    srv.post(&owner, &format!("/runs/{run_id}/cancel"), json!({}))
        .await;
    srv.until(&owner, "cancelled", all_idle).await;
    assert_eq!(
        mcp(&srv, &token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn read_only_runs_only_get_read_tools() {
    let srv = start().await;
    srv.runner.silent(true); // the review stays live (and its token valid)
    let owner = srv.owner().await;
    let snap = srv.snapshot(&owner).await;
    let uncle = agent(&snap, "Uncle Review").id;
    srv.req(
        &owner,
        "PATCH",
        &format!("/agents/{uncle}"),
        Some(json!({"board_tools": true})),
    )
    .await;
    // A ticket sitting in a review column: assigning the reviewer starts a review run.
    let tab = &snap.tabs[0];
    let review_col = tab
        .columns
        .iter()
        .find(|c| c.kind == ColumnKind::Review)
        .unwrap()
        .id;
    let t = snap.tickets.iter().find(|t| t.tab_id == tab.id).unwrap().id;
    srv.post(
        &owner,
        &format!("/tickets/{t}/move"),
        json!({"column_id": review_col, "position": 1.0}),
    )
    .await;
    srv.until(&owner, "review started", |s| {
        s.runs
            .iter()
            .any(|r| r.kind == RunKind::Review && !r.status.is_terminal())
    })
    .await;
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.kind == RunKind::Review)
        .unwrap();
    let token = spec.board_tools.unwrap().token;
    let (_, list) = mcp(&srv, &token, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"create_ticket") && names.contains(&"get_ticket"),
        "{names:?}"
    );
    let (_, r) = mcp(
        &srv,
        &token,
        "tools/call",
        json!({"name": "create_ticket", "arguments": {"title": "x"}}),
    )
    .await;
    let (err, msg) = tool_text(&r);
    assert!(
        err && msg.as_str().unwrap_or("").contains("read-only"),
        "{r}"
    );
}

// ---------------------------------------------------------------- concierge

async fn user_ws(srv: &Srv, email: &str, password: &str) -> RunnerWs {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let res = reqwest::Client::new()
        .post(format!("{}/auth/login", srv.base))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .unwrap();
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let url = srv.base.replace("http://", "ws://") + "/ws";
    let mut req = url.as_str().into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// Wait for a frame of `type`, or None after `secs`.
async fn ws_find(ws: &mut RunnerWs, ty: &str, secs: u64) -> Option<Value> {
    use tokio_tungstenite::tungstenite::Message;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v["type"] == ty {
                    return Some(v);
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

#[tokio::test]
async fn concierge_plans_for_the_asker_and_executes_with_their_rights() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let tab = snap.tabs[0].id;
    let concierge = agent(&snap, "Concierge").id;
    srv.runner.script.lock().unwrap().plan = Some(json!({
        "reply": "Sure! Here's the plan.",
        "actions": [
            {"label": "Create ticket \"Fix login\"", "action": {"action": "create_ticket",
                "ticket": {"tab_id": tab, "title": "Fix login", "body": null, "tags": ["bug"], "priority": "high",
                           "color": null, "repo_id": null, "column_id": null}}},
            {"label": "Hire a designer", "action": {"action": "create_agent", "agent": {"name": "Designer",
                "role": "Design", "sprite": "pinky_boba", "color": null, "tagline": null, "system_prompt": "",
                "skill_ids": [], "model": null, "permission_mode": null, "allowed_tools": null, "max_turns": null,
                "connector_ids": null, "board_tools": null}}},
            {"label": "nonsense", "action": {"action": "launch_rockets"}}
        ],
        "question": null
    }));
    let mut member_ws = user_ws(&srv, "m@test.io", "password123").await;
    let mut owner_ws = user_ws(&srv, "owner@test.io", "correct horse").await;

    let (s, run) = srv
        .post(
            &member,
            "/command",
            json!({"text": "file a ticket for the login bug and hire a designer",
            "tab_id": tab, "ticket_id": null, "agent_id": null, "voice": true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(
        (run["kind"].clone(), run["agent_id"].clone()),
        (json!("command"), json!(concierge))
    );

    // The plan reaches the asker only, with permissions marked for them.
    let plan = ws_find(&mut member_ws, "command_plan", 10)
        .await
        .expect("plan for the asker")["plan"]
        .clone();
    assert_eq!(plan["reply"], "Sure! Here's the plan.");
    let actions = plan["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2, "unknown actions are dropped");
    assert_eq!(
        (
            actions[0]["allowed"].clone(),
            actions[0]["required"].clone()
        ),
        (json!(true), json!("edit_tickets"))
    );
    assert_eq!(
        (
            actions[1]["allowed"].clone(),
            actions[1]["required"].clone()
        ),
        (json!(false), json!("manage_agents"))
    );
    assert!(
        ws_find(&mut owner_ws, "command_plan", 1).await.is_none(),
        "someone else's plan leaked"
    );

    // The run was read-only and carried the digest + context.
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.kind == RunKind::Command)
        .unwrap();
    assert_eq!(spec.agent.permission_mode, PermissionMode::Plan);
    let cmd = spec.command.unwrap();
    assert!(cmd.voice && cmd.user_name == "m@test.io" && cmd.tab_id == Some(tab));
    assert!(cmd.board_digest["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "Pinky SEO"));
    assert!(spec.ticket.is_none() && spec.mcp_servers.is_empty());

    // Execute: the member's own rights apply, the plan runs once, only for its owner.
    let run_id = run["id"].clone();
    let exec = json!({"run_id": run_id, "actions": [actions[0]["action"], actions[1]["action"]]});
    assert_eq!(
        srv.post(&owner, "/command/execute", exec.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let (s, results) = srv.post(&member, "/command/execute", exec.clone()).await;
    assert_eq!(s, StatusCode::OK, "{results}");
    assert_eq!(results[0]["ok"], true);
    assert_eq!(results[0]["entity"]["entity"], "ticket");
    assert_eq!(results[1]["ok"], false);
    assert_eq!(
        srv.post(&member, "/command/execute", exec).await.0,
        StatusCode::CONFLICT
    );
    let snap = srv.snapshot(&owner).await;
    assert!(snap
        .tickets
        .iter()
        .any(|t| t.title == "Fix login" && t.priority == Priority::High));
    assert!(!snap.agents.iter().any(|a| a.name == "Designer"));
    let kinds: Vec<String> = snap
        .history
        .iter()
        .map(|e| boba_server::db::enc(&e.kind))
        .collect();
    assert!(
        kinds.contains(&"command_issued".to_string())
            && kinds.contains(&"command_executed".to_string())
    );

    // The Concierge is built in.
    assert_eq!(
        srv.req(&owner, "DELETE", &format!("/agents/{concierge}"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

// ---------------------------------------------------------------- GitHub repo sync

#[derive(Default)]
struct MockGh {
    issues: Vec<Value>,
    pulls: Vec<Value>,
    /// (method + path, body) of every write.
    writes: Vec<(String, Value)>,
}

type Gh = Arc<Mutex<MockGh>>;

fn gh_issue(n: i64, title: &str) -> Value {
    json!({"number": n, "title": title, "body": "From GitHub", "html_url": format!("https://github.com/acme/app/issues/{n}"),
           "user": {"login": "octo"}, "labels": [{"name": "bug"}], "state": "open",
           "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-01T00:00:00Z"})
}

fn gh_pull(n: i64, title: &str, draft: bool) -> Value {
    json!({"number": n, "title": title, "body": "A contribution", "html_url": format!("https://github.com/acme/app/pull/{n}"),
           "user": {"login": "contributor"}, "labels": [], "state": "open", "draft": draft,
           "head": {"ref": format!("feature-{n}"), "sha": "abc", "repo": {"full_name": "contributor/app"}},
           "base": {"ref": "main", "sha": "def", "repo": {"full_name": "acme/app"}},
           "merged_at": null, "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-01T00:00:00Z"})
}

async fn mock_github(gh: Gh) -> String {
    use axum::extract::{Path as P, State as S};
    use axum::routing::{get, post};
    let app =
        axum::Router::new()
            .route(
                "/repos/acme/app/issues",
                get(|S(g): S<Gh>| async move {
                    axum::Json(Value::Array(g.lock().unwrap().issues.clone()))
                }),
            )
            .route(
                "/repos/acme/app/pulls",
                get(|S(g): S<Gh>| async move {
                    axum::Json(Value::Array(g.lock().unwrap().pulls.clone()))
                }),
            )
            .route(
                "/repos/acme/app/issues/{n}/comments",
                post(
                    |S(g): S<Gh>, P(n): P<i64>, axum::Json(b): axum::Json<Value>| async move {
                        g.lock().unwrap().writes.push((format!("comment #{n}"), b));
                        axum::Json(json!({"id": 1, "html_url": "https://github.com/x"}))
                    },
                ),
            )
            .route(
                "/repos/acme/app/pulls/{n}/reviews",
                post(
                    |S(g): S<Gh>, P(n): P<i64>, axum::Json(b): axum::Json<Value>| async move {
                        let event = b["event"].as_str().unwrap_or("").to_string();
                        g.lock()
                            .unwrap()
                            .writes
                            .push((format!("review #{n} {event}"), b));
                        if event == "COMMENT" {
                            (
                                axum::http::StatusCode::OK,
                                axum::Json(json!({"id": 2, "html_url": "https://github.com/y"})),
                            )
                        } else {
                            // The token's user "authored" the PR: GitHub refuses approve / request changes.
                            (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                axum::Json(
                                    json!({"message": "Can not approve your own pull request"}),
                                ),
                            )
                        }
                    },
                ),
            )
            .route(
                "/repos/acme/app/issues/{n}",
                axum::routing::patch(
                    |S(g): S<Gh>, P(n): P<i64>, axum::Json(b): axum::Json<Value>| async move {
                        g.lock().unwrap().writes.push((format!("patch #{n}"), b));
                        axum::Json(json!({}))
                    },
                ),
            )
            .with_state(gh);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn wait_writes(gh: &Gh, what: &str) -> Vec<(String, Value)> {
    for _ in 0..200 {
        let w = gh.lock().unwrap().writes.clone();
        if w.iter().any(|(k, _)| k.starts_with(what)) {
            return w;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "no GitHub write starting with {what}: {:?}",
        gh.lock().unwrap().writes
    );
}

#[tokio::test]
async fn github_sync_imports_reviews_and_reports_back() {
    let gh: Gh = Arc::default();
    {
        let mut g = gh.lock().unwrap();
        g.issues = vec![gh_issue(1, "Crash on login")];
        g.pulls = vec![gh_pull(2, "Add dark mode", false), gh_pull(3, "WIP", true)];
    }
    let api = mock_github(gh.clone()).await;
    let runner = FakeRunner::new();
    let r: Arc<dyn Runner> = Arc::new(runner.clone());
    let mut cfg = Config::for_tests(tmp_dir());
    cfg.github_webhook_secret = Some("hook-secret".into());
    let srv = start_cfg(cfg, Runners::from(r), runner).await;
    *srv.state.github.lock().await =
        Some(boba_github::GitHub::with_api("test-token", &api).unwrap());
    let owner = srv.owner().await;
    let snap = srv.snapshot(&owner).await;
    let tab = snap.tabs[0].id;

    let (_, repo) = srv
        .post(&owner, "/repos", json!({"name": "app", "path": "/srv/app", "remote_url": "https://github.com/acme/app.git"}))
        .await;
    let rid = repo["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .req(&owner, "PATCH", &format!("/repos/{rid}"), Some(json!({"sync": {
            "enabled": true, "target_tab_id": tab, "import_issues": true, "issue_labels": [], "import_prs": true,
            "skip_draft_prs": true, "comment_back": true, "post_reviews": true, "close_issues_on_done": true,
            "poll_minutes": 60, "last_synced_at": null, "last_error": null}})))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // First sync: one issue to Todo, one PR to the review column (draft skipped).
    let (s, report) = srv
        .post(&owner, &format!("/repos/{rid}/sync"), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{report}");
    assert_eq!(
        (
            report["imported_issues"].clone(),
            report["imported_prs"].clone()
        ),
        (json!(1), json!(1))
    );
    let snap = srv.snapshot(&owner).await;
    let issue_t = snap
        .tickets
        .iter()
        .find(|t| t.title == "Crash on login")
        .unwrap()
        .clone();
    let pr_t = snap
        .tickets
        .iter()
        .find(|t| t.title == "Add dark mode")
        .unwrap()
        .clone();
    assert_eq!(column_kind(&snap, &issue_t), ColumnKind::Todo);
    assert_eq!(issue_t.tags, vec!["bug".to_string()]);
    assert!(matches!(
        issue_t.source,
        boba_core::automation::TicketSource::GithubIssue { number: 1, .. }
    ));
    assert_eq!(
        (pr_t.pr_number, pr_t.branch.as_deref()),
        (Some(2), Some("feature-2"))
    );
    assert!(snap.repos[0].sync.last_synced_at.is_some());

    // The imported PR is auto reviewed; the verdict goes back as a review (422 -> comment).
    srv.until(&owner, "pr reviewed", |s| {
        column_kind(s, ticket(s, pr_t.id)) == ColumnKind::Done && all_idle(s)
    })
    .await;
    let w = wait_writes(&gh, "review #2 COMMENT").await;
    assert!(w.iter().any(|(k, _)| k == "review #2 APPROVE"), "{w:?}");
    let review_spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.kind == RunKind::Review)
        .unwrap();
    assert!(matches!(
        review_spec.ticket.unwrap().source,
        boba_core::automation::TicketSource::GithubPr { .. }
    ));

    // Idempotent; title edits upstream flow in.
    gh.lock().unwrap().issues = vec![gh_issue(1, "Crash on login (Safari)")];
    let (_, report) = srv
        .post(&owner, &format!("/repos/{rid}/sync"), json!({}))
        .await;
    assert_eq!(
        (
            report["imported_issues"].clone(),
            report["imported_prs"].clone(),
            report["updated"].clone()
        ),
        (json!(0), json!(0), json!(1))
    );
    assert_eq!(
        ticket(&srv.snapshot(&owner).await, issue_t.id).title,
        "Crash on login (Safari)"
    );

    // Work on the issue: summary comment back, and the issue is closed once it is Done.
    let dev = agent(&snap, "Boba Dev").id;
    srv.post(
        &owner,
        &format!("/tickets/{}/assign", issue_t.id),
        json!({"agent_id": dev}),
    )
    .await;
    srv.until(&owner, "issue done", |s| {
        column_kind(s, ticket(s, issue_t.id)) == ColumnKind::Done && all_idle(s)
    })
    .await;
    wait_writes(&gh, "comment #1").await;
    let w = wait_writes(&gh, "patch #1").await;
    let close = w.iter().find(|(k, _)| k == "patch #1").unwrap();
    assert_eq!(
        (close.1["state"].clone(), close.1["state_reason"].clone()),
        (json!("closed"), json!("completed"))
    );

    // Closed upstream -> Done here.
    gh.lock().unwrap().issues = vec![gh_issue(1, "Crash on login (Safari)"), gh_issue(4, "Typo")];
    srv.post(&owner, &format!("/repos/{rid}/sync"), json!({}))
        .await;
    gh.lock().unwrap().issues = vec![];
    let (_, report) = srv
        .post(&owner, &format!("/repos/{rid}/sync"), json!({}))
        .await;
    assert_eq!(report["closed"], 1);
    let snap = srv.snapshot(&owner).await;
    let typo = snap.tickets.iter().find(|t| t.title == "Typo").unwrap();
    assert_eq!(column_kind(&snap, typo), ColumnKind::Done);

    // Webhook: bad signature 401, good one 200 (and triggers a sync).
    let body = br#"{"zen": "Keep it logically awesome."}"#;
    let post = |sig: String| {
        reqwest::Client::new()
            .post(format!("{}/github/webhook", srv.base))
            .header("x-github-event", "ping")
            .header("x-hub-signature-256", sig)
            .body(body.to_vec())
            .send()
    };
    assert_eq!(
        post("sha256=00".into()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let good = boba_github::webhook::sign(b"hook-secret", body);
    assert_eq!(post(good).await.unwrap().status(), StatusCode::OK);
}

// ---------------------------------------------------------------- GitHub App

/// Throwaway RSA key for the test app (never used anywhere else).
const TEST_APP_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDnr00Gy+mIf6Zk\nYBLBrs91EZHdqBhIGzDTFhMlvcWWDoR9ziAupOi4Y7+3YxPwpetqxQAxjXomlyyR\n8VLTOX53itsTwyaX1IRW3pUkHa8zi0OSMBJJiCjrhhszlHoNGwOEBfTpvzs4ujMp\nmSdnUkyV2M5+QWa+8b6rMNft8PohHFYoYb+5vIlzUsf/IVZTi8B+liYogNMe5juS\nWoYQ8QsiNBTLjsF8SDy1vnCrPE8sqsOB/Znz6Rd1l1UXpzZ87I8S/sJ7NyUCHrAX\nbqnS7UAS/KE6t63GEcFvvWtiP+uDJU7pwQIYOPkLpihllhTaO5meE+OdocKyfPb+\nUICxLSyvAgMBAAECggEAQYFJbHYD4dGBmJinRPapdkaQ8NRBhUWW0N/Jax7UOiCB\nqlY4uCdtGxEs1+td7Gjk/kuTye7fzgBpHBxYmBsF3EO4vEYtZ7kw76TAEzWHXzHt\nWWxZB0xFkY3ZrAQ7W/A61r2eaOZmFpxiscy5HNfiim2BuKXCKJOOHrXRH1fGSMzB\n//F2nQ8kXgeHUXKsxWUO6ZfsTKSMNSoFnjhokWS2k1C+Imana2HFu4ISVvEnJkPM\nDBbvbXARqsKd31S4VDyMsEdk8A1bMaROyA9OkCLNYTD/PvD8ZSfekUS7buc1ChO1\nT5fPYwYxE4eqPiJ1FQ8mLv1E/n+TJNOOQkrJIG/7bQKBgQD2WtXrohs+TpXpdsmW\niXeDlCHSGwzNasyeaSvDAgqlX3By3tIiF9ergmnK84AQPBJh6UkfZu43cc0/dDMX\nzcu/EgbWFTbtM7soXu5I9KuZ9AQgscUVnBZ6TqeEHQE7OPP5Le5oOc409gss6b1N\n7aR0UCVfXyHm+okY6+jr97HkpQKBgQDwwW4k+gTtX42X+4SyJTJYYu/7NYcv1rut\nJHURDRmc9iJFgH4tewMYl2+QCJs0P13myhN2T537p9rnR7/5LL5AZ5dTxOB5B9lE\n/8TjqAR3I/Iz9GysUb1jCQcxlXbkYqLoxy86uoI5S22u97r5Lc/Hv69POqvoc4Gf\nlckRAGWHwwKBgQC7sQsoonIVL3Rh0iHWMle/BL4bwIYpfwP0cPZpdYW85xDaQ89f\nMEEdp5eIi6p3M/1hYYZXiWlXDrosbRB8Q0vHACn7M2CMqgwevuzW4qpW+iWm7ynT\nw/8QIhrYuex1PXgL0aXA/MGRch+jkVWsKLFR+Lnwhp9ixmBrf1kPZsC/DQKBgQC+\neaq04oHHc5PIrJudfcRJHW2UKBNei9tL7s4XwW+EOU+AAVrSjMI0UItZPxXbzk5y\na2nebrslAPY1iRSfBQE4Xzzgb6DQ4J7K8jhJjQKLaAsVg8n+w2S818WzOvcnVcph\ntETvh+PDhKOwjBqHTfFXrZX7bwXXEJt7MLw1EUVClQKBgGNyJKcai1T5bwfPclnO\nLj7XyIp02BBuShBBpTF0CdJm+WKd7dlxWAec5SdFYJTU4AfS2xX11WkCEbo+tOQY\nbwdoo0JYzXWl2wbImUfT+w/FRwbFSatNtoNwtch1cO4GSGL99ZDa02vQsZXTGdRk\nQ2Mt1qPHsJpGE+JMT5G934Rn\n-----END PRIVATE KEY-----";

#[derive(Default)]
struct MockApp {
    token_mints: u32,
    jwt_seen: Option<String>,
}

async fn mock_github_app(m: Arc<Mutex<MockApp>>) -> String {
    use axum::extract::{Path as P, State as S};
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    type St = Arc<Mutex<MockApp>>;
    let app = axum::Router::new()
        .route("/app-manifests/{code}/conversions", post(|P(code): P<String>| async move {
            assert_eq!(code, "the-code");
            axum::Json(json!({"id": 42, "slug": "boba-agent-x", "name": "Boba Agent", "client_id": "Iv1.abc",
                "client_secret": "csecret", "webhook_secret": "whsec", "pem": TEST_APP_PEM,
                "html_url": "https://github.com/apps/boba-agent-x", "owner": {"login": "Flotapponnier"}}))
        }))
        .route("/app/installations", get(|S(m): S<St>, h: HeaderMap| async move {
            let auth = h["authorization"].to_str().unwrap().trim_start_matches("Bearer ").to_string();
            m.lock().unwrap().jwt_seen = Some(auth);
            axum::Json(json!([{"id": 7, "account": {"login": "Flotapponnier"}}]))
        }))
        .route("/app/installations/{id}/access_tokens", post(|S(m): S<St>, P(id): P<i64>| async move {
            assert_eq!(id, 7);
            m.lock().unwrap().token_mints += 1;
            axum::Json(json!({"token": "ghs_installation_token", "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()}))
        }))
        .route("/installation/repositories", get(|h: HeaderMap| async move {
            assert_eq!(h["authorization"], "Bearer ghs_installation_token");
            axum::Json(json!({"total_count": 1, "repositories": [{"full_name": "Flotapponnier/boba-agent",
                "default_branch": "main", "private": false, "html_url": "https://github.com/Flotapponnier/boba-agent"}]}))
        }))
        .with_state(m);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn github_app_manifest_flow_tokens_and_git_credentials() {
    let mock = Arc::new(Mutex::new(MockApp::default()));
    let gh = mock_github_app(mock.clone()).await;
    let runner = FakeRunner::new();
    let r: Arc<dyn Runner> = Arc::new(runner.clone());
    let mut cfg = Config::for_tests(tmp_dir());
    cfg.github_api = gh.clone();
    cfg.github_web = gh.clone();
    cfg.public_url = "https://boba.example".into();
    let srv = start_cfg(cfg, Runners::from(r), runner).await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;

    let (_, st) = srv.get(&owner, "/github/app").await;
    assert_eq!(st["configured"], false);

    // 1. The setup page posts a manifest to GitHub.
    assert_eq!(
        srv.get(&member, "/github/app/setup").await.0,
        StatusCode::FORBIDDEN
    );
    let page = owner
        .get(format!("{}/github/app/setup", srv.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        page.contains(&format!("{gh}/settings/apps/new?state=")),
        "{page}"
    );
    assert!(page.contains("https://boba.example/api/github/webhook"));
    assert!(page.contains("&quot;pull_requests&quot;:&quot;write&quot;"));
    let state = page
        .split("state=")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();

    // 2. GitHub sends the browser back: bad state refused, good one stored + redirect to install.
    let nofollow = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bad = nofollow
        .get(format!(
            "{}/github/app/callback?code=the-code&state=nope",
            srv.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let ok = nofollow
        .get(format!(
            "{}/github/app/callback?code=the-code&state={state}",
            srv.base
        ))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_redirection(), "{}", ok.status());
    assert_eq!(
        ok.headers()["location"],
        format!("{gh}/apps/boba-agent-x/installations/new").as_str()
    );
    // A state works once.
    let again = nofollow
        .get(format!(
            "{}/github/app/callback?code=the-code&state={state}",
            srv.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    // Secrets are stored encrypted.
    let raw = srv.state.db.setting("github_app").await.unwrap().unwrap();
    assert!(
        !raw.contains("BEGIN") && !raw.contains("whsec") && !raw.contains("csecret"),
        "{raw}"
    );

    // 3. Status lists the installation (JWT RS256 -> installation token -> repos).
    let (_, st) = srv.get(&member, "/github/app").await;
    assert_eq!(
        (
            st["configured"].clone(),
            st["slug"].clone(),
            st["app_id"].clone()
        ),
        (json!(true), json!("boba-agent-x"), json!(42))
    );
    assert_eq!(
        st["installations"][0]["repos"],
        json!(["Flotapponnier/boba-agent"])
    );
    let jwt = mock.lock().unwrap().jwt_seen.clone().unwrap();
    let header: Value = serde_json::from_slice(
        &jwt.split('.')
            .next()
            .map(|h| {
                use std::io::Read;
                let mut b = Vec::new();
                let padded = format!("{h}{}", "=".repeat((4 - h.len() % 4) % 4))
                    .replace('-', "+")
                    .replace('_', "/");
                let _ = base64_decode(&padded).as_slice().read_to_end(&mut b);
                b
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(header["alg"], "RS256");

    // 4. Repo picker; once added, it is marked.
    assert_eq!(
        srv.get(&member, "/github/repos").await.0,
        StatusCode::FORBIDDEN
    );
    let (_, list) = srv.get(&owner, "/github/repos").await;
    assert_eq!(
        (list[0]["full_name"].clone(), list[0]["repo_id"].clone()),
        (json!("Flotapponnier/boba-agent"), Value::Null)
    );
    let (_, repo) = srv
        .post(&owner, "/repos", json!({"name": "boba-agent", "path": "/srv/boba-agent", "remote_url": "https://github.com/Flotapponnier/boba-agent"}))
        .await;
    let (_, list) = srv.get(&owner, "/github/repos").await;
    assert_eq!(list[0]["repo_id"], repo["id"]);

    // 5. Runs on that repo get a short-lived push token + the bot author; never in Debug output.
    let snap = srv.snapshot(&owner).await;
    srv.req(
        &owner,
        "PATCH",
        &format!("/tabs/{}", snap.tabs[0].id),
        Some(json!({"repo_id": repo["id"]})),
    )
    .await;
    let t = snap
        .tickets
        .iter()
        .find(|t| t.tab_id == snap.tabs[0].id)
        .unwrap()
        .id;
    srv.post(
        &owner,
        &format!("/tickets/{t}/assign"),
        json!({"agent_id": agent(&snap, "Boba Dev").id}),
    )
    .await;
    srv.until(&owner, "ran", |s| {
        s.runs.iter().any(|r| r.ticket_id == Some(t))
    })
    .await;
    let spec = srv
        .runner
        .specs()
        .into_iter()
        .find(|s| s.ticket.as_ref().is_some_and(|x| x.id == t))
        .unwrap();
    let repo_spec = spec.repo.clone().unwrap();
    assert_eq!(
        repo_spec.git_token.as_ref().map(|t| t.expose()),
        Some("ghs_installation_token")
    );
    assert_eq!(
        repo_spec.git_author.unwrap().email,
        "42+boba-agent-x[bot]@users.noreply.github.com"
    );
    assert!(
        !format!("{spec:?}").contains("ghs_installation_token"),
        "token leaked into Debug"
    );
    assert_eq!(
        mock.lock().unwrap().token_mints,
        1,
        "installation tokens are cached"
    );

    // 6. Webhooks are checked with the app's secret.
    let body = br#"{"action": "created", "installation": {"id": 7}}"#;
    let res = reqwest::Client::new()
        .post(format!("{}/github/webhook", srv.base))
        .header("x-github-event", "installation")
        .header(
            "x-hub-signature-256",
            boba_github::webhook::sign(b"whsec", body),
        )
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| *c != b'=') {
        let Some(v) = T.iter().position(|t| *t == c) else {
            continue;
        };
        buf = buf << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

#[tokio::test]
async fn remote_runs_survive_a_server_restart() {
    let dir = tmp_dir();
    let srv = start_in(dir.clone(), FakeRunner::new()).await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let (_, repo) = srv
        .post(
            &owner,
            "/repos",
            json!({"name": "app", "path": "/srv/app", "remote_url": "https://github.com/acme/app"}),
        )
        .await;
    srv.req(
        &owner,
        "PATCH",
        &format!("/tabs/{}", snap.tabs[0].id),
        Some(json!({"repo_id": repo["id"]})),
    )
    .await;
    let (_, v) = srv
        .post(
            &member,
            "/runners",
            json!({"name": "mbp", "scope": "personal"}),
        )
        .await;
    let token = v["token"].as_str().unwrap().to_string();
    let mut ws = runner_connect(&srv, &token).await.unwrap();
    runner_send(&mut ws, hello("subscription")).await;
    let rid = v["runner"]["id"].clone();
    srv.until(&owner, "online", |s| {
        s.runners.iter().any(|r| json!(r.id) == rid && r.online)
    })
    .await;
    let t = snap.tickets[0].id;
    let (_, run) = srv
        .post(
            &member,
            &format!("/tickets/{t}/assign"),
            json!({"agent_id": agent(&snap, "Golden QA").id}),
        )
        .await;
    let start = runner_recv(&mut ws).await.unwrap();
    let run_id = start["spec"]["run_id"].clone();
    assert_eq!(run_id, run["id"]);
    runner_send(
        &mut ws,
        json!({"type": "update", "run_id": run_id, "update":
        {"type": "started", "worktree_path": "/w", "branch": "b", "session_id": "s"}}),
    )
    .await;
    srv.until(&member, "running", |s| {
        s.runs
            .iter()
            .any(|r| json!(r.id) == run_id && r.status == RunStatus::Running)
    })
    .await;
    drop(ws);

    // Restart: a second server on the same data dir. The run is not failed at boot...
    let srv2 = start_in(dir, FakeRunner::new()).await;
    let c2 = client();
    srv2.post(
        &c2,
        "/auth/login",
        json!({"email": "m@test.io", "password": "password123"}),
    )
    .await;
    let s2 = srv2.snapshot(&c2).await;
    let r = s2.runs.iter().find(|r| json!(r.id) == run_id).unwrap();
    assert_eq!(
        r.status,
        RunStatus::Running,
        "remote run failed at boot: {:?}",
        r.error
    );
    assert_eq!(agent(&s2, "Golden QA").status, AgentStatus::Working);
    // ...the runner reconnects, resumes it and finishes it.
    let mut ws = runner_connect(&srv2, &token).await.unwrap();
    runner_send(&mut ws, hello("subscription")).await;
    runner_send(&mut ws, json!({"type": "resume", "run_ids": [run_id]})).await;
    runner_send(&mut ws, json!({"type": "update", "run_id": run_id, "update": {"type": "finished", "outcome":
        {"status": "succeeded", "summary": "done after restart", "verdict": null, "feedback": null, "error": null, "plan": null}}})).await;
    // (The snapshot only shows a ticket's latest run: the review that follows. Ask the ticket.)
    let mut done = None;
    for _ in 0..200 {
        let (_, runs) = srv2.get(&c2, &format!("/tickets/{t}/runs")).await;
        if let Some(r) = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run_id && r["status"] == "succeeded")
        {
            done = Some(r.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        done.expect("work run finished")["summary"],
        "done after restart"
    );
}

#[tokio::test]
async fn remote_capacity_is_the_smaller_of_registered_and_hello() {
    let srv = start().await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    let snap = srv.snapshot(&owner).await;
    let (_, repo) = srv
        .post(
            &owner,
            "/repos",
            json!({"name": "app", "path": "/srv/app", "remote_url": "https://github.com/acme/app"}),
        )
        .await;
    srv.req(
        &owner,
        "PATCH",
        &format!("/tabs/{}", snap.tabs[0].id),
        Some(json!({"repo_id": repo["id"]})),
    )
    .await;
    let (_, v) = srv
        .post(
            &member,
            "/runners",
            json!({"name": "mbp", "scope": "personal", "max_concurrent_runs": 2}),
        )
        .await;
    let rid = v["runner"]["id"].clone();
    let mut ws = runner_connect(&srv, v["token"].as_str().unwrap())
        .await
        .unwrap();
    runner_send(&mut ws, hello("subscription")).await; // the machine says 2… see hello(): 2
    let mut h = hello("subscription");
    h["max_concurrent_runs"] = json!(4);
    let mut ws2 = runner_connect(&srv, v["token"].as_str().unwrap())
        .await
        .unwrap();
    runner_send(&mut ws2, h).await; // a newer connection claiming 4
    drop(ws);
    let snap = srv
        .until(&owner, "online", |s| {
            s.runners.iter().any(|r| json!(r.id) == rid && r.online)
        })
        .await;
    assert_eq!(
        snap.runners
            .iter()
            .find(|r| json!(r.id) == rid)
            .unwrap()
            .max_concurrent_runs,
        2
    );

    // Three runs: only two are handed over; the third waits for a free slot.
    let tickets: Vec<Id> = snap
        .tickets
        .iter()
        .filter(|t| t.tab_id == snap.tabs[0].id)
        .map(|t| t.id)
        .take(3)
        .collect();
    let agents = ["Boba Dev", "Golden QA", "Diamond Architect"];
    for (t, a) in tickets.iter().zip(agents) {
        let (s, v) = srv
            .post(
                &member,
                &format!("/tickets/{t}/assign"),
                json!({"agent_id": agent(&snap, a).id}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let first = runner_recv(&mut ws2).await.unwrap();
    let _second = runner_recv(&mut ws2).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), ws2.next())
            .await
            .is_err(),
        "a third Start arrived beyond the capacity"
    );
    runner_send(&mut ws2, json!({"type": "update", "run_id": first["spec"]["run_id"], "update": {"type": "finished", "outcome":
        {"status": "succeeded", "summary": "ok", "verdict": null, "feedback": null, "error": null, "plan": null}}})).await;
    let third = runner_recv(&mut ws2).await.unwrap();
    assert_eq!(third["type"], "start");
}

#[tokio::test]
async fn github_app_manifest_as_json_for_the_spa() {
    let mut cfg = Config::for_tests(tmp_dir());
    cfg.public_url = "http://localhost:7777".into();
    let runner = FakeRunner::new();
    let r: Arc<dyn Runner> = Arc::new(runner.clone());
    let srv = start_cfg(cfg, Runners::from(r), runner).await;
    let owner = srv.owner().await;
    let member = srv.invite(&owner, "m@test.io", "member").await;
    assert_eq!(
        srv.get(&member, "/github/app/manifest").await.0,
        StatusCode::FORBIDDEN
    );
    let (s, v) = srv
        .get(
            &owner,
            "/github/app/manifest?org=acme&name=Boba%20Agent%20Acme",
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["action_url"]
        .as_str()
        .unwrap()
        .starts_with("https://github.com/organizations/acme/settings/apps/new?state="));
    assert_eq!(v["manifest"]["name"], "Boba Agent Acme");
    assert_eq!(
        v["manifest"]["hook_attributes"]["url"],
        "http://localhost:7777/api/github/webhook"
    );
    assert_eq!(v["manifest"]["default_permissions"]["contents"], "write");
    assert!(v["warnings"][0].as_str().unwrap().contains("local address"));
}

#[tokio::test]
async fn concierge_uses_the_askers_time_zone() {
    let srv = start().await;
    let owner = srv.owner().await;
    let snap = srv.snapshot(&owner).await;
    let pinky = agent(&snap, "Pinky SEO").id;
    let ask = |tz: &str| {
        json!({"text": "every monday at 9 audit the docs", "tab_id": null, "ticket_id": null,
                                "agent_id": null, "voice": false, "timezone": tz})
    };
    let (_, good) = srv.post(&owner, "/command", ask("Europe/Paris")).await;
    let (_, bad) = srv.post(&owner, "/command", ask("Mars/Olympus")).await;
    srv.until(&owner, "commands done", all_idle).await;
    let specs = srv.runner.specs();
    let tz_of = |id: &Value| {
        specs
            .iter()
            .find(|s| json!(s.run_id) == *id)
            .unwrap()
            .command
            .as_ref()
            .unwrap()
            .timezone
            .clone()
    };
    assert_eq!(tz_of(&good["id"]).as_deref(), Some("Europe/Paris"));
    assert_eq!(tz_of(&bad["id"]), None, "unknown zones are dropped");

    // A cron quest in the plan without a zone gets the asker's.
    let (s, results) = srv
        .post(&owner, "/command/execute", json!({"run_id": good["id"], "actions": [{"action": "create_job", "job": {
            "name": "Monday audit", "description": null, "icon": null, "agent_id": pinky, "prompt": "Audit the docs",
            "schedule": {"type": "cron", "expr": "0 9 * * 1", "timezone": ""}, "output": "report",
            "enabled": null, "repo_id": null, "tab_id": null, "skip_if_running": null}}]}))
        .await;
    assert_eq!(s, StatusCode::OK, "{results}");
    assert_eq!(results[0]["ok"], true, "{results}");
    assert_eq!(
        results[0]["entity"]["data"]["schedule"]["timezone"],
        "Europe/Paris"
    );
}

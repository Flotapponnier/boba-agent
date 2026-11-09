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
        let (verdict, hang) = {
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
            (v, s.hang)
        };
        self.live.lock().unwrap().push((spec.run_id, tx.clone()));
        tokio::spawn(async move {
            let id = spec.run_id;
            let session = spec
                .resume_session_id
                .clone()
                .unwrap_or_else(|| format!("sess-{id}"));
            let t = spec.ticket.clone().expect("ticket runs only");
            let branch = t
                .branch
                .clone()
                .unwrap_or_else(|| format!("boba/T-{}", t.number));
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
            if hang && spec.kind == RunKind::Work {
                return; // tx kept alive in `live`
            }
            if spec.kind == RunKind::Work && spec.ticket.as_ref().unwrap().pr_number.is_none() {
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
                        plan: None,
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
    _state: SharedState,
    dir: PathBuf,
}

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("boba-test-{}", Id::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_in(dir: PathBuf, runner: FakeRunner) -> Srv {
    let cfg = Config::for_tests(dir.clone());
    let (state, app) = build(cfg, Arc::new(runner.clone())).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Srv {
        base: format!("http://{addr}/api"),
        runner,
        _state: state,
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
        for _ in 0..200 {
            let s = self.snapshot(c).await;
            if f(&s) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
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
    assert_eq!(snap.agents.len(), 6);
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
            &format!("/tickets/{tid}/comment"),
            json!({"text": "Nice"}),
        )
        .await;
    assert_eq!((s, v["kind"].clone()), (StatusCode::OK, json!("comment")));
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

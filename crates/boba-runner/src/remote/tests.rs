//! Client tests against an in-process WebSocket server playing the Boba Agent side.

#![allow(clippy::result_large_err)] // tungstenite's handshake callback signature

use std::time::Duration;

use boba_core::runner::{AgentSpec, RepoSpec, RunSpec, RunUpdate, RunnerWire, TicketSpec};
use boba_core::{PermissionMode, RunKind, RunStatus};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use uuid::Uuid;

use super::*;
use crate::{DemoConfig, DemoRunner};

type Ws = WebSocketStream<TcpStream>;

/// Accepts connections; each accepted socket (with its Authorization header) is handed
/// to the test. `reject` answers the upgrade with HTTP 401 instead.
async fn server(reject: bool) -> (String, mpsc::UnboundedReceiver<(Ws, Option<String>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/api/runners/ws", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut auth = None;
                let cb = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                    auth = req
                        .headers()
                        .get("authorization")
                        .map(|v| v.to_str().unwrap_or_default().to_string());
                    assert_eq!(req.uri().path(), "/api/runners/ws");
                    if reject {
                        let mut r = ErrorResponse::new(Some("bad token".into()));
                        *r.status_mut() =
                            tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                        return Err(r);
                    }
                    Ok(resp)
                };
                if let Ok(ws) = tokio_tungstenite::accept_hdr_async(sock, cb).await {
                    let _ = tx.send((ws, auth));
                }
            });
        }
    });
    (url, rx)
}

async fn recv(ws: &mut Ws) -> RunnerWire {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .expect("client went silent")
            .expect("client closed")
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

async fn send(ws: &mut Ws, msg: &RunnerWire) {
    ws.send(Message::text(serde_json::to_string(msg).unwrap()))
        .await
        .unwrap();
}

fn spec() -> RunSpec {
    RunSpec {
        run_id: Uuid::now_v7(),
        kind: RunKind::Work,
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
            number: 3,
            title: "Add a thing".into(),
            body: String::new(),
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
        repo: None,
        mcp_servers: vec![],
        board_tools: None,
        skill_paths: vec![],
        skill_names: vec![],
        resume_session_id: None,
        extra_instructions: None,
    }
}

fn client(url: &str, speed: f64, data: &Path) -> Arc<Client> {
    let mut cfg = DemoConfig::new(Uuid::now_v7(), data);
    cfg.speed = speed;
    let runner: Arc<dyn Runner> = Arc::new(DemoRunner::new(cfg));
    Client::new(
        ClientConfig {
            url: url.into(),
            token: "tok-123".into(),
            hello: Hello {
                name: "laptop".into(),
                max_concurrent_runs: 4,
                version: "0.1.0".into(),
                auth: RunnerAuth::Demo,
                host: "laptop.local".into(),
                claude_version: None,
            },
            data_dir: data.to_path_buf(),
            backoff: Backoff {
                min: Duration::from_millis(50),
                max: Duration::from_millis(200),
                stable_after: Duration::from_secs(30),
                ping_every: Duration::from_secs(30),
            },
        },
        runner,
    )
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("boba-remote-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Hello, Resume, Skills, in that order. Returns the resumed run ids.
async fn handshake(ws: &mut Ws) -> Vec<Id> {
    match recv(ws).await {
        RunnerWire::Hello {
            name,
            max_concurrent_runs,
            auth,
            host,
            ..
        } => {
            assert_eq!(
                (name.as_str(), max_concurrent_runs, auth, host.as_str()),
                ("laptop", 4, RunnerAuth::Demo, "laptop.local")
            );
        }
        other => panic!("expected Hello, got {other:?}"),
    }
    let ids = match recv(ws).await {
        RunnerWire::Resume { run_ids } => run_ids,
        other => panic!("expected Resume, got {other:?}"),
    };
    assert!(matches!(recv(ws).await, RunnerWire::Skills { .. }));
    ids
}

#[tokio::test]
async fn runs_a_job_end_to_end() {
    let (url, mut conns) = server(false).await;
    let data = tmp();
    let c = client(&url, 0.0, &data);
    let (_stop_tx, stop) = watch::channel(false);
    tokio::spawn({
        let c = c.clone();
        async move { c.run(stop).await }
    });
    let (mut ws, auth) = conns.recv().await.unwrap();
    assert_eq!(auth.as_deref(), Some("Bearer tok-123"));
    assert!(handshake(&mut ws).await.is_empty());
    let s = spec();
    let id = s.run_id;
    send(
        &mut ws,
        &RunnerWire::Start {
            spec: Box::new(s.clone()),
        },
    )
    .await;
    send(&mut ws, &RunnerWire::Start { spec: Box::new(s) }).await; // duplicate is ignored
    let mut kinds = Vec::new();
    loop {
        match recv(&mut ws).await {
            RunnerWire::Update { run_id, update } => {
                assert_eq!(run_id, id);
                let fin = matches!(update, RunUpdate::Finished { .. });
                kinds.push(update);
                if fin {
                    break;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(matches!(kinds[0], RunUpdate::Started { .. }));
    assert!(
        matches!(kinds.last(), Some(RunUpdate::Finished { outcome }) if outcome.status == RunStatus::Succeeded)
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|u| matches!(u, RunUpdate::Started { .. }))
            .count(),
        1
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(c.active_runs().is_empty());
}

#[tokio::test]
async fn reconnects_resumes_and_flushes_without_losing_finished() {
    let (url, mut conns) = server(false).await;
    let data = tmp();
    let c = client(&url, 8.0, &data);
    let (_stop_tx, stop) = watch::channel(false);
    tokio::spawn({
        let c = c.clone();
        async move { c.run(stop).await }
    });
    let (mut ws, _) = conns.recv().await.unwrap();
    handshake(&mut ws).await;
    let s = spec();
    let id = s.run_id;
    send(&mut ws, &RunnerWire::Start { spec: Box::new(s) }).await;
    let mut updates = Vec::new();
    while updates.len() < 3 {
        if let RunnerWire::Update { update, .. } = recv(&mut ws).await {
            updates.push(update);
        }
    }
    drop(ws); // connection lost mid-run

    let (mut ws, _) = conns.recv().await.unwrap();
    assert_eq!(
        handshake(&mut ws).await,
        vec![id],
        "the live run is resumed"
    );
    loop {
        if let RunnerWire::Update { run_id, update } = recv(&mut ws).await {
            assert_eq!(run_id, id);
            let fin = matches!(update, RunUpdate::Finished { .. });
            updates.push(update);
            if fin {
                break;
            }
        }
    }
    assert_eq!(
        updates
            .iter()
            .filter(|u| matches!(u, RunUpdate::Started { .. }))
            .count(),
        1
    );
    assert_eq!(
        updates
            .iter()
            .filter(|u| matches!(u, RunUpdate::Finished { .. }))
            .count(),
        1
    );
    assert!(
        matches!(updates.last(), Some(RunUpdate::Finished { outcome }) if outcome.status == RunStatus::Succeeded)
    );
}

#[tokio::test]
async fn server_cancel_and_stop_cancel_runs() {
    let (url, mut conns) = server(false).await;
    let data = tmp();
    let c = client(&url, 1.0, &data);
    let (stop_tx, stop) = watch::channel(false);
    let task = tokio::spawn({
        let c = c.clone();
        async move { c.run(stop).await }
    });
    let (mut ws, _) = conns.recv().await.unwrap();
    handshake(&mut ws).await;
    let (a, b) = (spec(), spec());
    let (ida, idb) = (a.run_id, b.run_id);
    send(&mut ws, &RunnerWire::Start { spec: Box::new(a) }).await;
    send(&mut ws, &RunnerWire::Start { spec: Box::new(b) }).await;
    send(&mut ws, &RunnerWire::Cancel { run_id: ida }).await;
    let mut cancelled = HashSet::new();
    let mut stopped = false;
    while cancelled.len() < 2 {
        if let RunnerWire::Update {
            run_id,
            update: RunUpdate::Finished { outcome },
        } = recv(&mut ws).await
        {
            assert_eq!(outcome.status, RunStatus::Cancelled);
            cancelled.insert(run_id);
            if !stopped {
                // Ctrl-C: the remaining run is cancelled and its Finished still flushed.
                stop_tx.send(true).unwrap();
                stopped = true;
            }
        }
    }
    assert_eq!(cancelled, HashSet::from([ida, idb]));
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn fatal_refusals_stop_the_client() {
    // Policy close (shared runner on a subscription).
    let (url, mut conns) = server(false).await;
    let data = tmp();
    let c = client(&url, 0.0, &data);
    let (_s, stop) = watch::channel(false);
    let task = tokio::spawn({
        let c = c.clone();
        async move { c.run(stop).await }
    });
    let (mut ws, _) = conns.recv().await.unwrap();
    handshake(&mut ws).await;
    ws.close(Some(CloseFrame {
        code: CloseCode::from(4003),
        reason: "shared runners need an API key".into(),
    }))
    .await
    .unwrap();
    let err = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        err.to_string().contains("shared runners need an API key"),
        "{err}"
    );

    // Bad token.
    let (url, _conns) = server(true).await;
    let c = client(&url, 0.0, &data);
    let (_s, stop) = watch::channel(false);
    let err = tokio::time::timeout(Duration::from_secs(5), c.run(stop))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("token rejected"), "{err}");
}

#[tokio::test]
async fn runs_need_a_remote_url_and_repos_are_cloned() {
    let (url, mut conns) = server(false).await;
    let data = tmp();
    let c = client(&url, 0.0, &data);
    let (_s, stop) = watch::channel(false);
    tokio::spawn({
        let c = c.clone();
        async move { c.run(stop).await }
    });
    let (mut ws, _) = conns.recv().await.unwrap();
    handshake(&mut ws).await;
    let mut s = spec();
    s.repo = Some(RepoSpec {
        path: "/srv/on-the-server".into(),
        default_branch: "main".into(),
        remote_url: None,
        ai_attribution: true,
        git_token: None,
        git_author: None,
    });
    send(&mut ws, &RunnerWire::Start { spec: Box::new(s) }).await;
    match recv(&mut ws).await {
        RunnerWire::Update {
            update: RunUpdate::Finished { outcome },
            ..
        } => {
            assert_eq!(outcome.status, RunStatus::Failed);
            assert!(outcome.error.unwrap().contains("remote URL"));
        }
        other => panic!("{other:?}"),
    }

    // RepoCache: clone once, fetch after, refuse a different remote in the same slot.
    let root = tmp();
    let origin = crate::git::tests::fixture_repo(&root)
        .await
        .parent()
        .unwrap()
        .join("origin.git");
    let url = origin.to_string_lossy().into_owned();
    let cache = RepoCache::new(root.join("repos"), Tools::default());
    let dir = cache.prepare(&url).await.unwrap();
    assert!(dir.join("README.md").exists());
    assert_eq!(cache.prepare(&url).await.unwrap(), dir);
    // Same slug (same last two path segments), different URL.
    let twin_parent = root.join("sub").join(root.file_name().unwrap());
    std::fs::create_dir_all(&twin_parent).unwrap();
    let other = twin_parent.join("origin.git");
    crate::git::run(
        Path::new("git"),
        &["init", "-q", "--bare", &other.to_string_lossy()],
        &root,
    )
    .await
    .unwrap();
    assert_eq!(
        RepoCache::slug(&other.to_string_lossy()),
        RepoCache::slug(&url)
    );
    assert!(cache
        .prepare(&other.to_string_lossy())
        .await
        .unwrap_err()
        .to_string()
        .contains("is a clone of"));
}

#[test]
fn urls_args_and_slugs() {
    assert_eq!(
        ws_url("https://boba.example.com").unwrap(),
        "wss://boba.example.com/api/runners/ws"
    );
    assert_eq!(
        ws_url("http://127.0.0.1:7777/").unwrap(),
        "ws://127.0.0.1:7777/api/runners/ws"
    );
    assert_eq!(
        ws_url("https://x.io/boba").unwrap(),
        "wss://x.io/boba/api/runners/ws"
    );
    assert_eq!(
        ws_url("wss://x.io/api/runners/ws").unwrap(),
        "wss://x.io/api/runners/ws"
    );
    assert!(ws_url("boba.example.com").is_err());
    assert!(ws_url("ftp://x").is_err());

    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = ClientArgs::parse(&a(&[
        "--server",
        "https://b.io",
        "--token=t",
        "--max-runs",
        "3",
        "--demo",
        "--name",
        "box",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            p.server.as_str(),
            p.token.as_str(),
            p.max_runs,
            p.demo,
            p.name.as_deref()
        ),
        ("https://b.io", "t", Some(3), true, Some("box"))
    );
    assert!(ClientArgs::parse(&a(&["--help"])).unwrap().is_none());
    assert!(ClientArgs::parse(&a(&[
        "--server",
        "https://b.io",
        "--token",
        "t",
        "--max-runs",
        "0"
    ]))
    .is_err());
    assert!(
        ClientArgs::parse(&a(&["--server", "https://b.io", "--token", "t", "--bogus"]))
            .unwrap_err()
            .to_string()
            .contains("unknown option")
    );

    assert_eq!(RepoCache::slug("git@github.com:acme/app.git"), "acme-app");
    assert_eq!(RepoCache::slug("https://github.com/acme/app"), "acme-app");
    assert_eq!(
        RepoCache::slug("https://gitlab.com/group/sub/proj.git"),
        "sub-proj"
    );
    assert_eq!(RepoCache::slug("/srv/git/origin.git"), "git-origin");
}

//! Client tests against a tiny in-process HTTP server replaying recorded GitHub responses
//! (`tests/fixtures/*.json`, recorded with `gh api` from public repos and trimmed).

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const ISSUES: &str = include_str!("../tests/fixtures/issues_open.json");
const PULLS: &str = include_str!("../tests/fixtures/pulls_open_forks.json");
const PULL_9: &str = include_str!("../tests/fixtures/pull_9.json");

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

struct Route {
    method: &'static str,
    path: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn route(method: &'static str, path: &str, status: u16, body: &str) -> Route {
    Route {
        method,
        path: path.into(),
        status,
        headers: vec![],
        body: body.into(),
    }
}

struct Mock {
    base: String,
    reqs: Arc<Mutex<Vec<Req>>>,
}

impl Mock {
    fn requests(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }
}

/// Serves `routes` (first match on method + exact path-and-query; `{base}` in headers is
/// replaced by the server URL). Unmatched requests get 404.
async fn mock(routes: Vec<Route>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let reqs = Arc::new(Mutex::new(Vec::new()));
    let (r2, b2) = (reqs.clone(), base.clone());
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (reqs, base, routes) = (r2.clone(), b2.clone(), routes.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or_default().split_whitespace();
                let (method, path) = (
                    first.next().unwrap_or_default().to_string(),
                    first.next().unwrap_or_default().to_string(),
                );
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let len: usize = headers
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < header_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
                reqs.lock().unwrap().push(Req {
                    method: method.clone(),
                    path: path.clone(),
                    headers,
                    body,
                });
                let r = routes.iter().find(|r| r.method == method && r.path == path);
                let (status, extra, body) = match r {
                    Some(r) => (r.status, r.headers.clone(), r.body.clone()),
                    None => (404, vec![], r#"{"message":"Not Found"}"#.to_string()),
                };
                let mut resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                    body.len()
                );
                for (k, v) in extra {
                    resp.push_str(&format!("{k}: {}\r\n", v.replace("{base}", &base)));
                }
                resp.push_str("\r\n");
                resp.push_str(&body);
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { base, reqs }
}

fn repo() -> RepoRef {
    RepoRef::new("acme", "app")
}

#[tokio::test]
async fn lists_issues_without_prs_and_filters_labels() {
    let m = mock(vec![route(
        "GET",
        "/repos/acme/app/issues?state=open&sort=created&direction=asc&per_page=100",
        200,
        ISSUES,
    )])
    .await;
    let gh = GitHub::with_api("tok", &m.base).unwrap();
    let all = gh.list_open_issues(&repo(), &[]).await.unwrap();
    assert_eq!(
        all.iter().map(|i| i.number).collect::<Vec<_>>(),
        [8772],
        "PR items are dropped"
    );
    let i = &all[0];
    assert!(i
        .url
        .starts_with("https://github.com/tokio-rs/tokio/issues/"));
    assert!(!i.author.is_empty() && i.labels.contains(&"C-bug".to_string()));
    assert_eq!(
        gh.list_open_issues(&repo(), &["i-hang".into()])
            .await
            .unwrap()
            .len(),
        1,
        "case-insensitive OR filter"
    );
    assert!(gh
        .list_open_issues(&repo(), &["nope".into()])
        .await
        .unwrap()
        .is_empty());

    let req = &m.requests()[0];
    assert!(req.path.starts_with("/repos/acme/app/issues?"));
    let h = |k: &str| {
        req.headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(h("authorization"), Some("Bearer tok"));
    assert_eq!(h("accept"), Some("application/vnd.github+json"));
    assert_eq!(h("x-github-api-version"), Some("2022-11-28"));
    assert!(h("user-agent").unwrap().starts_with("boba-agent/"));
}

#[tokio::test]
async fn lists_pulls_with_forks_drafts_and_pagination() {
    let pulls: Vec<Value> = serde_json::from_str(PULLS).unwrap();
    let page1 = serde_json::to_string(&pulls[..2]).unwrap();
    let page2 = serde_json::to_string(&pulls[2..]).unwrap();
    let mut first = route(
        "GET",
        "/repos/acme/app/pulls?state=open&sort=created&direction=asc&per_page=100",
        200,
        &page1,
    );
    first.headers.push((
        "link".into(),
        "<{base}/repos/acme/app/pulls?page=2>; rel=\"next\", <{base}/repos/acme/app/pulls?page=2>; rel=\"last\"".into(),
    ));
    let m = mock(vec![
        first,
        route("GET", "/repos/acme/app/pulls?page=2", 200, &page2),
    ])
    .await;
    let gh = GitHub::with_api("tok", &m.base).unwrap();

    let all = gh.list_open_pulls(&repo(), false).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(m.requests().len(), 2, "followed the next link");
    let draft = all
        .iter()
        .find(|p| p.draft)
        .expect("a draft in the fixture");
    let ready = gh.list_open_pulls(&repo(), true).await.unwrap();
    assert_eq!(ready.len(), 2);
    assert!(ready.iter().all(|p| p.number != draft.number));
    let p = &all[0];
    assert!(p.is_fork, "fixture PRs come from forks");
    assert_ne!(p.head_repo.as_deref(), Some("tokio-rs/tokio"));
    assert_eq!(p.base_branch, "master");
    assert!(!p.head_branch.is_empty() && p.head_sha.len() == 40);
}

#[tokio::test]
async fn get_pull_comment_review_close() {
    let m = mock(vec![
        route("GET", "/repos/acme/app/pulls/9", 200, PULL_9),
        route("POST", "/repos/acme/app/issues/9/comments", 201, r#"{"id": 11, "html_url": "https://github.com/acme/app/pull/9#issuecomment-11"}"#),
        route("POST", "/repos/acme/app/pulls/9/reviews", 200, r#"{"id": 22, "html_url": "https://github.com/acme/app/pull/9#pullrequestreview-22", "state": "APPROVED"}"#),
        route("PATCH", "/repos/acme/app/issues/3", 200, r#"{"number": 3, "state": "closed"}"#),
    ])
    .await;
    let gh = GitHub::with_api("tok", &m.base).unwrap();
    let pr = gh.get_pull(&repo(), 9).await.unwrap();
    assert_eq!(pr.head_branch, "runner/claude");
    assert!(!pr.is_fork, "same-repo branch");
    assert_eq!(pr.head_repo.as_deref(), Some("Flotapponnier/boba-agent"));

    let c = gh.comment(&repo(), 9, "Run finished").await.unwrap();
    assert_eq!(c.id, 11);
    let r = gh
        .submit_review(&repo(), 9, ReviewEvent::RequestChanges, "Fix x")
        .await
        .unwrap();
    assert_eq!(r.id, 22);
    gh.close_issue(&repo(), 3, CloseReason::Completed)
        .await
        .unwrap();

    let reqs = m.requests();
    let body = |i: usize| serde_json::from_str::<Value>(&reqs[i].body).unwrap();
    assert_eq!(body(1)["body"], "Run finished");
    assert_eq!(body(2)["event"], "REQUEST_CHANGES");
    assert_eq!(body(2)["body"], "Fix x");
    assert_eq!(reqs[3].method, "PATCH");
    assert_eq!(
        body(3),
        json!({ "state": "closed", "state_reason": "completed" })
    );
}

#[tokio::test]
async fn errors_are_typed() {
    let mut limited = route(
        "GET",
        "/repos/acme/app/pulls/1",
        403,
        r#"{"message":"API rate limit exceeded for user."}"#,
    );
    limited.headers = vec![
        ("x-ratelimit-remaining".into(), "0".into()),
        ("x-ratelimit-reset".into(), "1794255628".into()),
    ];
    let m = mock(vec![
        limited,
        route("GET", "/repos/acme/app/pulls/2", 401, r#"{"message":"Bad credentials"}"#),
        route("POST", "/repos/acme/app/pulls/3/reviews", 422, r#"{"message":"Unprocessable Entity","errors":["Can not approve your own pull request"]}"#),
        route("GET", "/repos/acme/app/pulls/5", 200, "not json"),
    ])
    .await;
    let gh = GitHub::with_api("tok", &m.base).unwrap();
    match gh.get_pull(&repo(), 1).await {
        Err(Error::RateLimited { reset_at }) => assert_eq!(reset_at.timestamp(), 1_794_255_628),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(gh.get_pull(&repo(), 2).await, Err(Error::Unauthorized(m)) if m == "Bad credentials")
    );
    assert!(matches!(
        gh.submit_review(&repo(), 3, ReviewEvent::Approve, "").await,
        Err(Error::Api { status: 422, .. })
    ));
    assert!(matches!(
        gh.get_pull(&repo(), 4).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        gh.get_pull(&repo(), 5).await,
        Err(Error::Decode(_))
    ));
    assert!(matches!(GitHub::new("  "), Err(Error::NoToken)));
    let dead = GitHub::with_api("tok", "http://127.0.0.1:9").unwrap();
    assert!(matches!(
        dead.get_pull(&repo(), 1).await,
        Err(Error::Network(_))
    ));
}

#[test]
fn repo_refs_and_links() {
    for s in [
        "acme/app",
        "https://github.com/acme/app",
        "https://github.com/acme/app.git",
        "git@github.com:acme/app.git",
        "ssh://git@github.com/acme/app",
        "https://github.com/acme/app/pull/3",
    ] {
        assert_eq!(RepoRef::parse(s), Some(repo()), "{s}");
    }
    for s in [
        "acme",
        "https://gitlab.com/acme/app",
        "/srv/repos/app",
        "acme/ap p",
    ] {
        assert_eq!(RepoRef::parse(s), None, "{s}");
    }
    assert_eq!(repo().to_string(), "acme/app");
    assert_eq!(
        next_link(r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=5>; rel="last""#).as_deref(),
        Some("https://api.github.com/x?page=2")
    );
    assert_eq!(
        next_link(r#"<https://api.github.com/x?page=1>; rel="prev""#),
        None
    );
}

#[test]
fn webhooks() {
    let secret = b"It's a Secret to Everybody";
    // Example from GitHub's docs ("Validating webhook deliveries").
    let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    assert!(verify_signature(secret, b"Hello, World!", sig));
    assert!(!verify_signature(secret, b"Hello, World?", sig));
    assert!(!verify_signature(b"other", b"Hello, World!", sig));
    assert!(!verify_signature(secret, b"Hello, World!", "sha1=abc"));
    assert!(!verify_signature(secret, b"Hello, World!", "sha256=zz"));
    assert_eq!(webhook::sign(secret, b"Hello, World!"), sig);

    let pull: Value = serde_json::from_str(PULL_9).unwrap();
    let body = json!({ "action": "opened", "repository": { "full_name": "Flotapponnier/boba-agent" }, "pull_request": pull }).to_string();
    match parse_event("pull_request", body.as_bytes()).unwrap() {
        WebhookEvent::PullRequest { repo, action, pull } => {
            assert_eq!(repo.full_name(), "Flotapponnier/boba-agent");
            assert_eq!(action, "opened");
            assert_eq!(pull.number, 9);
        }
        other => panic!("{other:?}"),
    }
    let issues: Vec<Value> = serde_json::from_str(ISSUES).unwrap();
    let real = issues
        .iter()
        .find(|i| i.get("pull_request").is_none())
        .unwrap();
    let as_pr = issues
        .iter()
        .find(|i| i.get("pull_request").is_some())
        .unwrap();
    let ev = |issue: &Value| {
        let b = json!({ "action": "labeled", "repository": { "full_name": "tokio-rs/tokio" }, "issue": issue }).to_string();
        parse_event("issues", b.as_bytes()).unwrap()
    };
    assert!(matches!(ev(real), WebhookEvent::Issue { action, .. } if action == "labeled"));
    assert!(
        matches!(ev(as_pr), WebhookEvent::Other { .. }),
        "issue events about PRs are not issues"
    );
    assert!(
        matches!(parse_event("ping", br#"{"zen":"Keep it logically awesome."}"#).unwrap(), WebhookEvent::Ping { zen } if zen.starts_with("Keep"))
    );
    assert!(
        matches!(parse_event("push", br#"{"repository":{"full_name":"a/b"}}"#).unwrap(), WebhookEvent::Other { event, repo: Some(_), .. } if event == "push")
    );
    assert!(parse_event("issues", b"nope").is_err());
}

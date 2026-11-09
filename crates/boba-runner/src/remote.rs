//! `boba-agent runner`: a remote runner that connects out to a Boba Agent server.
//!
//! The machine running this needs Claude Code (logged in, or an API key) and git
//! credentials for the repos it works on. It dials `wss://<server>/api/runners/ws` with
//! `Authorization: Bearer <runner token>` (outbound only, works behind NAT) and speaks
//! [`RunnerWire`]:
//!
//! * on every (re)connect: `Hello`, then `Resume { run_ids }` (runs still executing here),
//!   then `Skills`, then every update buffered while disconnected, in order;
//! * server → runner: `Start` / `Cancel` / `Message` / `Permission`, executed by a local
//!   [`ClaudeCodeRunner`] (or a [`DemoRunner`] with `--demo`);
//! * runner → server: `Update { run_id, update }` for every [`RunUpdate`], and `Skills`
//!   again when the discovered skills change.
//!
//! Repos: `RepoSpec.path` is a server path, so remote runs are mapped by `remote_url` to
//! `<data-dir>/repos/<owner>-<name>` (cloned once, fetched before each run). Skills are
//! resolved by `RunSpec.skill_names` against the skills discovered on this machine.
//!
//! Disconnects never lose a `Finished`: updates queue in an outbox that survives
//! reconnects; the connection is retried with exponential backoff (1 s → 60 s, jittered).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use boba_core::runner::{RunSpec, RunUpdate, RunnerWire};
use boba_core::team::RunnerAuth;
use boba_core::{Id, Skill};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::emit;
use crate::git::Tools;
use crate::util::Rng;
use crate::{ClaudeCodeRunner, ClaudeConfig, DemoConfig, DemoRunner, Runner};

pub const USAGE: &str = "\
Usage: boba-agent runner --server <url> --token <token> [options]

Connects this machine to a Boba Agent server as a runner (outbound WebSocket).

Options:
  --server <url>      Server base URL, e.g. https://boba.example.com   [env BOBA_SERVER]
  --token <token>     Runner token from the server's Runners page       [env BOBA_RUNNER_TOKEN]
  --name <name>       Name shown on the board (default: host name)     [env BOBA_RUNNER_NAME]
  --max-runs <n>      Concurrent runs on this machine (default 2)      [env BOBA_MAX_CONCURRENT_RUNS]
  --data-dir <dir>    Worktrees, repo clones, skills (default ./boba-runner-data) [env BOBA_DATA_DIR]
  --demo              Simulated runs, no Claude Code needed
  -h, --help          Show this help

Claude Code auth: with ANTHROPIC_API_KEY (or Bedrock / Vertex) set, the key is used;
otherwise this machine's `claude auth login` (personal use: the server only routes your own
work to it). Force one with BOBA_CLAUDE_AUTH=api_key|subscription.";

#[derive(Debug, Clone, PartialEq)]
pub struct ClientArgs {
    pub server: String,
    pub token: String,
    pub name: Option<String>,
    pub max_runs: Option<u32>,
    pub data_dir: PathBuf,
    pub demo: bool,
}

impl ClientArgs {
    /// Parse `args` (after the `runner` subcommand), falling back to env vars.
    /// `Ok(None)` = help was requested.
    pub fn parse(args: &[String]) -> anyhow::Result<Option<Self>> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let mut server = env("BOBA_SERVER");
        let mut token = env("BOBA_RUNNER_TOKEN");
        let mut name = env("BOBA_RUNNER_NAME");
        let mut max_runs = env("BOBA_MAX_CONCURRENT_RUNS").and_then(|v| v.parse().ok());
        let mut data_dir = env("BOBA_DATA_DIR").map(PathBuf::from);
        let mut demo = false;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let (flag, inline) = match a.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
                _ => (a.as_str(), None),
            };
            let mut value = || -> anyhow::Result<String> {
                inline
                    .clone()
                    .or_else(|| it.next().cloned())
                    .ok_or_else(|| anyhow!("{flag} needs a value\n\n{USAGE}"))
            };
            match flag {
                "-h" | "--help" => return Ok(None),
                "--server" => server = Some(value()?),
                "--token" => token = Some(value()?),
                "--name" => name = Some(value()?),
                "--max-runs" => {
                    let v = value()?;
                    max_runs = Some(v.parse().ok().filter(|n: &u32| *n > 0).ok_or_else(|| {
                        anyhow!("--max-runs expects a positive number, got {v:?}")
                    })?);
                }
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
                "--demo" => demo = true,
                other => bail!("unknown option {other:?}\n\n{USAGE}"),
            }
        }
        let server = server.ok_or_else(|| anyhow!("--server is required\n\n{USAGE}"))?;
        let token = token.ok_or_else(|| anyhow!("--token is required\n\n{USAGE}"))?;
        ws_url(&server)?;
        Ok(Some(Self {
            server,
            token,
            name,
            max_runs,
            data_dir: data_dir.unwrap_or_else(|| PathBuf::from("boba-runner-data")),
            demo,
        }))
    }
}

/// `https://host[/prefix]` -> `wss://host[/prefix]/api/runners/ws` (http -> ws).
pub fn ws_url(server: &str) -> anyhow::Result<String> {
    let s = server.trim().trim_end_matches('/');
    let (scheme, rest) = s
        .split_once("://")
        .ok_or_else(|| anyhow!("--server must start with https:// or http://, got {server:?}"))?;
    let ws = match scheme {
        "https" | "wss" => "wss",
        "http" | "ws" => "ws",
        other => bail!("unsupported scheme {other:?} in --server"),
    };
    if rest.is_empty() {
        bail!("--server has no host");
    }
    let rest = rest.trim_end_matches("/api/runners/ws");
    Ok(format!("{ws}://{rest}/api/runners/ws"))
}

/// Entry point of `boba-agent runner …` (tracing is initialised by the caller).
pub async fn main(args: Vec<String>) -> anyhow::Result<()> {
    let Some(args) = ClientArgs::parse(&args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    std::fs::create_dir_all(&args.data_dir)
        .with_context(|| format!("creating {}", args.data_dir.display()))?;
    let data_dir = std::fs::canonicalize(&args.data_dir)?;
    let id = persistent_id(&data_dir.join("runner-id"))?;
    let host = hostname();
    let name = args.name.clone().unwrap_or_else(|| host.clone());

    let (runner, auth, claude_version): (Arc<dyn Runner>, RunnerAuth, Option<String>) = if args.demo
    {
        let mut cfg = DemoConfig::from_env(id, data_dir.clone());
        cfg.name = name.clone();
        if let Some(n) = args.max_runs {
            cfg.max_concurrent_runs = n;
        }
        (Arc::new(DemoRunner::new(cfg)), RunnerAuth::Demo, None)
    } else {
        let mut cfg = ClaudeConfig::from_env(id, data_dir.clone());
        // A connected machine is usually someone's own laptop: without an API key (and no
        // explicit BOBA_CLAUDE_AUTH), use its `claude auth login`.
        if std::env::var_os("BOBA_CLAUDE_AUTH").is_none()
            && !crate::claude::api_key_auth_available()
        {
            cfg.auth_mode = crate::AuthMode::Subscription;
        }
        cfg.name = name.clone();
        cfg.max_concurrent_runs = args.max_runs.unwrap_or(2);
        let r = ClaudeCodeRunner::connect(cfg).await?;
        let version = r.claude_version().await.ok();
        let auth = r.auth();
        (Arc::new(r), auth, version)
    };
    tracing::info!(%name, ?auth, demo = args.demo, data = %data_dir.display(), "runner starting");

    let client = Client::new(
        ClientConfig {
            url: ws_url(&args.server)?,
            token: args.token,
            hello: Hello {
                name,
                max_concurrent_runs: runner.max_concurrent_runs(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                auth,
                host,
                claude_version,
            },
            data_dir,
            backoff: Backoff::default(),
        },
        runner,
    );
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("stopping: cancelling runs (Ctrl-C again to force)");
        let _ = stop_tx.send(true);
        shutdown_signal().await;
        std::process::exit(130);
    });
    client.run(stop_rx).await
}

/// Ctrl-C, or SIGTERM (what systemd / launchd send to stop a service).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn persistent_id(path: &Path) -> anyhow::Result<Id> {
    if let Ok(s) = std::fs::read_to_string(path) {
        if let Ok(id) = s.trim().parse() {
            return Ok(id);
        }
    }
    let id = uuid::Uuid::now_v7();
    std::fs::write(path, id.to_string())?;
    Ok(id)
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "runner".into())
}

// ------------------------------------------------------------------ client

#[derive(Debug, Clone)]
pub struct Hello {
    pub name: String,
    pub max_concurrent_runs: u32,
    pub version: String,
    pub auth: RunnerAuth,
    pub host: String,
    pub claude_version: Option<String>,
}

impl Hello {
    fn wire(&self) -> RunnerWire {
        RunnerWire::Hello {
            name: self.name.clone(),
            max_concurrent_runs: self.max_concurrent_runs,
            version: self.version.clone(),
            auth: self.auth,
            host: self.host.clone(),
            claude_version: self.claude_version.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Backoff {
    pub min: Duration,
    pub max: Duration,
    /// A connection that lived this long resets the backoff.
    pub stable_after: Duration,
    /// Send a ping this often; drop the connection after 3 silent intervals.
    pub ping_every: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(1),
            max: Duration::from_secs(60),
            stable_after: Duration::from_secs(30),
            ping_every: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `wss://…/api/runners/ws`
    pub url: String,
    pub token: String,
    pub hello: Hello,
    pub data_dir: PathBuf,
    pub backoff: Backoff,
}

pub struct Client {
    cfg: ClientConfig,
    runner: Arc<dyn Runner>,
    outbox_tx: mpsc::UnboundedSender<RunnerWire>,
    outbox_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<RunnerWire>>,
    active: Arc<Mutex<HashSet<Id>>>,
    /// Cancels that arrived while a run was still being prepared (repo clone/fetch).
    pending_cancel: Arc<Mutex<HashSet<Id>>>,
    repos: Arc<RepoCache>,
    skills: Arc<Mutex<Vec<Skill>>>,
}

enum SessionEnd {
    Lost(String),
    /// The server refused this runner for good (bad token, policy): do not retry.
    Fatal(String),
    Stopped,
}

/// WS close codes the server uses to refuse a runner permanently (4003: a shared runner
/// reporting subscription auth, 4001: bad or revoked token).
pub const FATAL_CLOSE_CODES: &[u16] = &[4001, 4003];

impl Client {
    pub fn new(cfg: ClientConfig, runner: Arc<dyn Runner>) -> Arc<Self> {
        let (outbox_tx, outbox_rx) = mpsc::unbounded_channel();
        let repos = Arc::new(RepoCache::new(cfg.data_dir.join("repos"), Tools::default()));
        Arc::new(Self {
            cfg,
            runner,
            outbox_tx,
            outbox_rx: tokio::sync::Mutex::new(outbox_rx),
            active: Arc::new(Mutex::new(HashSet::new())),
            pending_cancel: Arc::new(Mutex::new(HashSet::new())),
            repos,
            skills: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn active_runs(&self) -> Vec<Id> {
        let mut v: Vec<Id> = self
            .active
            .lock()
            .map(|a| a.iter().copied().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Connect, serve, reconnect until `stop` turns true; then cancel live runs, flush
    /// their last updates (10 s max) and return.
    pub async fn run(self: &Arc<Self>, mut stop: watch::Receiver<bool>) -> anyhow::Result<()> {
        let b = &self.cfg.backoff;
        let mut delay = b.min;
        let mut rng = Rng::new(uuid::Uuid::now_v7().as_u128() as u64);
        let mut carry: Option<RunnerWire> = None;
        let mut stopping_since: Option<Instant> = None;
        loop {
            if *stop.borrow() && stopping_since.is_none() {
                stopping_since = Some(Instant::now());
                for id in self.active_runs() {
                    let _ = self.cancel(id).await;
                }
            }
            if let Some(t) = stopping_since {
                if self.active_runs().is_empty() && self.outbox_is_empty(&carry).await
                    || t.elapsed() > Duration::from_secs(10)
                {
                    return Ok(());
                }
            }
            let started = Instant::now();
            let end = match self.connect().await {
                Ok(ws) => {
                    tracing::info!(url = %self.cfg.url, "connected");
                    self.session(ws, &mut carry, &mut stop, stopping_since.is_some())
                        .await
                }
                Err(e) => e,
            };
            match end {
                SessionEnd::Stopped => return Ok(()),
                SessionEnd::Fatal(why) => bail!("the server refused this runner: {why}"),
                SessionEnd::Lost(why) => {
                    if started.elapsed() >= b.stable_after {
                        delay = b.min;
                    }
                    let jitter = delay.mul_f64(0.8 + 0.4 * rng.f64());
                    tracing::warn!(
                        "connection to the server lost ({why}); retrying in {:.1}s",
                        jitter.as_secs_f64()
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(jitter) => {}
                        _ = stop.changed() => {}
                    }
                    delay = (delay * 2).min(b.max);
                }
            }
        }
    }

    async fn outbox_is_empty(&self, carry: &Option<RunnerWire>) -> bool {
        carry.is_none() && self.outbox_rx.lock().await.is_empty()
    }

    async fn connect(
        &self,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        SessionEnd,
    > {
        let mut req = self
            .cfg
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| SessionEnd::Fatal(e.to_string()))?;
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.cfg.token))
            .map_err(|_| SessionEnd::Fatal("the token contains invalid characters".into()))?;
        req.headers_mut().insert("Authorization", auth);
        let connect = tokio_tungstenite::connect_async(req);
        match tokio::time::timeout(Duration::from_secs(20), connect).await {
            Err(_) => Err(SessionEnd::Lost("connect timed out".into())),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => {
                let status = resp.status();
                let body = resp
                    .body()
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                let msg = format!("HTTP {status} {}", emit::first_line(&body, 200));
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    Err(SessionEnd::Fatal(format!("runner token rejected ({msg})")))
                } else {
                    Err(SessionEnd::Lost(msg))
                }
            }
            Ok(Err(e)) => Err(SessionEnd::Lost(e.to_string())),
            Ok(Ok((ws, _))) => Ok(ws),
        }
    }

    async fn session(
        self: &Arc<Self>,
        ws: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        carry: &mut Option<RunnerWire>,
        stop: &mut watch::Receiver<bool>,
        mut stopping: bool,
    ) -> SessionEnd {
        let (mut sink, mut stream) = ws.split();
        macro_rules! send {
            ($msg:expr) => {{
                let msg: RunnerWire = $msg;
                match serde_json::to_string(&msg) {
                    Ok(text) => {
                        if let Err(e) = sink.send(Message::text(text)).await {
                            *carry = Some(msg);
                            return SessionEnd::Lost(format!("send failed: {e}"));
                        }
                    }
                    Err(e) => tracing::error!("cannot encode {msg:?}: {e}"),
                }
            }};
        }
        send!(self.cfg.hello.wire());
        send!(RunnerWire::Resume {
            run_ids: self.active_runs()
        });
        let skills = self
            .refresh_skills()
            .await
            .unwrap_or_else(|| self.current_skills());
        send!(RunnerWire::Skills { skills });
        if let Some(m) = carry.take() {
            send!(m);
        }

        let mut outbox = self.outbox_rx.lock().await;
        let ping_every = self.cfg.backoff.ping_every;
        let mut ping = tokio::time::interval_at(Instant::now() + ping_every, ping_every);
        let mut skills_tick = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(120),
            Duration::from_secs(120),
        );
        let mut last_seen = Instant::now();
        let mut stop_deadline: Option<Instant> = None;
        loop {
            if stopping {
                let deadline =
                    *stop_deadline.get_or_insert_with(|| Instant::now() + Duration::from_secs(10));
                if (self.active_runs().is_empty() && outbox.is_empty()) || Instant::now() > deadline
                {
                    let _ = sink.send(Message::Close(None)).await;
                    return SessionEnd::Stopped;
                }
            }
            tokio::select! {
                frame = stream.next() => match frame {
                    Some(Ok(Message::Text(t))) => {
                        last_seen = Instant::now();
                        match serde_json::from_str::<RunnerWire>(t.as_str()) {
                            Ok(msg) => self.handle(msg).await,
                            Err(e) => tracing::warn!("ignoring an unreadable server message: {e}"),
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let Some(f) = frame else { return SessionEnd::Lost("closed by server".into()) };
                        let code = u16::from(f.code);
                        let why = format!("closed by server ({code}): {}", f.reason);
                        return if FATAL_CLOSE_CODES.contains(&code) { SessionEnd::Fatal(why) } else { SessionEnd::Lost(why) };
                    }
                    Some(Ok(_)) => last_seen = Instant::now(),
                    Some(Err(e)) => return SessionEnd::Lost(e.to_string()),
                    None => return SessionEnd::Lost("connection closed".into()),
                },
                Some(msg) = outbox.recv() => send!(msg),
                _ = ping.tick() => {
                    if last_seen.elapsed() > ping_every * 3 {
                        return SessionEnd::Lost("no answer from the server".into());
                    }
                    if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await {
                        return SessionEnd::Lost(format!("ping failed: {e}"));
                    }
                }
                _ = skills_tick.tick() => {
                    if let Some(skills) = self.refresh_skills().await {
                        send!(RunnerWire::Skills { skills });
                    }
                }
                _ = stop.changed(), if !stopping => {
                    stopping = true;
                    for id in self.active_runs() {
                        let _ = self.cancel(id).await;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)), if stopping => {}
            }
        }
    }

    fn current_skills(&self) -> Vec<Skill> {
        self.skills.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Rediscover skills; `Some` when they changed since last time.
    async fn refresh_skills(&self) -> Option<Vec<Skill>> {
        let repo_dirs: Vec<String> = std::fs::read_dir(&self.repos.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        let boba = self.cfg.data_dir.join("skills");
        let found = match self
            .runner
            .discover_skills(&repo_dirs, &boba.to_string_lossy())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("skill discovery failed: {e:#}");
                return None;
            }
        };
        let mut cur = self.skills.lock().ok()?;
        let key = |v: &[Skill]| {
            v.iter()
                .map(|s| (s.id, s.name.clone(), s.description.clone()))
                .collect::<Vec<_>>()
        };
        if key(&cur) == key(&found) && !cur.is_empty() {
            return None;
        }
        *cur = found.clone();
        Some(found)
    }

    async fn handle(self: &Arc<Self>, msg: RunnerWire) {
        let res = match msg {
            RunnerWire::Start { spec } => {
                self.clone().start(*spec);
                Ok(())
            }
            RunnerWire::Cancel { run_id } => self.cancel(run_id).await,
            RunnerWire::Message { run_id, text } => self.runner.send_message(run_id, text).await,
            RunnerWire::Permission {
                run_id,
                request_id,
                allow,
            } => {
                self.runner
                    .answer_permission(run_id, request_id, allow)
                    .await
            }
            other => {
                tracing::debug!("ignoring {other:?} from the server");
                Ok(())
            }
        };
        if let Err(e) = res {
            tracing::warn!("server request failed: {e:#}");
        }
    }

    /// Cancel a run, also when it is still being prepared and unknown to the runner yet.
    async fn cancel(&self, run_id: Id) -> anyhow::Result<()> {
        if !self
            .active
            .lock()
            .map(|a| a.contains(&run_id))
            .unwrap_or(false)
        {
            return self.runner.cancel(run_id).await;
        }
        if let Ok(mut p) = self.pending_cancel.lock() {
            p.insert(run_id);
        }
        // Already started: cancel now (the pending mark is then harmless).
        let _ = self.runner.cancel(run_id).await;
        Ok(())
    }

    fn take_pending_cancel(&self, run_id: Id) -> bool {
        self.pending_cancel
            .lock()
            .map(|mut p| p.remove(&run_id))
            .unwrap_or(false)
    }

    fn start(self: Arc<Self>, spec: RunSpec) {
        let run_id = spec.run_id;
        {
            let Ok(mut active) = self.active.lock() else {
                return;
            };
            if !active.insert(run_id) {
                tracing::debug!(%run_id, "duplicate Start ignored");
                return;
            }
        }
        tokio::spawn(async move {
            let outbox = self.outbox_tx.clone();
            let send = |update: RunUpdate| {
                let _ = outbox.send(RunnerWire::Update { run_id, update });
            };
            let spec = match self.localize(spec).await {
                Ok(s) => s,
                Err(e) => {
                    send(RunUpdate::Finished {
                        outcome: emit::failed(format!("{e:#}")),
                    });
                    self.done(run_id);
                    return;
                }
            };
            if self.take_pending_cancel(run_id) {
                send(RunUpdate::Finished {
                    outcome: emit::outcome(boba_core::RunStatus::Cancelled),
                });
                self.done(run_id);
                return;
            }
            let (tx, mut rx) = mpsc::channel(256);
            if let Err(e) = self.runner.start(spec, tx).await {
                send(RunUpdate::Finished {
                    outcome: emit::failed(format!("could not start the run: {e:#}")),
                });
                self.done(run_id);
                return;
            }
            // A cancel that raced the start (the runner did not know the run yet).
            if self.take_pending_cancel(run_id) {
                let _ = self.runner.cancel(run_id).await;
            }
            let mut finished = false;
            while let Some(u) = rx.recv().await {
                finished |= matches!(u, RunUpdate::Finished { .. });
                send(u);
            }
            if !finished {
                send(RunUpdate::Finished {
                    outcome: emit::failed("the runner stopped without finishing the run"),
                });
            }
            self.done(run_id);
        });
    }

    fn done(&self, run_id: Id) {
        if let Ok(mut a) = self.active.lock() {
            a.remove(&run_id);
        }
        if let Ok(mut p) = self.pending_cancel.lock() {
            p.remove(&run_id);
        }
    }

    /// Map server paths to this machine: the repo by `remote_url`, skills by name.
    async fn localize(&self, mut spec: RunSpec) -> anyhow::Result<RunSpec> {
        if let Some(repo) = spec.repo.as_mut() {
            let url = repo.remote_url.clone().filter(|u| !u.trim().is_empty()).ok_or_else(|| {
                anyhow!("remote runners need the repo's remote URL (set it on the repo in Boba Agent)")
            })?;
            let tools = Tools::default().with_auth(
                repo.git_token.as_ref().map(|t| t.expose()),
                repo.git_author
                    .as_ref()
                    .map(|a| (a.name.as_str(), a.email.as_str())),
            );
            let local = self.repos.prepare_with(&url, &tools).await?;
            repo.path = local.to_string_lossy().into_owned();
        }
        if !spec.skill_names.is_empty() {
            if self.current_skills().is_empty() {
                self.refresh_skills().await;
            }
            let skills = self.current_skills();
            let mut paths = Vec::new();
            for name in &spec.skill_names {
                match skills.iter().find(|s| &s.name == name) {
                    Some(s) => paths.push(s.path.clone()),
                    None => tracing::warn!(%name, "skill not installed on this runner"),
                }
            }
            spec.skill_paths = paths;
        } else {
            // Server paths only make sense if they also exist here.
            spec.skill_paths
                .retain(|p| Path::new(p).join("SKILL.md").is_file());
        }
        Ok(spec)
    }
}

// ------------------------------------------------------------------ repo clones

/// Clones of remote repos under one directory, one per remote URL.
pub struct RepoCache {
    pub root: PathBuf,
    tools: Tools,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl RepoCache {
    pub fn new(root: PathBuf, tools: Tools) -> Self {
        Self {
            root,
            tools,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// `git@github.com:o/r.git` -> `o-r`; other URLs -> a sanitized tail of the URL.
    pub fn slug(url: &str) -> String {
        if let Some(s) = crate::demo::github_slug(url) {
            return s.replace('/', "-");
        }
        let tail: String = url
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .rsplit(['/', ':'])
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("-");
        let s: String = tail
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let s = s.trim_matches(['-', '.']).to_string();
        if s.is_empty() {
            "repo".into()
        } else {
            s
        }
    }

    /// Clone `url` if needed, else fetch it. Serialized per repo.
    pub async fn prepare(&self, url: &str) -> anyhow::Result<PathBuf> {
        let tools = self.tools.clone();
        self.prepare_with(url, &tools).await
    }

    /// [`prepare`](Self::prepare) with specific (e.g. token-authenticated) tools.
    pub async fn prepare_with(&self, url: &str, tools: &Tools) -> anyhow::Result<PathBuf> {
        let slug = Self::slug(url);
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .map_err(|_| anyhow!("repo cache poisoned"))?;
            locks.entry(slug.clone()).or_default().clone()
        };
        let _guard = lock.lock().await;
        let dir = self.root.join(&slug);
        if dir.join(".git").exists() {
            let current = self
                .tools
                .git(&dir, &["remote", "get-url", "origin"])
                .await
                .unwrap_or_default();
            if current != url {
                bail!(
                    "{} is a clone of {current}, not {url}; remove it or rename the repo",
                    dir.display()
                );
            }
            if let Err(e) = tools.fetch(&dir).await {
                tracing::warn!("git fetch failed for {url}: {e:#} (using the existing clone)");
            }
        } else {
            std::fs::create_dir_all(&self.root)?;
            let _ = std::fs::remove_dir_all(&dir);
            tools
                .git(
                    &self.root,
                    &["clone", "--quiet", url, &dir.to_string_lossy()],
                )
                .await
                .with_context(|| {
                    format!("cloning {url} (does this machine have git access to it?)")
                })?;
        }
        Ok(dir)
    }
}

#[cfg(test)]
mod tests;

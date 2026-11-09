//! `boba-agent`: the self-hosted Boba Agent server.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use boba_core::team::{RunnerAuth, RunnerScope};
use boba_core::Id;
use boba_runner::{ClaudeCodeRunner, ClaudeConfig, DemoConfig, DemoRunner, Runner};
use boba_server::router::{RunnerSlot, Runners};
use boba_server::{build, shutdown_runs, spawn_housekeeping, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn,sqlx=warn")),
        )
        .init();

    let mut cfg = Config::from_env();
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let runner_id = local_runner_id(&cfg)?;

    let runners = pick_runners(&mut cfg, runner_id).await?;
    tracing::info!(demo = cfg.demo_mode, data = %cfg.data_dir.display(), "starting Boba Agent");

    let addr = cfg.addr;
    let (state, app) = build(cfg, runners).await?;
    spawn_housekeeping(state.clone());

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("🧋 Boba Agent is ready on http://{addr}");
    let on_signal = async move {
        shutdown().await;
        // Stop agents first: a killed server must not leave `claude` processes behind.
        shutdown_runs(&state, Duration::from_secs(10)).await;
        // Open WebSockets would keep a graceful shutdown waiting forever.
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            tracing::warn!("connections still open, exiting");
            std::process::exit(0);
        });
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(on_signal)
        .await?;
    Ok(())
}

/// The runners this server routes to.
///
/// * demo mode: the demo runner only (shared, simulated).
/// * API key / Bedrock / Vertex: Claude Code as a shared runner.
/// * `BOBA_CLAUDE_AUTH=subscription`: Claude Code as the PERSONAL runner of
///   `BOBA_LOCAL_RUNNER_OWNER` (Anthropic's terms keep a subscription for its owner's own
///   work), plus the demo runner for everyone else, whose runs are marked as simulated.
///
/// Exactly one ClaudeCodeRunner per data dir, created before any run: on creation it reaps
/// `claude` processes and run dirs left behind by a crashed server.
async fn pick_runners(cfg: &mut Config, runner_id: Id) -> anyhow::Result<Runners> {
    let demo = || -> Arc<dyn Runner> {
        Arc::new(DemoRunner::new(DemoConfig::from_env(
            Id::new_v5(&runner_id, b"demo"),
            cfg.data_dir.clone(),
        )))
    };
    if cfg.demo_mode {
        tracing::info!("no Claude credentials configured: demo mode (simulated runs)");
        return Ok(Runners::from(demo()));
    }
    if cfg.claude_subscription && cfg.local_runner_owner.is_none() {
        anyhow::bail!(
            "BOBA_CLAUDE_AUTH=subscription needs BOBA_LOCAL_RUNNER_OWNER=<email of the subscription owner>: \
             a Claude subscription may only run its owner's work"
        );
    }
    let claude =
        match ClaudeCodeRunner::new(ClaudeConfig::from_env(runner_id, cfg.data_dir.clone())) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Claude Code runner failed to start: demo mode");
                cfg.demo_mode = true;
                return Ok(Runners::from(demo()));
            }
        };
    let version = match claude.claude_version().await {
        Ok(v) => v.trim().to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "`claude` CLI not usable: demo mode");
            cfg.demo_mode = true;
            return Ok(Runners::from(demo()));
        }
    };
    let auth = if cfg.claude_subscription {
        RunnerAuth::Subscription
    } else if std::env::var("CLAUDE_CODE_USE_BEDROCK").is_ok_and(|v| !v.is_empty()) {
        RunnerAuth::Bedrock
    } else if std::env::var("CLAUDE_CODE_USE_VERTEX").is_ok_and(|v| !v.is_empty()) {
        RunnerAuth::Vertex
    } else {
        RunnerAuth::ApiKey
    };
    let host = hostname();
    let mut slot = RunnerSlot::shared(Arc::new(claude), auth);
    slot.claude_version = Some(version.clone());
    slot.host = host.clone();
    let slots = match &cfg.local_runner_owner {
        Some(owner) => {
            slot.scope = RunnerScope::Personal;
            slot.owner_email = Some(owner.clone());
            tracing::info!(%version, %owner, "Claude Code runs only {owner}'s work; everyone else's is simulated");
            let mut fallback = RunnerSlot::shared(demo(), RunnerAuth::Demo);
            fallback.host = host;
            vec![slot, fallback]
        }
        None => {
            tracing::info!(%version, "Claude Code runs everyone's work (shared runner)");
            vec![slot]
        }
    };
    Ok(Runners::new(slots))
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The local runner's id is stable across restarts.
fn local_runner_id(cfg: &Config) -> anyhow::Result<Id> {
    let path = cfg.data_dir.join("runner_id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(id) = Id::parse_str(s.trim()) {
            return Ok(id);
        }
    }
    let id = Id::now_v7();
    std::fs::write(&path, id.to_string()).with_context(|| format!("writing {}", path.display()))?;
    Ok(id)
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}

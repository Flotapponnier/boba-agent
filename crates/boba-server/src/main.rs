//! `boba-agent`: the self-hosted Boba Agent server.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use boba_core::Id;
use boba_runner::{DemoConfig, DemoRunner, Runner};
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

    if !cfg.demo_mode {
        // The Claude Code runner ships in boba-runner next; until then everything is simulated.
        tracing::warn!("Claude Code runner not available in this build yet: starting in demo mode");
        cfg.demo_mode = true;
    }
    let runner: Arc<dyn Runner> = Arc::new(DemoRunner::new(DemoConfig::from_env(
        runner_id,
        cfg.data_dir.clone(),
    )));
    tracing::info!(demo = cfg.demo_mode, data = %cfg.data_dir.display(), "starting Boba Agent");

    let addr = cfg.addr;
    let (state, app) = build(cfg, runner).await?;
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

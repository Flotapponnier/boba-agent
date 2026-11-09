//! # boba-server
//!
//! axum REST + WebSocket API, SQLite persistence, auth/RBAC, the run orchestrator and the
//! embedded web UI. The binary (`boba-agent`) is a thin wrapper around [`build`].

pub mod auth;
pub mod comments;
pub mod config;
pub mod connectors;
pub mod db;
pub mod error;
pub mod hub;
pub mod notify;
pub mod orchestrator;
pub mod routes;
pub mod seed;
pub mod skills;
pub mod speech;
pub mod state;
pub mod web;
pub mod ws;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderValue, Method};
use axum::Router;
use boba_runner::Runner;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub use config::Config;
pub use state::{AppState, SharedState};

/// Open the database, recover from a previous crash, discover skills and build the router.
pub async fn build(cfg: Config, runner: Arc<dyn Runner>) -> anyhow::Result<(SharedState, Router)> {
    let state = AppState::new(cfg, runner).await?;
    if let Err(e) = skills::rescan(&state).await {
        tracing::warn!(error = ?e, "initial skill scan failed");
    }
    orchestrator::recover(&state).await?;
    let router = router(state.clone());
    Ok((state, router))
}

pub fn router(state: SharedState) -> Router {
    let mut app = Router::new()
        .nest("/api", routes::api())
        .fallback(web::serve)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());
    if let Some(origin) = &state.cfg.dev_cors {
        match origin.parse::<HeaderValue>() {
            Ok(o) => {
                app = app.layer(
                    CorsLayer::new()
                        .allow_origin(o)
                        .allow_credentials(true)
                        .allow_methods([
                            Method::GET,
                            Method::POST,
                            Method::PATCH,
                            Method::PUT,
                            Method::DELETE,
                        ])
                        .allow_headers([header::CONTENT_TYPE]),
                );
            }
            Err(_) => tracing::warn!(%origin, "ignoring invalid BOBA_DEV_CORS"),
        }
    }
    app.with_state(state)
}

/// Graceful shutdown: cancel every live run (so no `claude` process outlives the server)
/// and wait up to `grace` for the runner to confirm.
pub async fn shutdown_runs(state: &SharedState, grace: Duration) {
    let live: Vec<boba_core::Id> = state
        .live
        .lock()
        .map(|l| l.iter().copied().collect())
        .unwrap_or_default();
    if live.is_empty() {
        return;
    }
    tracing::info!(runs = live.len(), "cancelling live runs before exit");
    for id in &live {
        if let Err(e) = state.runner.cancel(*id).await {
            tracing::warn!(error = ?e, run_id = %id, "cancel on shutdown");
        }
    }
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        let left = state.live.lock().map(|l| l.len()).unwrap_or(0);
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    state.flush_logs().await;
}

/// Background housekeeping: expired sessions.
pub fn spawn_housekeeping(state: SharedState) {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(3600));
        loop {
            t.tick().await;
            if let Err(e) = state.db.purge_expired_sessions().await {
                tracing::warn!(error = ?e, "purging sessions");
            }
        }
    });
}

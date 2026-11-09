//! HTTP routes (see docs/API.md).

mod auth;
mod board;
mod team;
mod work;

use axum::routing::{get, patch, post};
use axum::Router;

use crate::state::SharedState;

pub fn api() -> Router<SharedState> {
    Router::new()
        // auth & users
        .route("/auth/status", get(auth::status))
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/accept-invite", post(auth::accept_invite))
        .route("/users/invite", post(auth::invite))
        .route("/users/{id}", patch(auth::update_user))
        // state
        .route("/snapshot", get(board::snapshot))
        .route("/history", get(board::history))
        .route("/settings", patch(board::update_settings))
        .route("/ws", get(crate::ws::upgrade))
        // tabs
        .route("/tabs", post(board::create_tab))
        .route(
            "/tabs/{id}",
            patch(board::update_tab).delete(board::delete_tab),
        )
        // agents
        .route("/agents", post(work::create_agent))
        .route(
            "/agents/{id}",
            patch(work::update_agent).delete(work::delete_agent),
        )
        // tickets
        .route("/tickets", post(work::create_ticket))
        .route(
            "/tickets/{id}",
            patch(work::update_ticket).delete(work::delete_ticket),
        )
        .route("/tickets/{id}/move", post(work::move_ticket))
        .route("/tickets/{id}/assign", post(work::assign))
        .route("/tickets/{id}/runs", get(work::ticket_runs))
        // runs
        .route("/runs/{id}", get(work::get_run))
        .route("/runs/{id}/logs", get(work::run_logs))
        .route("/runs/{id}/cancel", post(work::cancel_run))
        .route("/runs/{id}/message", post(work::message_run))
        .route("/runs/{id}/permission", post(work::permission_run))
        // skills & repos
        .route("/skills", post(work::create_skill))
        .route("/skills/rescan", post(work::rescan_skills))
        .route("/skills/{id}", get(work::get_skill).put(work::update_skill))
        .route("/repos", post(board::create_repo))
        .route(
            "/repos/{id}",
            patch(board::update_repo).delete(board::delete_repo),
        )
        .route("/health", get(|| async { "ok" }))
        // quests
        .route("/jobs", post(crate::jobs::create))
        .route("/jobs/preview-schedule", post(crate::jobs::preview))
        .route(
            "/jobs/{id}",
            patch(crate::jobs::update).delete(crate::jobs::delete),
        )
        .route("/jobs/{id}/run", post(crate::jobs::run_now))
        .route("/jobs/{id}/runs", get(crate::jobs::runs))
        // remote runners
        .route("/runners", post(crate::runners_api::create))
        .route("/runners/ws", get(crate::runners_api::upgrade))
        .route(
            "/runners/{id}",
            axum::routing::delete(crate::runners_api::delete),
        )
        // team
        .route("/me", patch(team::update_me))
        .route("/notifications", get(team::list_notifications))
        .route("/tickets/{id}/comments", get(crate::comments::list))
        .route("/comments", post(crate::comments::create))
        .route(
            "/comments/{id}",
            patch(crate::comments::update).delete(crate::comments::delete),
        )
        .route("/comments/{id}/react", post(crate::comments::react))
        .route("/notifications/read", post(team::mark_read))
        // connectors & speech (coordinator workstream)
        .route("/connectors", post(crate::connectors::create))
        .route(
            "/connectors/{id}",
            patch(crate::connectors::update).delete(crate::connectors::delete),
        )
        .route("/connectors/{id}/test", post(crate::connectors::test))
        .route(
            "/speech/transcribe",
            post(crate::speech::transcribe).layer(axum::extract::DefaultBodyLimit::max(
                crate::speech::MAX_AUDIO_BYTES,
            )),
        )
}

//! # boba-core
//!
//! The shared contract of Boba Agent. Every crate and the web UI speak these types:
//!
//! * [`model`]  – persisted domain entities (agents, tickets, tabs, runs…)
//! * [`api`]    – REST request/response bodies
//! * [`event`]  – the realtime WebSocket protocol and the history log
//! * [`runner`] – what the server hands to a runner and what a runner streams back
//!
//! TypeScript bindings are generated with `cargo test -p boba-core export_bindings`
//! into `web/src/api/generated/` (ts-rs). Never hand-edit the generated files.
//!
//! Changing anything here is a contract change: open a PR labelled `contract`
//! and ping every workstream (see CONTRIBUTING.md).

pub mod api;
pub mod event;
pub mod model;
pub mod runner;

pub use model::*;

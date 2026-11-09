//! Helper that turns runner events into [`RunUpdate`]s on a run's channel.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use boba_core::runner::{RunOutcome, RunUpdate};
use boba_core::{Id, LogKind, RunLogLine, RunStatus};
use chrono::Utc;
use serde_json::{json, Value};

use crate::UpdateTx;

/// The receiving side (the server) went away: stop working, nobody is listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// Cheap to clone; all clones share the per-run `seq` counter.
#[derive(Clone)]
pub struct Emitter {
    run_id: Id,
    seq: Arc<AtomicU64>,
    tx: UpdateTx,
}

impl Emitter {
    pub fn new(run_id: Id, tx: UpdateTx) -> Self {
        Self {
            run_id,
            seq: Arc::new(AtomicU64::new(0)),
            tx,
        }
    }

    pub fn run_id(&self) -> Id {
        self.run_id
    }

    pub async fn send(&self, update: RunUpdate) -> Result<(), Closed> {
        self.tx.send(update).await.map_err(|_| Closed)
    }

    pub async fn log(
        &self,
        kind: LogKind,
        text: impl Into<String>,
        data: Option<Value>,
    ) -> Result<(), Closed> {
        let line = RunLogLine {
            run_id: self.run_id,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            ts: Utc::now(),
            kind,
            text: text.into(),
            data,
        };
        self.send(RunUpdate::Log { line }).await
    }

    pub async fn system(&self, text: impl Into<String>) -> Result<(), Closed> {
        self.log(LogKind::System, text, None).await
    }

    pub async fn text(&self, text: impl Into<String>) -> Result<(), Closed> {
        self.log(LogKind::Text, text, None).await
    }

    pub async fn thinking(&self, text: impl Into<String>) -> Result<(), Closed> {
        self.log(LogKind::Thinking, text, None).await
    }

    pub async fn error(&self, text: impl Into<String>) -> Result<(), Closed> {
        self.log(LogKind::Error, text, None).await
    }

    pub async fn user(&self, text: impl Into<String>) -> Result<(), Closed> {
        self.log(LogKind::User, text, None).await
    }

    /// `data` = `{"id", "name", "input"}`; `text` is a one-line rendering.
    pub async fn tool_use(&self, id: &str, name: &str, input: Value) -> Result<(), Closed> {
        let text = format!("{name} {}", tool_input_summary(name, &input));
        self.log(
            LogKind::ToolUse,
            text.trim_end(),
            Some(json!({ "id": id, "name": name, "input": input })),
        )
        .await
    }

    /// `data` = `{"tool_use_id", "output", "is_error"}`.
    pub async fn tool_result(
        &self,
        tool_use_id: &str,
        output: &str,
        is_error: bool,
    ) -> Result<(), Closed> {
        self.log(
            LogKind::ToolResult,
            truncate_lines(output, 40),
            Some(json!({ "tool_use_id": tool_use_id, "output": output, "is_error": is_error })),
        )
        .await
    }

    pub async fn progress(&self, progress: f32, activity: impl Into<String>) -> Result<(), Closed> {
        self.send(RunUpdate::Progress {
            progress: progress.clamp(0.0, 1.0),
            activity: activity.into(),
        })
        .await
    }

    /// Cumulative totals for the run so far.
    pub async fn usage(
        &self,
        cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<(), Closed> {
        self.send(RunUpdate::Usage {
            cost_usd,
            input_tokens,
            output_tokens,
        })
        .await
    }

    pub async fn finished(&self, outcome: RunOutcome) -> Result<(), Closed> {
        self.send(RunUpdate::Finished { outcome }).await
    }
}

pub fn outcome(status: RunStatus) -> RunOutcome {
    RunOutcome {
        status,
        summary: None,
        verdict: None,
        feedback: None,
        error: None,
        plan: None,
    }
}

pub fn failed(error: impl Into<String>) -> RunOutcome {
    RunOutcome {
        error: Some(error.into()),
        ..outcome(RunStatus::Failed)
    }
}

/// Short human rendering of a tool call's input, used for log text and activity lines.
pub fn tool_input_summary(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default();
    let out = match name {
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => s("file_path").to_string(),
        "Bash" => s("command").to_string(),
        "Grep" => match input.get("path").and_then(Value::as_str) {
            Some(p) => format!("{:?} in {p}", s("pattern")),
            None => format!("{:?}", s("pattern")),
        },
        "Glob" => s("pattern").to_string(),
        "WebFetch" => s("url").to_string(),
        "WebSearch" => s("query").to_string(),
        "Task" | "Agent" => s("description").to_string(),
        "TodoWrite" => "update todo list".to_string(),
        _ => String::new(),
    };
    first_line(&out, 160)
}

pub fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or_default();
    if line.chars().count() > max {
        let mut t: String = line.chars().take(max).collect();
        t.push('…');
        t
    } else {
        line.to_string()
    }
}

fn truncate_lines(s: &str, max_lines: usize) -> String {
    let total = s.lines().count();
    if total <= max_lines {
        return s.to_string();
    }
    let mut out: Vec<&str> = s.lines().take(max_lines).collect();
    let more = format!("… ({} more lines)", total - max_lines);
    out.push(&more);
    out.join("\n")
}

//! Parser for `claude -p --output-format stream-json --verbose` output.
//!
//! Line types seen with Claude Code 2.1.319 (see `tests/fixtures/*.jsonl`):
//! * `system` / `init` – `session_id`, `model`, `skills`, `plugins`, `mcp_servers`…
//! * `system` / `status`, `hook_*`, `thinking_tokens` – noise, ignored
//! * `system` / `compact_boundary` – the context was compacted
//! * `assistant` – `message.content[]` blocks: `text`, `thinking` (may be empty when
//!   redacted), `tool_use {id, name, input}`. One line per block; the same `message.id`
//!   repeats with the same `usage`.
//! * `user` – `tool_result {tool_use_id, content: string | [{type: text}], is_error?}`,
//!   or with `isReplay: true` the echo of a user message we wrote on stdin
//!   (`--replay-user-messages`): proof that Claude took it.
//! * `control_request` – `request.subtype = can_use_tool {tool_name, input, tool_use_id}`
//!   when `--permission-prompt-tool stdio` is set; must be answered on stdin.
//! * `result` – `subtype`, `is_error`, `result` (final text), `total_cost_usd`, `usage`,
//!   `num_turns`, `permission_denials`. Note: `subtype: "success"` with `is_error: true`
//!   happens (e.g. "Not logged in"), so `is_error` is what counts.

use std::collections::{HashMap, HashSet};

use boba_core::LogKind;
use serde_json::{json, Value};

use crate::emit::{first_line, tool_input_summary, truncate_lines};

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Init {
        session_id: String,
        model: Option<String>,
        skills: Vec<String>,
        /// MCP servers whose status is `failed` / `needs-auth`.
        mcp_failed: Vec<String>,
    },
    Log {
        kind: LogKind,
        text: String,
        data: Option<Value>,
    },
    /// A main-thread tool call (not a subagent's), for activity/progress heuristics.
    ToolCall {
        name: String,
        input: Value,
    },
    /// Cumulative tokens so far (cost only becomes known with the result).
    Tokens {
        input: u64,
        output: u64,
    },
    Permission {
        request_id: String,
        tool: String,
        input: Value,
    },
    /// A control request we do not support; it must be answered with an error.
    UnsupportedControl {
        request_id: String,
        subtype: String,
    },
    /// Claude consumed a user message we sent on stdin.
    Replay,
    Result(ResultInfo),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultInfo {
    pub is_error: bool,
    pub subtype: String,
    pub text: Option<String>,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub num_turns: u64,
    pub permission_denials: usize,
    pub errors: Vec<String>,
}

#[derive(Default)]
pub struct StreamParser {
    seen_messages: HashSet<String>,
    input_tokens: u64,
    output_tokens: u64,
    tool_names: HashMap<String, String>,
}

fn total_input(usage: &Value) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .filter_map(|k| usage.get(k).and_then(Value::as_u64))
    .sum()
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tool_name(&self, tool_use_id: &str) -> Option<&str> {
        self.tool_names.get(tool_use_id).map(String::as_str)
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<Event> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![Event::Log {
                kind: LogKind::System,
                text: first_line(line, 500),
                data: None,
            }];
        };
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "system" => self.system(&v),
            "assistant" => self.assistant(&v),
            "user" => self.user(&v),
            "control_request" => control_request(&v),
            "result" => vec![Event::Result(result(&v))],
            _ => vec![],
        }
    }

    fn system(&mut self, v: &Value) -> Vec<Event> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        match s("subtype").as_deref() {
            Some("init") => vec![Event::Init {
                session_id: s("session_id").unwrap_or_default(),
                model: s("model"),
                skills: v
                    .get("skills")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                mcp_failed: v
                    .get("mcp_servers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|m| {
                        matches!(
                            m.get("status").and_then(Value::as_str),
                            Some("failed" | "needs-auth")
                        )
                    })
                    .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string))
                    .collect(),
            }],
            Some("compact_boundary") => vec![Event::Log {
                kind: LogKind::System,
                text: "Context compacted".into(),
                data: None,
            }],
            Some("permission_denied") => vec![Event::Log {
                kind: LogKind::System,
                text: format!(
                    "Permission denied{}",
                    s("tool_name").map(|t| format!(": {t}")).unwrap_or_default()
                ),
                data: None,
            }],
            Some("api_error") | Some("error") => vec![Event::Log {
                kind: LogKind::Error,
                text: s("message")
                    .or_else(|| s("error"))
                    .unwrap_or_else(|| first_line(&v.to_string(), 300)),
                data: None,
            }],
            _ => vec![],
        }
    }

    fn assistant(&mut self, v: &Value) -> Vec<Event> {
        let Some(msg) = v.get("message") else {
            return vec![];
        };
        let parent = v.get("parent_tool_use_id").and_then(Value::as_str);
        let mut out = Vec::new();
        for block in msg
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text" => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !text.trim().is_empty() {
                        out.push(Event::Log {
                            kind: LogKind::Text,
                            text: text.to_string(),
                            data: parent.map(|p| json!({ "parent_tool_use_id": p })),
                        });
                    }
                }
                "thinking" => {
                    let text = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !text.trim().is_empty() {
                        out.push(Event::Log {
                            kind: LogKind::Thinking,
                            text: text.to_string(),
                            data: None,
                        });
                    }
                }
                "tool_use" | "server_tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    self.tool_names.insert(id.clone(), name.clone());
                    let mut data = json!({ "id": id, "name": name, "input": input });
                    if let Some(p) = parent {
                        data["parent_tool_use_id"] = json!(p);
                    }
                    out.push(Event::Log {
                        kind: LogKind::ToolUse,
                        text: format!("{name} {}", tool_input_summary(&name, &input))
                            .trim_end()
                            .to_string(),
                        data: Some(data),
                    });
                    if parent.is_none() {
                        out.push(Event::ToolCall { name, input });
                    }
                }
                _ => {}
            }
        }
        if let (Some(id), Some(usage)) = (msg.get("id").and_then(Value::as_str), msg.get("usage")) {
            if self.seen_messages.insert(id.to_string()) {
                self.input_tokens += total_input(usage);
                self.output_tokens += usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                out.push(Event::Tokens {
                    input: self.input_tokens,
                    output: self.output_tokens,
                });
            }
        }
        out
    }

    fn user(&mut self, v: &Value) -> Vec<Event> {
        if v.get("isReplay").and_then(Value::as_bool) == Some(true) {
            return vec![Event::Replay];
        }
        let parent = v.get("parent_tool_use_id").and_then(Value::as_str);
        let content = v.pointer("/message/content");
        let mut out = Vec::new();
        for block in content.and_then(Value::as_array).into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let output = tool_result_text(block.get("content"));
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut data = json!({ "tool_use_id": id, "output": output, "is_error": is_error });
            if let Some(p) = parent {
                data["parent_tool_use_id"] = json!(p);
            }
            out.push(Event::Log {
                kind: LogKind::ToolResult,
                text: truncate_lines(&output, 40),
                data: Some(data),
            });
        }
        out
    }
}

fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn control_request(v: &Value) -> Vec<Event> {
    let request_id = v
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let req = v.get("request").cloned().unwrap_or_default();
    let subtype = req
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if subtype == "can_use_tool" {
        vec![Event::Permission {
            request_id,
            tool: req
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
            input: req.get("input").cloned().unwrap_or_else(|| json!({})),
        }]
    } else {
        vec![Event::UnsupportedControl {
            request_id,
            subtype,
        }]
    }
}

fn result(v: &Value) -> ResultInfo {
    let usage = v.get("usage").cloned().unwrap_or_default();
    ResultInfo {
        is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        subtype: v
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        text: v.get("result").and_then(Value::as_str).map(str::to_string),
        cost_usd: v
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        input_tokens: total_input(&usage),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        num_turns: v.get("num_turns").and_then(Value::as_u64).unwrap_or(0),
        permission_denials: v
            .get("permission_denials")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        errors: v
            .get("errors")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

// ------------------------------------------------------------------ stdin messages

pub fn user_message(text: &str) -> String {
    json!({ "type": "user", "message": { "role": "user", "content": text } }).to_string()
}

pub fn permission_response(
    request_id: &str,
    allow: bool,
    input: &Value,
    deny_message: &str,
) -> String {
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": deny_message })
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response }
    })
    .to_string()
}

pub fn control_error(request_id: &str, error: &str) -> String {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/stream_edit_bash_permission.jsonl");

    fn parse_all(src: &str) -> Vec<Event> {
        let mut p = StreamParser::new();
        src.lines().flat_map(|l| p.parse_line(l)).collect()
    }

    #[test]
    fn parses_real_transcript() {
        let events = parse_all(FIXTURE);
        let Event::Init {
            session_id, model, ..
        } = &events[0]
        else {
            panic!("init first: {:?}", events[0]);
        };
        assert_eq!(session_id, "79fdffc8-3886-4b96-9fd2-62f2eb765389");
        assert_eq!(model.as_deref(), Some("claude-haiku-5-5"));

        let replays = events.iter().filter(|e| matches!(e, Event::Replay)).count();
        assert_eq!(replays, 2, "initial prompt + the mid-run message");

        let tools: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tools, ["Read", "Edit", "Bash"]);

        // Every tool_result pairs with a tool_use.
        let mut use_ids = HashSet::new();
        let mut results = 0;
        for e in &events {
            if let Event::Log {
                kind,
                data: Some(d),
                ..
            } = e
            {
                match kind {
                    LogKind::ToolUse => {
                        use_ids.insert(d["id"].as_str().unwrap().to_string());
                    }
                    LogKind::ToolResult => {
                        results += 1;
                        assert!(use_ids.contains(d["tool_use_id"].as_str().unwrap()));
                        assert!(d["output"].is_string());
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(results, 3);
        assert!(events.iter().any(
            |e| matches!(e, Event::Log { kind: LogKind::ToolResult, text, .. } if text == "world")
        ));

        let perm = events.iter().find_map(|e| match e {
            Event::Permission {
                request_id,
                tool,
                input,
            } => Some((request_id, tool, input)),
            _ => None,
        });
        let (rid, tool, input) = perm.expect("permission request");
        assert_eq!(rid, "29f64b91-7691-4fcd-bc76-e5b7c54428d8");
        assert_eq!(tool, "Edit");
        assert_eq!(input["old_string"], "hello");

        // Tokens are cumulative and deduped per message id.
        let tokens: Vec<(u64, u64)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Tokens { input, output } => Some((*input, *output)),
                _ => None,
            })
            .collect();
        assert!(tokens
            .windows(2)
            .all(|w| w[1].0 >= w[0].0 && w[1].1 >= w[0].1));

        let Some(Event::Result(r)) = events.last() else {
            panic!("result last")
        };
        assert!(!r.is_error);
        assert_eq!(r.subtype, "success");
        assert!(r.text.as_deref().unwrap().ends_with("boba"));
        assert!(r.cost_usd > 0.0);
        assert!(r.output_tokens > 0);
    }

    #[test]
    fn edge_cases() {
        let mut p = StreamParser::new();
        assert_eq!(p.parse_line(""), vec![]);
        assert!(matches!(
            &p.parse_line("not json")[0],
            Event::Log {
                kind: LogKind::System,
                ..
            }
        ));
        // Empty (redacted) thinking is dropped, array tool_result content is joined.
        let ev = p.parse_line(r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":""}],"usage":{"input_tokens":3,"cache_read_input_tokens":7,"output_tokens":2}}}"#);
        assert_eq!(
            ev,
            vec![Event::Tokens {
                input: 10,
                output: 2
            }]
        );
        let ev = p.parse_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"a"},{"type":"image"}],"is_error":true}]}}"#);
        let Event::Log { data: Some(d), .. } = &ev[0] else {
            panic!()
        };
        assert_eq!(d["output"], "a\n[image]");
        assert_eq!(d["is_error"], true);
        // is_error wins over subtype.
        let ev = p.parse_line(r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#);
        assert!(matches!(&ev[0], Event::Result(r) if r.is_error));
        let ev = p.parse_line(
            r#"{"type":"control_request","request_id":"r","request":{"subtype":"hook_callback"}}"#,
        );
        assert_eq!(
            ev,
            vec![Event::UnsupportedControl {
                request_id: "r".into(),
                subtype: "hook_callback".into()
            }]
        );
    }

    #[test]
    fn stdin_messages() {
        let v: Value =
            serde_json::from_str(&permission_response("r1", true, &json!({"a":1}), "")).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedInput"]["a"], 1);
        let v: Value =
            serde_json::from_str(&permission_response("r1", false, &json!({}), "no")).unwrap();
        assert_eq!(v["response"]["response"]["message"], "no");
        let v: Value = serde_json::from_str(&user_message("hi")).unwrap();
        assert_eq!(v["message"]["content"], "hi");
    }
}

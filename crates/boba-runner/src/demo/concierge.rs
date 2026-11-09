//! Simulated Concierge (command bar / voice): keyword heuristics turn a sentence into a
//! `CommandPlan`-shaped JSON (`reply`, `actions[{label, action}]`, `question`), resolving
//! names through the board digest. Read-only: it only proposes, the user confirms.
//!
//! Digest fields read (all optional): `context {tab_id, ticket_id, agent_id}`,
//! `tabs[{id, name, columns[{id, name, kind, auto_reviewer_agent_id}]}]`,
//! `agents[{id, name, role}]`, `jobs[{id, name}]`, `repos[{id, name}]`,
//! `tickets[{id, number, title, tab_id}]`.

use boba_core::api::{AssignAgent, CommandAction, CreateAgent, CreateJob, CreateTab, CreateTicket};
use boba_core::automation::{JobOutput, Schedule};
use boba_core::runner::{RunOutcome, RunSpec, RunUpdate};
use boba_core::{Id, RunStatus, SPRITES};
use serde_json::{json, Value};

use super::{Ctx, Step};
use crate::emit;

pub(super) async fn run(spec: &RunSpec, ctx: &mut Ctx) -> Step<RunOutcome> {
    let Some(cmd) = spec.command.clone() else {
        return Ok(emit::failed("command run without a command"));
    };
    ctx.em
        .send(RunUpdate::Started {
            worktree_path: String::new(),
            branch: String::new(),
            session_id: Some(uuid::Uuid::now_v7().to_string()),
        })
        .await?;
    ctx.em.progress(0.2, "Understanding the request").await?;
    ctx.pause(500).await?;
    ctx.em
        .thinking(format!(
            "{} asked{}: \"{}\". Resolve names against the board, then propose actions for them to confirm.",
            cmd.user_name,
            if cmd.voice { " by voice" } else { "" },
            cmd.text
        ))
        .await?;
    ctx.spend(4_000, 120).await?;
    ctx.em.progress(0.6, "Matching names on the board").await?;
    ctx.pause(600).await?;
    let digest = with_context(&cmd);
    let plan = plan(&cmd.text, &digest, ctx.rng.next_u64());
    let block = serde_json::to_string_pretty(&plan).unwrap_or_default();
    let reply = plan["question"]
        .as_str()
        .or_else(|| plan["reply"].as_str())
        .unwrap_or_default()
        .to_string();
    ctx.em
        .text(format!("{reply}\n\n```boba-plan\n{block}\n```"))
        .await?;
    ctx.spend(1_500, 300).await?;
    ctx.em.progress(1.0, "Plan ready").await?;
    Ok(RunOutcome {
        summary: Some(reply),
        plan: Some(plan),
        ..emit::outcome(RunStatus::Succeeded)
    })
}

/// The digest with `context` filled from `CommandSpec` (preferred over the legacy
/// `board_digest.context`).
pub(crate) fn with_context(cmd: &boba_core::runner::CommandSpec) -> Value {
    let mut d = cmd.board_digest.clone();
    if !d.is_object() {
        d = json!({});
    }
    let mut ctx = d
        .get("context")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    for (k, v) in [
        ("tab_id", cmd.tab_id),
        ("ticket_id", cmd.ticket_id),
        ("agent_id", cmd.agent_id),
    ] {
        if let Some(id) = v {
            ctx[k] = json!(id);
        }
    }
    d["context"] = ctx;
    d
}

#[derive(Debug, Clone)]
struct Named {
    id: Id,
    name: String,
    extra: Value,
}

fn list(digest: &Value, key: &str) -> Vec<Named> {
    digest
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(Named {
                id: v.get("id")?.as_str()?.parse().ok()?,
                name: v
                    .get("name")
                    .or_else(|| v.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                extra: v.clone(),
            })
        })
        .collect()
}

/// Longest name mentioned in `text` (case-insensitive; a name's first word of 4+ chars
/// also matches, to forgive voice transcription).
fn mentioned<'a>(text: &str, items: &'a [Named]) -> Option<&'a Named> {
    let t = text.to_lowercase();
    let full = items
        .iter()
        .filter(|i| !i.name.is_empty() && t.contains(&i.name.to_lowercase()))
        .max_by_key(|i| i.name.len());
    full.or_else(|| {
        items.iter().find(|i| {
            i.name
                .split_whitespace()
                .next()
                .map(str::to_lowercase)
                .is_some_and(|w| {
                    w.len() >= 4 && t.split(|c: char| !c.is_alphanumeric()).any(|x| x == w)
                })
        })
    })
}

fn context_id(digest: &Value, key: &str) -> Option<Id> {
    digest
        .pointer(&format!("/context/{key}"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

/// "T-12", "t 12", "ticket 12", "#12".
fn ticket_number(text: &str) -> Option<i64> {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '#');
        if let Some(n) = w.strip_prefix("t-").or_else(|| w.strip_prefix('#')) {
            if let Ok(n) = n.parse() {
                return Some(n);
            }
        }
        if (w == "t" || w == "ticket") && i + 1 < words.len() {
            if let Ok(n) = words[i + 1]
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
            {
                return Some(n);
            }
        }
    }
    None
}

fn resolve_ticket(text: &str, digest: &Value) -> Option<(Id, String)> {
    if let Some(n) = ticket_number(text) {
        let found = digest
            .get("tickets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|t| t.get("number").and_then(Value::as_i64) == Some(n))
            .and_then(|t| t.get("id")?.as_str()?.parse().ok());
        return found.map(|id| (id, format!("T-{n}")));
    }
    context_id(digest, "ticket_id").map(|id| (id, "this ticket".to_string()))
}

/// Text after the first of `markers`, trimmed of filler, for names/titles.
fn after<'a>(text: &'a str, markers: &[&str]) -> Option<&'a str> {
    let lower = text.to_lowercase();
    markers.iter().find_map(|m| {
        let i = lower.find(m)?;
        let rest = text[i + m.len()..]
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'' || c == ':');
        (!rest.is_empty()).then_some(rest)
    })
}

/// Up to 3 words, stopping at the first connective ("Mochi to review PRs" -> "Mochi").
fn name_words(s: &str) -> String {
    const STOP: &[&str] = &[
        "to", "who", "that", "for", "and", "with", "which", "in", "on",
    ];
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'))
        .take_while(|w| !w.is_empty() && !STOP.contains(&w.to_lowercase().as_str()))
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn schedule_for(text: &str) -> (Schedule, &'static str) {
    let t = text.to_lowercase();
    let cron = |expr: &str| Schedule::Cron {
        expr: expr.into(),
        timezone: "UTC".into(),
    };
    let words: Vec<&str> = t.split_whitespace().collect();
    for w in words.windows(3) {
        if w[0] == "every" && w[2].starts_with("min") {
            if let Ok(m) = w[1].parse::<u32>() {
                return (
                    Schedule::Interval { minutes: m.max(5) },
                    "every few minutes",
                );
            }
        }
        if w[0] == "every" && w[2].starts_with("hour") {
            if let Ok(h) = w[1].parse::<u32>() {
                return (
                    Schedule::Interval {
                        minutes: (h * 60).max(5),
                    },
                    "every few hours",
                );
            }
        }
    }
    for (day, n) in [
        ("monday", 1),
        ("tuesday", 2),
        ("wednesday", 3),
        ("thursday", 4),
        ("friday", 5),
        ("saturday", 6),
        ("sunday", 0),
    ] {
        if t.contains(day) {
            return (cron(&format!("0 9 * * {n}")), "weekly at 09:00 UTC");
        }
    }
    if t.contains("hourly") || t.contains("every hour") {
        (Schedule::Interval { minutes: 60 }, "every hour")
    } else if t.contains("daily") || t.contains("every day") || t.contains("every morning") {
        (cron("0 9 * * *"), "daily at 09:00 UTC")
    } else if t.contains("weekly") || t.contains("every week") {
        (cron("0 9 * * 1"), "Mondays at 09:00 UTC")
    } else if t.contains("monthly") || t.contains("every month") {
        (cron("0 9 1 * *"), "monthly on the 1st at 09:00 UTC")
    } else {
        (Schedule::Manual, "on demand")
    }
}

fn action(label: String, a: CommandAction) -> Value {
    json!({ "label": label, "action": a })
}

fn ask(question: &str) -> Value {
    json!({ "reply": question, "actions": [], "question": question })
}

/// Turn a sentence into a plan. `seed` only picks cosmetic details (sprite).
pub fn plan(text: &str, digest: &Value, seed: u64) -> Value {
    let t = text.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    let agents = list(digest, "agents");
    let tabs = list(digest, "tabs");
    let jobs = list(digest, "jobs");
    let repos = list(digest, "repos");
    let agent = mentioned(text, &agents).cloned().or_else(|| {
        context_id(digest, "agent_id").and_then(|id| agents.iter().find(|a| a.id == id).cloned())
    });
    let tab = mentioned(text, &tabs)
        .cloned()
        .or_else(|| {
            context_id(digest, "tab_id").and_then(|id| tabs.iter().find(|x| x.id == id).cloned())
        })
        .or_else(|| tabs.first().cloned());

    let mut actions = Vec::new();
    let reply;

    if has(&[
        "create an agent",
        "new agent",
        "create agent",
        "hire",
        "add an agent",
    ]) {
        let name = after(text, &["named ", "called "])
            .map(name_words)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "New Boba".into());
        let role = if has(&["review"]) {
            "Reviewer"
        } else if has(&["seo"]) {
            "SEO"
        } else if has(&["triage", "support"]) {
            "Triage"
        } else {
            "Dev"
        };
        let sprite = SPRITES[(seed % SPRITES.len() as u64) as usize];
        let a = CreateAgent {
            connector_ids: None,
            board_tools: Some(role == "Triage"),
            name: name.clone(),
            role: role.into(),
            sprite: sprite.into(),
            color: None,
            tagline: None,
            system_prompt: format!(
                "You are {name}, the team's {role} agent. {}",
                capitalize(text.trim())
            ),
            skill_ids: vec![],
            model: None,
            permission_mode: None,
            allowed_tools: None,
            max_turns: None,
        };
        actions.push(action(
            format!("Create agent \"{name}\" ({role})"),
            CommandAction::CreateAgent { agent: a },
        ));
        reply = format!("Sure, I'll set up {name} as a {role} agent.");
    } else if has(&[
        "create a job",
        "new job",
        "schedule",
        "quest",
        "every ",
        "daily",
        "weekly",
        "hourly",
        "monthly",
    ]) && !has(&["run the job", "run job"])
    {
        let Some(agent) = agent.clone().or_else(|| agents.first().cloned()) else {
            return ask("Which agent should run this job? There are no agents yet.");
        };
        let (schedule, when) = schedule_for(text);
        let output = if has(&["ticket", "issue"]) {
            JobOutput::Tickets
        } else if has(&["pull request", " pr", "bump", "upgrade"]) {
            JobOutput::PullRequest
        } else {
            JobOutput::Report
        };
        let task = after(text, &[" that ", " to "])
            .unwrap_or(text)
            .trim()
            .to_string();
        let name = capitalize(
            &task
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" "),
        );
        let repo_id = mentioned(text, &repos).map(|r| r.id).or_else(|| {
            (output == JobOutput::PullRequest)
                .then(|| repos.first().map(|r| r.id))
                .flatten()
        });
        let job = CreateJob {
            name: name.clone(),
            description: Some(capitalize(text.trim())),
            icon: None,
            agent_id: agent.id,
            prompt: capitalize(&task),
            schedule,
            output,
            enabled: Some(true),
            repo_id,
            tab_id: tab.as_ref().map(|x| x.id),
            skip_if_running: Some(true),
        };
        actions.push(action(
            format!("Create job \"{name}\" ({when}) for {}", agent.name),
            CommandAction::CreateJob { job },
        ));
        reply = format!("Here's the quest for {}, running {when}.", agent.name);
    } else if has(&["create a tab", "new tab", "add a tab"]) {
        let name = after(text, &["named ", "called ", "tab for ", "tab "])
            .map(|n| capitalize(&name_words(n)))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "New tab".into());
        let tab = CreateTab {
            name: name.clone(),
            icon: None,
            color: None,
            repo_id: mentioned(text, &repos).map(|r| r.id),
            columns: None,
        };
        actions.push(action(
            format!("Create tab \"{name}\" with the default columns"),
            CommandAction::CreateTab { tab },
        ));
        reply = format!("I'll add a \"{name}\" tab.");
    } else if has(&["run the job", "run job", "run the quest", "start the job"])
        || (t.starts_with("run ") && mentioned(text, &jobs).is_some())
    {
        let Some(job) = mentioned(text, &jobs) else {
            return ask("Which job should I run?");
        };
        actions.push(action(
            format!("Run job \"{}\" now", job.name),
            CommandAction::RunJob { job_id: job.id },
        ));
        reply = format!("Starting \"{}\" now.", job.name);
    } else if has(&["sync"]) {
        let Some(repo) = mentioned(text, &repos).or_else(|| repos.first()) else {
            return ask("Which repo should I sync? None is configured yet.");
        };
        actions.push(action(
            format!("Sync {} with GitHub", repo.name),
            CommandAction::SyncRepo { repo_id: repo.id },
        ));
        reply = format!("Syncing {} with GitHub.", repo.name);
    } else if has(&["review"]) && resolve_ticket(text, digest).is_some() {
        let (ticket_id, label) = resolve_ticket(text, digest).expect("checked");
        // A Review column (preferably the one this agent auto-reviews) in the tab.
        let columns: Vec<Value> = tabs
            .iter()
            .flat_map(|x| {
                x.extra
                    .get("columns")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|c| c.get("kind").and_then(Value::as_str) == Some("review"))
            .collect();
        let by_agent = agent.as_ref().and_then(|a| {
            columns.iter().find(|c| {
                c.get("auto_reviewer_agent_id").and_then(Value::as_str)
                    == Some(a.id.to_string().as_str())
            })
        });
        let Some(col) = by_agent.or(columns.first()) else {
            return ask("There is no Review column yet. Which column should the ticket go to?");
        };
        let Some(column_id) = col
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
        else {
            return ask("Which Review column should I use?");
        };
        let who = agent
            .as_ref()
            .map(|a| format!(" for {}", a.name))
            .unwrap_or_default();
        actions.push(action(
            format!(
                "Move {label} to {}{who}",
                col.get("name").and_then(Value::as_str).unwrap_or("Review")
            ),
            CommandAction::MoveTicket {
                ticket_id,
                column_id,
            },
        ));
        reply = format!("Sending {label} to review{who}.");
    } else if has(&["work on", "assign", "pick up", "take ", "implement", "fix "])
        && resolve_ticket(text, digest).is_some()
    {
        let (ticket_id, label) = resolve_ticket(text, digest).expect("checked");
        let Some(agent) = agent.clone() else {
            return ask(&format!("Which agent should work on {label}?"));
        };
        actions.push(action(
            format!("Assign {} to {label}", agent.name),
            CommandAction::AssignAgent {
                ticket_id,
                assign: AssignAgent {
                    agent_id: agent.id,
                    instructions: None,
                },
            },
        ));
        reply = format!("{} will start on {label}.", agent.name);
    } else if has(&["ticket", "todo", "task", "bug", "remind"]) || has(&["add ", "create "]) {
        let Some(tab) = tab.clone() else {
            return ask("Which tab should the ticket go to? There are no tabs yet.");
        };
        let title = after(
            text,
            &[
                "ticket to ",
                "ticket for ",
                "ticket: ",
                "ticket ",
                "task to ",
                "bug: ",
                "todo: ",
                "add ",
                "create ",
            ],
        )
        .unwrap_or(text);
        let title = capitalize(title.trim_end_matches(['.', '!']).trim());
        let ticket = CreateTicket {
            tab_id: tab.id,
            column_id: None,
            title: title.clone(),
            body: Some(format!(
                "Requested from the command bar: \"{}\"",
                text.trim()
            )),
            tags: has(&["bug"]).then(|| vec!["bug".to_string()]),
            priority: None,
            color: None,
            repo_id: None,
        };
        actions.push(action(
            format!("Create ticket \"{title}\" in {}", tab.name),
            CommandAction::CreateTicket { ticket },
        ));
        if let Some(agent) = agent.filter(|_| has(&["and assign", "and have", "and ask"])) {
            reply = format!(
                "I'll create the ticket. Assign {} once it exists from its card.",
                agent.name
            );
        } else {
            reply = "I'll create that ticket.".into();
        }
    } else {
        return ask(
            "I can create tickets, agents, tabs and scheduled jobs, assign agents, send tickets to review, run jobs \
             and sync repos. What would you like me to do?",
        );
    }
    json!({ "reply": reply, "actions": actions, "question": null })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> Value {
        json!({
            "context": { "tab_id": "00000000-0000-0000-0000-0000000000a1", "ticket_id": null },
            "tabs": [{ "id": "00000000-0000-0000-0000-0000000000a1", "name": "Dev", "columns": [
                { "id": "00000000-0000-0000-0000-0000000000c1", "name": "Todo", "kind": "todo" },
                { "id": "00000000-0000-0000-0000-0000000000c2", "name": "PR review", "kind": "review",
                  "auto_reviewer_agent_id": "00000000-0000-0000-0000-0000000000b2" }
            ]}, { "id": "00000000-0000-0000-0000-0000000000a2", "name": "SEO", "columns": [] }],
            "agents": [
                { "id": "00000000-0000-0000-0000-0000000000b1", "name": "Pinky SEO", "role": "SEO" },
                { "id": "00000000-0000-0000-0000-0000000000b2", "name": "Boba Dev", "role": "Dev" }
            ],
            "jobs": [{ "id": "00000000-0000-0000-0000-0000000000d1", "name": "Weekly SEO audit" }],
            "repos": [{ "id": "00000000-0000-0000-0000-0000000000e1", "name": "web-app" }],
            "tickets": [{ "id": "00000000-0000-0000-0000-0000000000f1", "number": 12, "title": "x" }]
        })
    }

    fn first_action(p: &Value) -> &Value {
        &p["actions"][0]["action"]
    }

    #[test]
    fn plans_are_valid_command_actions() {
        let d = digest();
        let cases = [
            (
                "create a job that audits SEO every monday for Pinky SEO and files tickets",
                "create_job",
            ),
            ("create an agent named Mochi to review PRs", "create_agent"),
            ("have Boba Dev review T-12", "move_ticket"),
            ("ask Boba Dev to work on ticket 12", "assign_agent"),
            ("run the job weekly seo audit", "run_job"),
            ("sync web-app", "sync_repo"),
            ("create a tab called Marketing", "create_tab"),
            ("add a ticket to fix the login redirect", "create_ticket"),
        ];
        for (text, kind) in cases {
            let p = plan(text, &d, 1);
            assert_eq!(first_action(&p)["action"], kind, "{text}: {p}");
            assert!(p["actions"][0]["label"]
                .as_str()
                .is_some_and(|l| !l.is_empty()));
            // Round-trips into the contract type.
            let a: CommandAction = serde_json::from_value(first_action(&p).clone()).unwrap();
            let _ = a;
        }
        let p = plan(
            "create a job that audits SEO every monday for Pinky SEO and files tickets",
            &d,
            1,
        );
        let job = &first_action(&p)["job"];
        assert_eq!(job["agent_id"], "00000000-0000-0000-0000-0000000000b1");
        assert_eq!(job["output"], "tickets");
        assert_eq!(job["schedule"]["expr"], "0 9 * * 1");
        let p = plan("have Boba Dev review T-12", &d, 1);
        assert_eq!(
            first_action(&p)["column_id"],
            "00000000-0000-0000-0000-0000000000c2"
        );
        assert_eq!(
            first_action(&p)["ticket_id"],
            "00000000-0000-0000-0000-0000000000f1"
        );
    }

    #[test]
    fn asks_when_unclear() {
        let p = plan("hmm", &digest(), 1);
        assert!(p["question"].is_string());
        assert_eq!(p["actions"].as_array().unwrap().len(), 0);
        let p = plan("create a job every day", &json!({}), 1);
        assert!(p["question"].as_str().unwrap().contains("agent"));
        assert!(matches!(
            schedule_for("every 2 minutes").0,
            Schedule::Interval { minutes: 5 }
        ));
        assert!(matches!(
            schedule_for("daily standup notes").0,
            Schedule::Cron { .. }
        ));
        assert!(matches!(schedule_for("once").0, Schedule::Manual));
    }
}

//! Simulated job runs (Quest Board): research through MCP connectors, then a report or
//! tickets filed through the board tools, or a PR for `JobOutput::PullRequest`.

use boba_core::automation::{JobOutput, TicketSource};
use boba_core::runner::{RunOutcome, RunSpec, RunUpdate, TicketSpec};
use boba_core::RunStatus;
use serde_json::{json, Value};

use super::{work, Ctx, Inner, Plan, Step};
use crate::emit;
use crate::util::slugify;

pub(super) async fn run(inner: &Inner, spec: &RunSpec, ctx: &mut Ctx) -> Step<RunOutcome> {
    let Some(job) = spec.job.clone() else {
        return Ok(emit::failed("job run without a job"));
    };
    if job.output == JobOutput::PullRequest {
        if spec.repo.is_none() {
            return Ok(emit::failed(format!(
                "job \"{}\" produces a pull request but has no repo",
                job.name
            )));
        }
        let t = TicketSpec {
            id: job.id,
            number: 0,
            title: job.name.clone(),
            body: job.prompt.clone(),
            tags: vec!["job".into()],
            branch: Some(format!(
                "boba/job-{}-{}",
                slugify(&job.name),
                chrono::Utc::now().format("%Y%m%d")
            )),
            pr_number: None,
            pr_url: None,
            review_feedback: None,
            review_rounds: 0,
            source: TicketSource::Manual,
        };
        let plan = Plan::new(&t.title, &t.tags, &t.body, &mut ctx.rng);
        return work(inner, spec, &t, &plan, ctx).await;
    }

    let slug = slugify(&job.name);
    let dir = match &spec.repo {
        Some(r) => r.path.clone(),
        None => inner
            .cfg
            .data_dir
            .join("scratch")
            .join(format!("job-{slug}"))
            .to_string_lossy()
            .into_owned(),
    };
    ctx.em
        .send(RunUpdate::Started {
            worktree_path: dir.clone(),
            branch: String::new(),
            session_id: Some(uuid::Uuid::now_v7().to_string()),
        })
        .await?;
    let connectors: Vec<String> = if spec.mcp_servers.is_empty() {
        vec!["search_console".into(), "web_search".into()]
    } else {
        spec.mcp_servers.iter().map(|m| m.name.clone()).collect()
    };
    let board = spec.board_tools.is_some() || job.output == JobOutput::Tickets;
    ctx.em
        .system(format!(
            "Quest \"{}\" · output {} · connectors: {} · board tools {}",
            job.name,
            match job.output {
                JobOutput::Report => "report",
                JobOutput::Tickets => "tickets",
                JobOutput::PullRequest => "pull request",
            },
            connectors.join(", "),
            if board { "on" } else { "off" }
        ))
        .await?;
    ctx.em.progress(0.04, "Reading the quest").await?;
    ctx.pause(900).await?;
    let mut intro = format!(
        "Starting \"{}\". {}",
        job.name,
        emit::first_line(&job.prompt, 160)
    );
    if let Some(prev) = &job.previous_result {
        intro.push_str(&format!(
            "\n\nLast run reported: \"{}\". I'll focus on what changed since then.",
            emit::first_line(prev, 120)
        ));
    }
    ctx.em.text(intro).await?;
    ctx.spend(7_000, 150).await?;

    let n = connectors.len().max(1) as f32;
    let mut findings = Vec::new();
    for (i, name) in connectors.iter().enumerate() {
        let p = 0.1 + 0.45 * (i as f32 / n);
        let (tool, input, output, finding) = fake_call(name, &job.prompt, &mut ctx.rng);
        ctx.em
            .progress(p, format!("Querying {}", name.replace('_', " ")))
            .await?;
        let full = format!("mcp__{name}__{tool}");
        ctx.tool(&full, input, 1_400, &output, false).await?;
        findings.push(finding);
    }

    ctx.em
        .thinking(format!(
            "Cross-checking the {} source(s): the strongest signal is \"{}\". I should turn it into \
             something actionable and skip anything already on the board.",
            connectors.len(),
            findings.first().cloned().unwrap_or_default()
        ))
        .await?;
    ctx.pause(1_300).await?;

    let mut created = Vec::new();
    if job.output == JobOutput::Tickets {
        if spec.board_tools.is_none() {
            ctx.em
                .system("Board tools are not enabled for this agent: reporting instead of filing tickets")
                .await?;
        } else {
            ctx.em.progress(0.65, "Checking the board").await?;
            ctx.tool(
                "mcp__boba__list_tickets",
                json!({ "text": slug.replace('-', " "), "limit": 20 }),
                600,
                "[]",
                false,
            )
            .await?;
            for (i, f) in findings.iter().enumerate() {
                ctx.em
                    .progress(0.7 + 0.08 * i as f32, "Filing tickets")
                    .await?;
                let number = 500 + ctx.rng.range(0, 400) as i64;
                let input = json!({
                    "title": f,
                    "body": format!("Found by the quest \"{}\".\n\n{}", job.name, f),
                    "tags": [slug.split('-').next().unwrap_or("job")],
                    "priority": if i == 0 { "high" } else { "normal" },
                });
                let output = json!({ "id": uuid::Uuid::now_v7(), "number": number }).to_string();
                ctx.tool("mcp__boba__create_ticket", input, 700, &output, false)
                    .await?;
                created.push(format!("T-{number} {f}"));
            }
        }
    }

    ctx.em.progress(0.95, "Writing the report").await?;
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let mut report = format!("## {} · {date}\n\n", job.name);
    for f in &findings {
        report.push_str(&format!("- {f}\n"));
    }
    if !created.is_empty() {
        report.push_str("\nTickets filed:\n");
        for c in &created {
            report.push_str(&format!("- {c}\n"));
        }
    }
    ctx.em.text(report.clone()).await?;
    ctx.spend(3_500, 400).await?;
    ctx.em.progress(1.0, "Quest complete").await?;
    Ok(RunOutcome {
        summary: Some(report),
        ..emit::outcome(RunStatus::Succeeded)
    })
}

/// (tool, input, output, one-line finding) for a connector, by name.
fn fake_call(
    name: &str,
    prompt: &str,
    rng: &mut crate::util::Rng,
) -> (&'static str, Value, String, String) {
    let n = name.to_ascii_lowercase();
    let pct = rng.range(12, 48);
    if n.contains("search_console") || n.contains("gsc") {
        let rows = json!({ "rows": [
            { "keys": ["/pricing"], "clicks": 412, "impressions": 18_230, "ctr": 0.022, "position": 8.4 },
            { "keys": ["/docs/getting-started"], "clicks": 1_021, "impressions": 9_870, "ctr": 0.103, "position": 3.1 },
            { "keys": ["/blog/launch"], "clicks": 37, "impressions": 6_400, "ctr": 0.006, "position": 14.9 },
        ]});
        (
            "query",
            json!({ "siteUrl": "sc-domain:example.com", "startDate": "28daysAgo", "endDate": "today", "dimensions": ["page"], "rowLimit": 25 }),
            serde_json::to_string_pretty(&rows).unwrap_or_default(),
            format!("/blog/launch has 6.4k impressions but a 0.6% CTR: rewrite its title and meta description (-{pct}% clicks vs last period)"),
        )
    } else if n.contains("sentry") {
        (
            "list_issues",
            json!({ "query": "is:unresolved", "sort": "freq", "limit": 10 }),
            json!([{ "id": "4512", "title": "TypeError: Cannot read properties of undefined (reading 'id')", "count": 1_284, "culprit": "src/api/client.ts" }]).to_string(),
            "TypeError in src/api/client.ts fired 1,284 times this week: guard the missing id".to_string(),
        )
    } else if n.contains("linear") {
        (
            "list_issues",
            json!({ "filter": { "state": "Backlog" }, "first": 20 }),
            json!({ "issues": [{ "identifier": "ENG-212", "title": "Flaky checkout test", "priority": 2 }] }).to_string(),
            "ENG-212 \"Flaky checkout test\" has been in the backlog for 3 weeks".to_string(),
        )
    } else if n.contains("github") {
        (
            "search_issues",
            json!({ "q": "is:open label:bug sort:reactions-+1-desc" }),
            json!({ "total_count": 7, "items": [{ "number": 88, "title": "Dark mode resets on reload", "reactions": 14 }] }).to_string(),
            "Issue #88 \"Dark mode resets on reload\" is the most upvoted open bug (14 reactions)".to_string(),
        )
    } else if n.contains("search") || n.contains("web") {
        let q = emit::first_line(prompt, 60);
        (
            "search",
            json!({ "query": q, "count": 5 }),
            json!({ "results": [{ "title": "What changed in 2026", "url": "https://example.org/changes" }] }).to_string(),
            format!("Two competitors shipped similar features this month; our docs page ranks below both for \"{q}\""),
        )
    } else if n.contains("postgres") || n.contains("db") {
        (
            "query",
            json!({ "sql": "select count(*) from signups where created_at > now() - interval '7 days'" }),
            json!([{ "count": 318 }]).to_string(),
            format!("318 signups in the last 7 days ({pct}% week over week)"),
        )
    } else {
        (
            "list",
            json!({}),
            json!({ "items": 3 }).to_string(),
            format!("{name}: 3 new items since the last run"),
        )
    }
}

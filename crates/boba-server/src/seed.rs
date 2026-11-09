//! First-run content: the demo workspace created by `POST /api/auth/setup`.

use boba_core::*;
use chrono::Utc;

use crate::db::new_id;
use crate::skills::write_boba_skill;
use crate::state::SharedState;

/// Default column colours by kind.
pub fn column_color(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Backlog => "#9aa5b1",
        ColumnKind::Todo => "#f6c453",
        ColumnKind::Doing => "#5fa8ff",
        ColumnKind::Review => "#b78cf2",
        ColumnKind::Done => "#5cc98b",
        ColumnKind::Custom => "#e8a87c",
    }
}

/// Todo, Doing, PR Review (auto reviewer), Done.
pub fn default_columns(tab_id: Id, reviewer: Option<Id>) -> Vec<Column> {
    [
        ("Todo", ColumnKind::Todo),
        ("Doing", ColumnKind::Doing),
        ("PR Review", ColumnKind::Review),
        ("Done", ColumnKind::Done),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, kind))| Column {
        id: new_id(),
        tab_id,
        name: name.into(),
        kind,
        color: column_color(kind).into(),
        position: i as i32,
        auto_reviewer_agent_id: if kind == ColumnKind::Review {
            reviewer
        } else {
            None
        },
        on_approve_column_id: None,
        on_changes_column_id: None,
    })
    .collect()
}

struct SkillSeed {
    name: &'static str,
    description: &'static str,
    body: &'static str,
}

const SKILLS: &[SkillSeed] = &[
    SkillSeed {
        name: "review",
        description: "Review a pull request for correctness, tests and clarity, then give a clear verdict.",
        body: "# Pull request review\n\n1. Read the ticket to understand the intent.\n2. Read the full diff (`gh pr diff` or `git diff <base>...HEAD`).\n3. Check, in order: correctness and edge cases, security (injection, secrets, auth), tests that cover the change, readability and naming, scope creep.\n4. Only request changes for problems that matter; say exactly what to change and where (`file:line`).\n5. Approve when the change does what the ticket asks and nothing is broken. Small style nits are not a reason to block.\n\nEnd with the `boba-verdict` block described in your instructions.",
    },
    SkillSeed {
        name: "frontend",
        description: "Build accessible, responsive UI with clean components and no layout regressions.",
        body: "# Frontend work\n\n- Match the existing component patterns and styling system before adding new ones.\n- Every interactive element is keyboard reachable and has an accessible name.\n- Check narrow (375px) and wide layouts.\n- Keep components small; lift state only as far as needed.\n- Run the project's lint, typecheck and build before committing.",
    },
    SkillSeed {
        name: "testing",
        description: "Write focused automated tests that fail before the fix and pass after it.",
        body: "# Testing\n\n- Reproduce the bug or specify the feature with a failing test first.\n- Test behaviour, not implementation details.\n- Cover the edge cases named in the ticket plus empty, large and invalid inputs.\n- Keep tests deterministic: no sleeps, no real network, fixed seeds and clocks.\n- Run the whole suite before you finish and report the result.",
    },
    SkillSeed {
        name: "architecture",
        description: "Design changes that fit the codebase: clear boundaries, small interfaces, documented trade-offs.",
        body: "# Architecture\n\n- Read the existing architecture docs and the modules you touch before designing.\n- Prefer extending an existing abstraction over adding a parallel one.\n- Write down the trade-off you chose in the PR description (2-4 sentences).\n- Keep public interfaces small and stable; additive changes over breaking ones.\n- Split large changes into reviewable steps.",
    },
    SkillSeed {
        name: "seo",
        description: "Improve search and answer-engine visibility: metadata, structured data, content and performance.",
        body: "# SEO\n\n- Every page has a unique `<title>` (50-60 chars) and meta description (120-160 chars).\n- One `<h1>` per page, logical heading order.\n- Add JSON-LD structured data where it fits (Organization, Article, FAQ, Product).\n- Canonical URLs, sitemap and robots.txt stay consistent.\n- Images have descriptive alt text and explicit sizes; avoid layout shift.\n- Content answers the query in the first paragraph.",
    },
    SkillSeed {
        name: "devops",
        description: "Ship reliable infrastructure: reproducible builds, safe deploys, observability and least privilege.",
        body: "# DevOps\n\n- Infrastructure changes are code, reviewed like code.\n- Never commit secrets; read them from the environment or a secret manager.\n- Deploys are reversible: document the rollback.\n- Add health checks, structured logs and an alert for anything that can page someone.\n- Pin versions of images, actions and tools.",
    },
];

struct AgentSeed {
    name: &'static str,
    role: &'static str,
    sprite: &'static str,
    color: &'static str,
    tagline: &'static str,
    prompt: &'static str,
    skills: &'static [&'static str],
    mode: PermissionMode,
}

const AGENTS: &[AgentSeed] = &[
    AgentSeed {
        name: "Boba Dev",
        role: "Dev",
        sprite: "boba",
        color: "#c98b5a",
        tagline: "One more pearl, one more feature.",
        prompt: "You are Boba Dev, a pragmatic senior software engineer on a small team.\n\nWork style:\n- Read the relevant code before changing it and follow the conventions you find.\n- Make the smallest change that fully solves the ticket; no drive-by refactors.\n- Add or update tests for what you change and run the project's checks before finishing.\n- Write clear, imperative commit messages.\n- Finish with a short summary: what changed, why, and how you verified it.",
        skills: &["frontend", "testing"],
        mode: PermissionMode::AcceptEdits,
    },
    AgentSeed {
        name: "Uncle Review",
        role: "Reviewer",
        sprite: "boba_uncle",
        color: "#8e6bbf",
        tagline: "Seen it all. Approves the good stuff.",
        prompt: "You are Uncle Review, a calm and experienced code reviewer.\n\nYou never modify files. You read the ticket and the pull request diff, check correctness, security, tests and clarity, and give actionable feedback with file:line references. Be kind and specific. Do not block on taste; block on bugs, missing tests for risky logic, security problems or work that does not match the ticket.\n\nAlways end your final message with exactly one fenced block:\n```boba-verdict\n{\"verdict\": \"approved\" | \"changes_requested\", \"feedback\": \"<what to fix, or why it is good>\"}\n```",
        skills: &["review"],
        mode: PermissionMode::Plan,
    },
    AgentSeed {
        name: "Pinky SEO",
        role: "SEO",
        sprite: "pinky_boba",
        color: "#f28cb1",
        tagline: "Rank first, sip later.",
        prompt: "You are Pinky SEO, a technical SEO and content specialist.\n\nYou improve how pages are understood by search engines and answer engines: titles, meta descriptions, headings, structured data, internal links, sitemaps and page speed. You keep copy accurate and human; never stuff keywords. Explain in your summary which pages changed and what each change is expected to improve.",
        skills: &["seo", "frontend"],
        mode: PermissionMode::AcceptEdits,
    },
    AgentSeed {
        name: "Diamond Architect",
        role: "Architect",
        sprite: "boba_diamond",
        color: "#5fc6e8",
        tagline: "Strong foundations, shiny results.",
        prompt: "You are Diamond Architect, a staff engineer who designs before building.\n\nFor every ticket: understand the existing architecture, propose the simplest design that fits it, then implement it in small, well-named steps. Keep interfaces minimal, document trade-offs in the PR description, and leave the codebase easier to change than you found it.",
        skills: &["architecture", "testing"],
        mode: PermissionMode::AcceptEdits,
    },
    AgentSeed {
        name: "Golden QA",
        role: "QA",
        sprite: "golden_boba",
        color: "#f2c94c",
        tagline: "If it can break, I will find it.",
        prompt: "You are Golden QA, a meticulous quality engineer.\n\nYou reproduce bugs, write the failing test first, fix the root cause (not the symptom) and expand coverage around fragile areas. You think about edge cases: empty input, huge input, concurrency, time zones, permissions. Report exactly what you tested and the results.",
        skills: &["testing"],
        mode: PermissionMode::AcceptEdits,
    },
    AgentSeed {
        name: "Black Ops",
        role: "DevOps",
        sprite: "black_boba",
        color: "#4a4a5a",
        tagline: "Silent deploys. Zero downtime.",
        prompt: "You are Black Ops, a careful DevOps and infrastructure engineer.\n\nYou work on CI, build scripts, containers, deploy configs and observability. You never print or commit secrets, you pin versions, and every change you make is reversible: describe the rollback in your summary. Prefer boring, well-documented tools over clever ones.",
        skills: &["devops"],
        mode: PermissionMode::AcceptEdits,
    },
];

struct TicketSeed {
    tab: usize,
    title: &'static str,
    body: &'static str,
    tags: &'static [&'static str],
    priority: Priority,
}

const TICKETS: &[TicketSeed] = &[
    TicketSeed {
        tab: 0,
        title: "Add a dark mode toggle to the settings page",
        body: "Users want a dark theme.\n\n- Add a toggle in Settings that switches between light and dark.\n- Remember the choice in localStorage and respect `prefers-color-scheme` by default.\n- No flash of the wrong theme on load.",
        tags: &["frontend"],
        priority: Priority::Normal,
    },
    TicketSeed {
        tab: 0,
        title: "Fix: login form accepts empty password",
        body: "Submitting the login form with an empty password sends a request and shows a generic error.\n\nValidate on the client, show an inline message, and add a test.",
        tags: &["testing", "bug"],
        priority: Priority::High,
    },
    TicketSeed {
        tab: 0,
        title: "Split the API client into one module per resource",
        body: "`api.ts` is 1,200 lines. Split it into `tickets.ts`, `agents.ts`, `auth.ts` with a shared `request()` helper. No behaviour change.",
        tags: &["architecture"],
        priority: Priority::Low,
    },
    TicketSeed {
        tab: 0,
        title: "Write tests for the date formatting helpers",
        body: "`formatRelative` and `formatDuration` have no tests. Cover past/future, zero, very large values and time zones.",
        tags: &["testing"],
        priority: Priority::Normal,
    },
    TicketSeed {
        tab: 1,
        title: "Unique titles and meta descriptions for every docs page",
        body: "Half of the docs pages share the same `<title>`. Give each page a unique title (50-60 chars) and description (120-160 chars).",
        tags: &["seo"],
        priority: Priority::High,
    },
    TicketSeed {
        tab: 1,
        title: "Add FAQ structured data to the pricing page",
        body: "The pricing page has an FAQ section. Mark it up with `FAQPage` JSON-LD and validate it.",
        tags: &["seo"],
        priority: Priority::Normal,
    },
    TicketSeed {
        tab: 2,
        title: "Cache dependencies in CI",
        body: "CI reinstalls everything on every push (6 min). Add dependency caching keyed on the lockfile.",
        tags: &["devops"],
        priority: Priority::Normal,
    },
    TicketSeed {
        tab: 2,
        title: "Add a /healthz endpoint and a container healthcheck",
        body: "The load balancer needs a cheap health endpoint. Add `/healthz` returning 200 with the version, and a `HEALTHCHECK` in the Dockerfile.",
        tags: &["devops"],
        priority: Priority::Urgent,
    },
];

/// Create skills, agents, tabs and sample tickets. Idempotent-ish: only runs on setup.
pub async fn seed_demo(state: &SharedState, owner: Id) -> anyhow::Result<()> {
    let now = Utc::now();
    let dir = state.cfg.skills_dir();
    std::fs::create_dir_all(&dir)?;

    let mut skills: Vec<Skill> = Vec::new();
    for s in SKILLS {
        let path = write_boba_skill(&dir, s.name, s.description, s.body)?;
        let sk = state
            .db
            .upsert_skill(&Skill {
                id: new_id(),
                name: s.name.into(),
                description: s.description.into(),
                source: SkillSource::Boba,
                path: path.to_string_lossy().to_string(),
                editable: true,
            })
            .await?;
        skills.push(sk);
    }

    let mut agent_ids = Vec::new();
    for a in AGENTS {
        let agent = Agent {
            id: new_id(),
            name: a.name.into(),
            role: a.role.into(),
            sprite: a.sprite.into(),
            color: a.color.into(),
            tagline: a.tagline.into(),
            system_prompt: a.prompt.into(),
            skill_ids: a
                .skills
                .iter()
                .filter_map(|n| skills.iter().find(|s| s.name == *n).map(|s| s.id))
                .collect(),
            model: None,
            permission_mode: a.mode,
            allowed_tools: Vec::new(),
            max_turns: None,
            xp: 0,
            level: 1,
            runs_completed: 0,
            prs_approved: 0,
            status: AgentStatus::Idle,
            current_run_id: None,
            connector_ids: Vec::new(),
            board_tools: false,
            created_by: Some(owner),
            created_at: now,
        };
        state.db.save_agent(&agent).await?;
        agent_ids.push(agent.id);
    }
    let reviewer = agent_ids.get(1).copied();

    let mut tabs = Vec::new();
    for (i, (name, icon, color)) in [
        ("Dev", "🧋", "#c98b5a"),
        ("SEO", "🔎", "#f28cb1"),
        ("Infra", "🛠️", "#4a4a5a"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = new_id();
        let tab = Tab {
            id,
            name: name.into(),
            icon: icon.into(),
            color: color.into(),
            position: i as i32,
            repo_id: None,
            columns: default_columns(id, reviewer),
        };
        state.db.save_tab(&tab).await?;
        tabs.push(tab);
    }

    for (n, t) in TICKETS.iter().enumerate() {
        let tab = &tabs[t.tab];
        let todo = &tab.columns[0];
        let ticket = Ticket {
            id: new_id(),
            number: state.db.next_ticket_number().await?,
            tab_id: tab.id,
            column_id: todo.id,
            position: (n as f64 + 1.0) * 1024.0,
            title: t.title.into(),
            body: t.body.into(),
            tags: t.tags.iter().map(|s| s.to_string()).collect(),
            priority: t.priority,
            color: None,
            repo_id: None,
            agent_id: None,
            branch: None,
            pr_url: None,
            pr_number: None,
            review_feedback: None,
            review_rounds: 0,
            created_by: Some(owner),
            created_at: now,
            updated_at: now,
            source: Default::default(),
            // Maintained by the comments feature (server workstream).
            comment_count: 0,
            open_thread_count: 0,
        };
        state.db.save_ticket(&ticket).await?;
    }
    Ok(())
}

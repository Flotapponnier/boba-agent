# Architecture

Boba Agent is a single self-hosted binary (`boba-agent`) that serves a web UI and runs
Claude Code agents for a whole team.

```
 browser(s) ──HTTP/WS──▶ boba-server (axum) ──▶ SQLite (data/boba.db)
                           │   ▲
                 RunSpec   │   │ RunUpdate stream
                           ▼   │
                     Runner trait (boba-runner)
                     ├── ClaudeCodeRunner  → `claude -p --output-format stream-json` in a git worktree
                     ├── DemoRunner        → simulated runs (no API key needed)
                     └── RemoteRunner      → same messages over WS to another server (planned)
```

## Crates

| Crate | Role | Owner (see CONTRIBUTING.md) |
|---|---|---|
| `boba-core` | Shared contract: domain model, REST DTOs, WS protocol, runner protocol. Generates TS types into `web/src/api/generated/` via ts-rs. | coordinator |
| `boba-server` | axum REST + WebSocket, SQLite (sqlx, migrations), auth + RBAC, orchestration state machine, history log, embeds `web/dist` (rust-embed) | server |
| `boba-runner` | `Runner` trait + Claude Code runner + demo runner, git worktrees, PRs via `gh`, skill discovery | runner |
| `web/` | React + TypeScript + Vite UI: the game board | web |

## Core flows

### 1. Drag an agent onto a ticket
1. UI: `POST /api/tickets/:id/assign {agent_id}` (needs `RunAgents`).
2. Server: creates `Run{kind: Work, status: Queued}`, moves the ticket to the tab's `Doing`
   column, sets agent `status = Working`, records `HistoryKind::RunStarted`, broadcasts upserts.
3. Server builds a `RunSpec` (agent persona, ticket, repo, skill paths) and calls `Runner::start`.
4. Runner: `git worktree add <data>/worktrees/T-<n> -b boba/T-<n>-<slug>` from the default branch,
   then spawns Claude Code headless:
   `claude -p --output-format stream-json --verbose --append-system-prompt <persona>
    --permission-mode <mode> [--model] [--allowedTools] [--max-turns] [--resume <session>]`
   with the task prompt on stdin, `cwd` = the worktree.
5. Runner parses stream-json into `RunUpdate::Log/Progress/Usage`, the server persists every
   `RunLogLine`, updates `Run.progress/activity`, and fans out to subscribed sockets.
6. On success the runner commits, pushes and opens a PR (`gh pr create`) → `PrOpened`, then
   `Finished{Succeeded}`. Server moves the ticket to the Review column, gives XP, emits
   `Fx::WorkFinished` (+ `Fx::LevelUp`).

### 2. Auto review (the PR column)
Entering a `ColumnKind::Review` column whose `auto_reviewer_agent_id` is set and idle starts a
`Run{kind: Review}`: read-only permissions (`plan` mode, allowed tools limited to reading and
`git diff` / `gh pr diff` / `gh pr review`), the reviewer's skills loaded. The reviewer must end
its final message with a fenced block:

````
```boba-verdict
{"verdict": "approved" | "changes_requested", "feedback": "…"}
```
````

Approved → ticket to `on_approve_column_id` (default first Done column), `Fx::PrApproved`.
Changes requested → `review_feedback` stored, `review_rounds += 1`, ticket to
`on_changes_column_id` (default first Todo). If the reviewer is busy, the ticket waits in the
column and is picked up when the reviewer becomes idle (queue by time of entry).

### 3. Skills & personas
An agent is a Claude Code persona: `system_prompt` is appended to Claude Code's system prompt,
and its `skill_ids` decide which skill folders are exposed. The runner exposes only those skills
by writing them (symlinks) into the worktree's `.claude/skills/` and running with
`--setting-sources project` so the user's personal config does not leak between teammates.
Boba-managed skills live in `<data>/skills/<name>/SKILL.md` and are editable from the UI;
discovered user/project/plugin skills are read-only.

## Credentials and terms
Shared servers must use an **organisation API key** (`ANTHROPIC_API_KEY`) or a cloud provider
(Bedrock/Vertex). Do not put one person's Claude Pro/Max login on a shared server: Anthropic's
terms reserve subscriptions for individual use. Without a key Boba Agent starts in **demo mode**
(`DemoRunner`) so anyone can try the UI.

## Security defaults
- Every run in its own worktree; never in the main checkout.
- Per-run `CLAUDE_CONFIG_DIR` under `<data>/runs/<id>/claude` and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`.
- `bypassPermissions` only selectable by Admins, and only if the server runs inside a container
  (`BOBA_ALLOW_BYPASS=1`).
- Reviews are read-only.
- Ticket bodies, PR content and tool output are untrusted input (prompt injection): reviewers never
  get write tools; work runs never get secrets other than the API key and `GH_TOKEN`.

## Data
SQLite in `BOBA_DATA_DIR` (default `./data`): users, sessions, tabs, columns, agents, skills, repos,
tickets, runs, run_logs, history. Run logs are append-only; history is append-only.

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
and its `skill_ids` decide which skill folders are exposed. The runner builds a per-run plugin
directory with symlinks to those skills and passes `--plugin-dir` (they appear as `boba:<skill>`),
so nothing is written into the worktree (it would get committed). Runs use
`--setting-sources project --strict-mcp-config`, which hides the server user's own skills, plugins,
hooks and MCP servers. Boba-managed skills live in `<data>/skills/<name>/SKILL.md` and are editable
from the UI; discovered user/project/plugin skills are read-only.

## Credentials and terms
Shared servers must use an **organisation API key** (`ANTHROPIC_API_KEY`) or a cloud provider
(Bedrock/Vertex). Do not put one person's Claude Pro/Max login on a shared server: Anthropic's
terms reserve subscriptions for individual use. Without a key Boba Agent starts in **demo mode**
(`DemoRunner`) so anyone can try the UI.

## Security defaults
- Every run in its own worktree; never in the main checkout.
- Per-**agent** `CLAUDE_CONFIG_DIR` under `<data>/agents/<id>/claude` (per-run would break `--resume`)
  and `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`. A fresh config dir has no OAuth login, so runs need
  `ANTHROPIC_API_KEY` (or Bedrock/Vertex).
- Headless sessions can reach other local Claude sessions, so runs pass `--disallowedTools` for
  SendMessage, ListAgents, Cron*, ScheduleWakeup, RemoteTrigger, PushNotification, EnterWorktree,
  ExitWorktree.
- `bypassPermissions` only selectable by Admins, and only if the server runs inside a container
  (`BOBA_ALLOW_BYPASS=1`).
- Reviews are read-only.
- Ticket bodies, PR content and tool output are untrusted input (prompt injection): reviewers never
  get write tools; work runs never get secrets other than the API key and `GH_TOKEN`.

## Data
SQLite in `BOBA_DATA_DIR` (default `./data`): users, sessions, tabs, columns, agents, skills, repos,
tickets, runs, run_logs, history. Run logs are append-only; history is append-only.

## Integrations, connectors and jobs

Types live in `crates/boba-core/src/automation.rs`.

### Repo sync (GitHub)
Each `Repo` has a `RepoSync`. When enabled, the server syncs every `poll_minutes` (and on
`POST /api/repos/:id/sync`, and on GitHub webhooks if `BOBA_GITHUB_WEBHOOK_SECRET` is set):
- open issues (optionally filtered by label) → tickets in the target tab's first Todo column,
  `source = GithubIssue`; title/body kept in sync while the ticket is not running;
- open PRs (incl. external contributors, drafts skipped by default) → tickets in the target tab's
  Review column, `source = GithubPr`, so the column's auto reviewer reviews them. Review runs on an
  imported PR check out `head_branch`;
- back to GitHub: run summaries and verdicts as comments (`comment_back`), real PR reviews
  (`post_reviews`), issue closed when the ticket reaches Done (`close_issues_on_done`).
Imports are idempotent: (repo_id, kind, number) is unique. GitHub access uses `GH_TOKEN` /
`gh auth` on the server (a GitHub App is a later option).

### Connectors (MCP)
A `Connector` is an MCP server (stdio command or remote HTTP) with write-only secrets stored
encrypted (`BOBA_SECRET_KEY`, AES-GCM) in SQLite. Presets in the UI (Google Workspace, Search
Console, Linear, Slack, Sentry, Notion, web search, Postgres, custom) only pre-fill the transport.
Agents list the connectors they may use (`Agent.connector_ids`). For each run the server resolves
secrets into `RunSpec.mcp_servers`; the runner writes a per-run `--mcp-config` file (0600, deleted
after the run) and adds the allowed tools as `mcp__<name>__<tool>` to `--allowedTools`.

### Board tools (the server's own MCP endpoint)
`/api/mcp` is a streamable-HTTP MCP server authenticated with a per-run bearer token
(`RunSpec.board_tools`, revoked when the run ends). Tools: `list_tabs`, `list_tickets`
(filters: tab, column kind, tag, text), `get_ticket`, `create_ticket` (tab, title, body, tags,
priority), `update_ticket`, `comment_ticket`, `search_history`. Everything an agent does through
it is attributed to the agent in the history (`ActorKind::Agent`) and tickets get
`source = Agent | Job`. Agents with `board_tools = true` get it; this is how a "Triage" agent turns
Search Console / Sentry / Linear data into tickets.

### Scheduled jobs (Quest Board)
A `Job` = agent + prompt + `Schedule` (manual, interval ≥ 5 min, or cron + IANA timezone) +
`JobOutput` (tickets via board tools, a stored report, or a PR in `repo_id`). The server scheduler
ticks every 15 s, computes `next_run_at`, and starts a `Run{kind: Job, ticket_id: None, job_id}`;
`skip_if_running` avoids overlaps; missed ticks while the server was down run once at startup.
"Run now" = `POST /api/jobs/:id/run`. A job run's report is stored in `Job.last_result` and passed to
the next run as `previous_result` so recurring jobs don't repeat themselves.

### Command bar & voice (the Concierge)
`⌘K` / `Ctrl+K` opens the command bar; holding `Space` (when no input is focused) or the mic button
records a voice command. Speech-to-text is `WorkspaceSettings.speech`: the browser's Web Speech API
by default, or a self-hosted Whisper-compatible endpoint behind `POST /api/speech/transcribe`.

`POST /api/command` starts a read-only `Run{kind: Command}` of the **Concierge** (a built-in agent).
Its `RunSpec.command` carries the sentence, the UI context (current tab / ticket / agent) and a board
digest so it can resolve "Pinky", "the SEO tab" or "this ticket" to ids. It must end with a
```` ```boba-plan ```` JSON block → the server validates it into a `CommandPlan`, marks each action
`allowed` against **the requesting user's** permissions, and sends `ServerMessage::CommandPlan` to that
user only. The UI shows one confirmation card per action (editable); nothing happens until the user
confirms via `POST /api/command/execute`, which runs the actions with the user's own permissions and
records `CommandIssued` / `CommandExecuted` in the history. The concierge never gets write tools, so a
prompt-injected sentence cannot act on its own.

### Board digest (`CommandSpec.board_digest`)
Built by the server, parsed leniently by runners:
```json
{
  "tabs":    [{"id": "…", "name": "Dev", "columns": [{"id": "…", "name": "PR Review", "kind": "review", "auto_reviewer_agent_id": "…"}]}],
  "agents":  [{"id": "…", "name": "Pinky SEO", "role": "SEO"}],
  "jobs":    [{"id": "…", "name": "Weekly SEO audit"}],
  "repos":   [{"id": "…", "name": "openchainbench"}],
  "tickets": [{"id": "…", "number": 12, "title": "…", "tab_id": "…"}],
  "connectors": [{"id": "…", "name": "Search Console", "kind": "google_search_console"}],
  "skills":  [{"id": "…", "name": "seo-audit"}]
}
```
UI context is in `CommandSpec.tab_id/ticket_id/agent_id` (runners may also accept a legacy
`board_digest.context` object). Tickets: the 200 most recently updated.

## Claude Code CLI notes (2.1.319, verified)
- `--max-turns`, `--permission-prompt-tool stdio` and `--permission-mode default` work though hidden in `--help`.
- Permission prompts need `--input-format stream-json --permission-prompt-tool stdio`: stdout emits
  `control_request {subtype: can_use_tool, tool_name, input, tool_use_id}`, answered on stdin with
  `control_response {behavior: allow, updatedInput}` or `{behavior: deny, message}`. Without it,
  prompts are silently denied.
- Messages sent mid-run merge into the current turn (one `result` per batch); `--replay-user-messages`
  echoes consumed messages with `isReplay: true`.
- A `result` with `subtype: success` can still have `is_error: true` (e.g. "Not logged in").
- `--allowedTools "Read,Grep,Bash(git diff:*)"` as one comma-joined argument works.

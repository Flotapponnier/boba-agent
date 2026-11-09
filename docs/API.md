# HTTP & WebSocket API

All bodies are JSON and typed in `crates/boba-core/src/api.rs` (TS: `web/src/api/generated/`).
Auth is a `boba_session` HttpOnly cookie. Errors: `ApiError {error, code}` with 400/401/403/404/409.
Required permission in brackets (see `Permission` in `model.rs`).

## Auth
| Method | Path | Body → Response |
|---|---|---|
| GET | `/api/auth/status` | → `AuthStatus` (public) |
| POST | `/api/auth/setup` | `SetupRequest` → `User` (only when no user exists; creates Owner + seeds demo workspace) |
| POST | `/api/auth/login` | `LoginRequest` → `User` |
| POST | `/api/auth/logout` | → 204 |
| POST | `/api/users/invite` [ManageUsers] | `InviteRequest` → `InviteResponse` |
| POST | `/api/auth/accept-invite` | `{token, password}` → `User` |
| PATCH | `/api/users/:id` [ManageUsers] | `{role}` → `User` |

## State
| GET | `/api/snapshot` [View] | → `Snapshot` |
|---|---|---|
| GET | `/api/history` [View] | query `HistoryQuery` → `HistoryEvent[]` (newest first) |
| PATCH | `/api/settings` [ManageWorkspace] | partial `WorkspaceSettings` → `WorkspaceSettings` |

## Tabs / columns
| POST | `/api/tabs` [ManageWorkspace] | `CreateTab` → `Tab` |
|---|---|---|
| PATCH | `/api/tabs/:id` [ManageWorkspace] | `UpdateTab` → `Tab` |
| DELETE | `/api/tabs/:id` [ManageWorkspace] | → 204 (409 if it has running tickets) |

## Agents
| POST | `/api/agents` [ManageAgents] | `CreateAgent` → `Agent` |
|---|---|---|
| PATCH | `/api/agents/:id` [ManageAgents] | `UpdateAgent` → `Agent` |
| DELETE | `/api/agents/:id` [ManageAgents] | → 204 (409 while working) |

## Tickets
| POST | `/api/tickets` [EditTickets] | `CreateTicket` → `Ticket` |
|---|---|---|
| PATCH | `/api/tickets/:id` [EditTickets] | `UpdateTicket` → `Ticket` |
| POST | `/api/tickets/:id/move` [EditTickets] | `MoveTicket` → `Ticket` |
| POST | `/api/tickets/:id/assign` [RunAgents] | `AssignAgent` → `Run` (409 if agent busy or ticket already running) |
| POST | `/api/tickets/:id/comment` [EditTickets] | `CommentRequest` → `HistoryEvent` |
| DELETE | `/api/tickets/:id` [EditTickets] | → 204 (cancels its run) |

## Runs
| GET | `/api/runs/:id` [View] | → `Run` |
|---|---|---|
| GET | `/api/runs/:id/logs?after_seq=&limit=` [View] | → `RunLogLine[]` |
| GET | `/api/tickets/:id/runs` [View] | → `Run[]` (all rounds, newest first) |
| POST | `/api/runs/:id/cancel` [RunAgents] | → `Run` |
| POST | `/api/runs/:id/message` [RunAgents] | `RunMessage` → `Run` |
| POST | `/api/runs/:id/permission` [RunAgents] | `PermissionAnswer` → 204 |

## Skills, repos, runners
| GET | `/api/skills/:id` [View] | → `SkillDetail` |
|---|---|---|
| POST | `/api/skills` [ManageSkills] | `UpsertSkill` → `Skill` (Boba-managed) |
| PUT | `/api/skills/:id` [ManageSkills] | `UpsertSkill` → `Skill` (editable only) |
| POST | `/api/skills/rescan` [ManageSkills] | → `Skill[]` |
| POST | `/api/repos` [ManageWorkspace] | `CreateRepo` → `Repo` |
| DELETE | `/api/repos/:id` [ManageWorkspace] | → 204 |

## WebSocket `/api/ws` [View]
Server → client `ServerMessage`, client → server `ClientMessage` (see `event.rs`).
1. On connect the server sends `Snapshot`.
2. Every mutation is broadcast as `Upsert`/`Delete` + `History` (+ `Fx` for celebrations).
3. `RunLog` lines only for runs the client `Subscribe`d to (server replays the last 200).
4. Client sends `Ping` every 25 s; on reconnect send `Resync`.
5. `Presence` is rebroadcast when someone connects/disconnects or changes tab.

## Dev mode
`BOBA_DEV_CORS=http://localhost:5173` lets the Vite dev server call the API; Vite proxies
`/api` (incl. WS) to `http://localhost:7777`.

## Integrations & automation [ManageAutomations unless stated]
| Method | Path | Body → Response |
|---|---|---|
| PATCH | `/api/repos/:id` | `UpdateRepo` → `Repo` (incl. `sync`) |
| POST | `/api/repos/:id/sync` [RunAgents] | → `SyncReport` |
| POST | `/api/github/webhook` | GitHub webhook (HMAC `BOBA_GITHUB_WEBHOOK_SECRET`), public |
| POST | `/api/connectors` | `CreateConnector` → `Connector` |
| PATCH | `/api/connectors/:id` | `UpdateConnector` → `Connector` |
| DELETE | `/api/connectors/:id` | → 204 |
| POST | `/api/connectors/:id/test` | → `ConnectorTestResult` |
| POST | `/api/jobs` | `CreateJob` → `Job` |
| PATCH | `/api/jobs/:id` | `UpdateJob` → `Job` |
| DELETE | `/api/jobs/:id` | → 204 |
| POST | `/api/jobs/:id/run` [RunAgents] | → `Run` |
| GET | `/api/jobs/:id/runs` [View] | → `Run[]` |
| POST | `/api/jobs/preview-schedule` [View] | `Schedule` → `SchedulePreview` |
| POST | `/api/mcp` | MCP streamable HTTP for agents, bearer = per-run token (not a user session) |
| POST | `/api/command` [View] | `CommandRequest` → `Run` (plan arrives as `ServerMessage::CommandPlan`) |
| POST | `/api/command/execute` [per action] | `ExecuteCommand` → `ActionResult[]` |
| POST | `/api/speech/transcribe` [View] | audio body → `Transcript` (only with `SpeechProvider::Whisper`) |

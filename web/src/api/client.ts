import type {
  Agent,
  AssignAgent,
  AuthStatus,
  CreateAgent,
  CreateRepo,
  CreateTab,
  CreateTicket,
  HistoryEvent,
  HistoryQuery,
  InviteRequest,
  InviteResponse,
  LoginRequest,
  MoveTicket,
  PermissionAnswer,
  Repo,
  Run,
  RunLogLine,
  SetupRequest,
  Skill,
  SkillDetail,
  Snapshot,
  Tab,
  Ticket,
  UpdateAgent,
  UpdateTab,
  UpdateTicket,
  UpsertSkill,
  User,
  Role,
  WorkspaceSettings,
  ActionResult,
  CommandAction,
  CommandRequest,
  Connector,
  ConnectorTestResult,
  CreateConnector,
  CreateJob,
  Job,
  Schedule,
  SchedulePreview,
  SyncReport,
  Transcript,
  UpdateConnector,
  UpdateJob,
  UpdateRepo,
  Comment,
  CreateComment,
  CreateRunner,
  Notification,
  RunnerToken,
  UpdateComment,
  UpdateMe,
  GithubAppStatus,
  GithubRepo,
} from './types';
import { httpTransport, type Transport } from './transport';

let transport: Transport = httpTransport;
export const setTransport = (t: Transport) => {
  transport = t;
};
export const getTransport = () => transport;

const req = <T>(method: string, path: string, body?: unknown) => transport.request<T>(method, path, body);

function qs(q: Partial<Record<string, string | number | null | undefined>>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== null && v !== undefined && v !== '') p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : '';
}

/** `GET /api/github/app/manifest` (PR #45, not in core yet): data for the GitHub App manifest form. */
export interface GithubManifest {
  name: string;
  /** Where the browser POSTs the form (carries a fresh single-use `state`). */
  action_url: string;
  manifest: Record<string, unknown>;
  warnings: string[];
}

/** Typed REST client, one function per endpoint of docs/API.md. */
export const api = {
  authStatus: () => req<AuthStatus>('GET', '/api/auth/status'),
  setup: (b: SetupRequest) => req<User>('POST', '/api/auth/setup', b),
  login: (b: LoginRequest) => req<User>('POST', '/api/auth/login', b),
  logout: () => req<void>('POST', '/api/auth/logout'),
  invite: (b: InviteRequest) => req<InviteResponse>('POST', '/api/users/invite', b),
  acceptInvite: (token: string, password: string) => req<User>('POST', '/api/auth/accept-invite', { token, password }),
  setRole: (id: string, role: Role) => req<User>('PATCH', `/api/users/${id}`, { role }),

  snapshot: () => req<Snapshot>('GET', '/api/snapshot'),
  history: (q: Partial<HistoryQuery>) => req<HistoryEvent[]>('GET', `/api/history${qs(q)}`),
  updateSettings: (b: Partial<WorkspaceSettings>) => req<WorkspaceSettings>('PATCH', '/api/settings', b),

  createTab: (b: CreateTab) => req<Tab>('POST', '/api/tabs', b),
  updateTab: (id: string, b: Partial<UpdateTab>) => req<Tab>('PATCH', `/api/tabs/${id}`, b),
  deleteTab: (id: string) => req<void>('DELETE', `/api/tabs/${id}`),

  createAgent: (b: CreateAgent) => req<Agent>('POST', '/api/agents', b),
  updateAgent: (id: string, b: Partial<UpdateAgent>) => req<Agent>('PATCH', `/api/agents/${id}`, b),
  deleteAgent: (id: string) => req<void>('DELETE', `/api/agents/${id}`),

  createTicket: (b: CreateTicket) => req<Ticket>('POST', '/api/tickets', b),
  updateTicket: (id: string, b: Partial<UpdateTicket>) => req<Ticket>('PATCH', `/api/tickets/${id}`, b),
  moveTicket: (id: string, b: MoveTicket) => req<Ticket>('POST', `/api/tickets/${id}/move`, b),
  assign: (id: string, b: AssignAgent) => req<Run>('POST', `/api/tickets/${id}/assign`, b),
  comment: (id: string, text: string) => req<HistoryEvent>('POST', `/api/tickets/${id}/comment`, { text }),
  deleteTicket: (id: string) => req<void>('DELETE', `/api/tickets/${id}`),
  ticketRuns: (id: string) => req<Run[]>('GET', `/api/tickets/${id}/runs`),

  run: (id: string) => req<Run>('GET', `/api/runs/${id}`),
  runLogs: (id: string, afterSeq?: number, limit = 500) =>
    req<RunLogLine[]>('GET', `/api/runs/${id}/logs${qs({ after_seq: afterSeq, limit })}`),
  cancelRun: (id: string) => req<Run>('POST', `/api/runs/${id}/cancel`),
  messageRun: (id: string, text: string) => req<Run>('POST', `/api/runs/${id}/message`, { text }),
  answerPermission: (id: string, b: PermissionAnswer) => req<void>('POST', `/api/runs/${id}/permission`, b),

  skill: (id: string) => req<SkillDetail>('GET', `/api/skills/${id}`),
  createSkill: (b: UpsertSkill) => req<Skill>('POST', '/api/skills', b),
  updateSkill: (id: string, b: UpsertSkill) => req<Skill>('PUT', `/api/skills/${id}`, b),
  rescanSkills: () => req<Skill[]>('POST', '/api/skills/rescan'),

  createRepo: (b: CreateRepo) => req<Repo>('POST', '/api/repos', b),
  deleteRepo: (id: string) => req<void>('DELETE', `/api/repos/${id}`),
  updateRepo: (id: string, b: Partial<UpdateRepo>) => req<Repo>('PATCH', `/api/repos/${id}`, b),
  syncRepo: (id: string) => req<SyncReport>('POST', `/api/repos/${id}/sync`),

  createConnector: (b: CreateConnector) => req<Connector>('POST', '/api/connectors', b),
  updateConnector: (id: string, b: Partial<UpdateConnector>) => req<Connector>('PATCH', `/api/connectors/${id}`, b),
  deleteConnector: (id: string) => req<void>('DELETE', `/api/connectors/${id}`),
  testConnector: (id: string) => req<ConnectorTestResult>('POST', `/api/connectors/${id}/test`),

  createJob: (b: CreateJob) => req<Job>('POST', '/api/jobs', b),
  updateJob: (id: string, b: Partial<UpdateJob>) => req<Job>('PATCH', `/api/jobs/${id}`, b),
  deleteJob: (id: string) => req<void>('DELETE', `/api/jobs/${id}`),
  runJob: (id: string) => req<Run>('POST', `/api/jobs/${id}/run`),
  jobRuns: (id: string) => req<Run[]>('GET', `/api/jobs/${id}/runs`),
  previewSchedule: (s: Schedule) => req<SchedulePreview>('POST', '/api/jobs/preview-schedule', s),

  command: (b: CommandRequest) => req<Run>('POST', '/api/command', b),
  executeCommand: (runId: string, actions: CommandAction[]) => req<ActionResult[]>('POST', '/api/command/execute', { run_id: runId, actions }),
  transcribe: (audio: Blob) => transport.upload<Transcript>('/api/speech/transcribe', audio),

  updateMe: (b: Partial<UpdateMe>) => req<User>('PATCH', '/api/me', b),
  comments: (ticketId: string) => req<Comment[]>('GET', `/api/tickets/${ticketId}/comments`),
  createComment: (b: CreateComment) => req<Comment>('POST', '/api/comments', b),
  updateComment: (id: string, b: Partial<UpdateComment>) => req<Comment>('PATCH', `/api/comments/${id}`, b),
  deleteComment: (id: string) => req<void>('DELETE', `/api/comments/${id}`),
  react: (id: string, emoji: string) => req<Comment>('POST', `/api/comments/${id}/react`, { emoji }),
  notifications: (before?: string, limit = 50) => req<Notification[]>('GET', `/api/notifications${qs({ before, limit })}`),
  markRead: (ids: string[] | null) => req<void>('POST', '/api/notifications/read', { ids }),
  createRunner: (b: CreateRunner) => req<RunnerToken>('POST', '/api/runners', b),
  deleteRunner: (id: string) => req<void>('DELETE', `/api/runners/${id}`),

  githubApp: () => req<GithubAppStatus>('GET', '/api/github/app'),
  githubRepos: () => req<GithubRepo[]>('GET', '/api/github/repos'),
  /** Mints a fresh single-use state: call it when the setup page opens, not in advance. */
  githubManifest: (org?: string, name?: string) => req<GithubManifest>('GET', `/api/github/app/manifest${qs({ org, name })}`),
  /** Full-page navigation target (GitHub's app-manifest flow), not a fetch. */
  githubSetupUrl: (org?: string, name?: string) => `/api/github/app/setup${qs({ org, name })}`,
};

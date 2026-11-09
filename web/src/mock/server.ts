/**
 * In-browser fake Boba server (VITE_MOCK=1). Implements the REST + WS semantics of docs/API.md
 * with simulated Claude Code runs, so the UI can be developed and demoed without the backend.
 */
import type {
  Agent,
  AssignAgent,
  Column,
  ColumnSpec,
  CreateAgent,
  CreateRepo,
  CreateTab,
  CreateTicket,
  Entity,
  EntityKind,
  Fx,
  HistoryEvent,
  HistoryKind,
  LogKind,
  PresenceUser,
  Run,
  RunKind,
  RunLogLine,
  ServerMessage,
  Snapshot,
  Tab,
  Ticket,
  UpdateAgent,
  UpdateTab,
  UpdateTicket,
  UpsertSkill,
  User,
  ClientMessage,
} from '../api/types';
import { ApiFailure, type SocketHandlers, type SocketHandle, type Transport } from '../api/transport';
import { grants, xpForNextLevel } from '../lib/game';
import { defaultSync, mkId, seed, type MockState, type MockUser } from './seed';
import { reviewScript, workScript, type Step } from './scripts';
import { TOOLS, jobScript, plan } from './automation';
import { describe as describeSchedule, nextRuns, validateCron } from '../lib/schedule';
import type { Comment, CreateComment, CreateRunner, Notification, RunnerInfo, UpdateComment, UpdateMe, CommandAction, CommandPlan, Connector, CreateConnector, CreateJob, Job, PlannedAction, Schedule, SyncReport, UpdateConnector, UpdateJob, UpdateRepo } from '../api/types';

const NOPE = Symbol('nope');

const STORE_KEY = 'boba-mock-v7';
const now = () => new Date().toISOString();
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
const slug = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 32);
const pick = <T>(xs: T[]): T => xs[Math.floor(Math.random() * xs.length)] as T;

interface Client {
  h: SocketHandlers;
  subs: Set<string>;
  viewing: string | null;
}

interface Sim {
  runId: string;
  steps: Step[];
  idx: number;
  /** ms until next step */
  nextIn: number;
  waiting: string | null;
  speed: number;
}

class MockServer implements Transport {
  readonly mock = true;
  s: MockState;
  private clients = new Set<Client>();
  private sims = new Map<string, Sim>();
  private saveTimer: ReturnType<typeof setTimeout> | undefined;
  private botsOn = true;
  private botPresence: PresenceUser[] = [
    { user_id: 'u_mika', name: 'Mika', avatar: 'pinky_boba_avatar', tab_id: 'tab_dev' },
    { user_id: 'u_sam', name: 'Sam', avatar: 'golden_boba_avatar', tab_id: 'tab_seo' },
  ];
  speed = 1;
  private plans = new Map<string, PlannedAction[]>();
  private pendingCommands = new Map<string, { text: string; tab_id: string | null; ticket_id: string | null; agent_id: string | null }>();

  constructor() {
    this.s = this.load();
    if (this.s.runs.length === 0 && this.s.users.length) this.seedRuns();
    for (const t of this.s.tickets) this.recount(t.id, false);
    for (const r of this.s.runs) if (r.status === 'running' || r.status === 'queued' || r.status === 'waiting') this.resume(r);
    setInterval(() => this.tick(250), 250);
    setInterval(() => this.botTick(), 9000);
    setTimeout(() => this.botAssign(true), 4500);
    setTimeout(() => this.autoReview(), 6000);
    setInterval(() => this.schedulerTick(), 5000);
  }

  // ---------------------------------------------------------------- persistence
  private load(): MockState {
    try {
      if (new URLSearchParams(location.search).has('fresh')) {
        const s = seed();
        s.users = [];
        s.sessionUserId = null;
        return s;
      }
      const raw = localStorage.getItem(STORE_KEY);
      if (raw) return JSON.parse(raw) as MockState;
    } catch {
      /* storage unavailable */
    }
    return seed();
  }
  private save() {
    clearTimeout(this.saveTimer);
    this.saveTimer = setTimeout(() => {
      try {
        // Keep logs bounded so localStorage never overflows.
        const logs: MockState['logs'] = {};
        const keep = new Set(this.s.runs.slice(-40).map((r) => r.id));
        for (const [k, v] of Object.entries(this.s.logs)) if (keep.has(k)) logs[k] = v.slice(-300);
        localStorage.setItem(STORE_KEY, JSON.stringify({ ...this.s, logs, history: this.s.history.slice(0, 400) }));
      } catch {
        /* quota / private mode */
      }
    }, 400);
  }
  /** Finished runs (with transcripts) behind the seeded tickets, so history and profiles are not empty. */
  private seedRuns() {
    const mk = (ticketId: string, agentId: string, kind: RunKind, verdict: 'approved' | 'changes_requested' | undefined, agoMs: number) => {
      const t = this.ticket(ticketId);
      const a = this.agent(agentId);
      if (!t || !a) return;
      const skills = a.skill_ids.map((id) => this.s.skills.find((s) => s.id === id)?.name).filter((x): x is string => !!x);
      const ctx = { ticket: t, agent: a, repo: this.s.repos.find((r) => r.id === this.tab(t.tab_id)?.repo_id)?.name ?? 'repo', skills, instructions: null, resume: false, prNumber: t.pr_number ?? 0, verdict: kind === 'review' ? verdict : 'approved' as const };
      const steps = (kind === 'review' ? reviewScript(ctx) : workScript(ctx)).filter((s) => s.kind !== 'permission' && s.kind !== 'fail');
      const start = Date.now() - agoMs;
      const id = mkId('run');
      let tms = start;
      this.s.logs[id] = steps.map((s, seq) => {
        tms += (s.wait ?? 900) * 1.4;
        return { run_id: id, seq, ts: new Date(tms).toISOString(), kind: s.kind as LogKind, text: s.text, data: s.data ?? null };
      });
      const tokensIn = 9000 + steps.length * 1500;
      const tokensOut = 1200 + steps.length * 260;
      this.s.runs.push({
        id, ticket_id: t.id, job_id: null, agent_id: a.id, kind, status: 'succeeded', runner_id: 'local', branch: t.branch, worktree_path: `/data/worktrees/T-${t.number}`,
        session_id: mkId('sess'), progress: 1, activity: kind === 'review' ? (verdict === 'approved' ? 'Approved' : 'Changes requested') : 'Pull request opened',
        summary: kind === 'review' ? (verdict === 'approved' ? `Approved PR #${t.pr_number}.` : `Changes requested: ${t.review_feedback}`) : `Opened PR #${t.pr_number}.`,
        verdict: kind === 'review' ? verdict ?? null : null, cost_usd: +(tokensIn * 0.000003 + tokensOut * 0.000015).toFixed(4), input_tokens: tokensIn, output_tokens: tokensOut,
        started_by: kind === 'work' ? 'u_owner' : null, created_at: new Date(start).toISOString(), started_at: new Date(start).toISOString(), ended_at: new Date(tms).toISOString(), error: null,
      });
    };
    const H = 3600_000;
    mk('tk_6', 'a_dev', 'work', undefined, 72 * H);
    mk('tk_6', 'a_uncle', 'review', 'approved', 71.6 * H);
    mk('tk_2', 'a_dev', 'work', undefined, 24 * H);
    mk('tk_2', 'a_uncle', 'review', 'changes_requested', 23.5 * H);
    mk('tk_10', 'a_pinky', 'work', undefined, 5 * H);
    this.save();
  }

  resetDemo() {
    try {
      localStorage.removeItem(STORE_KEY);
    } catch {
      /* ignore */
    }
    this.sims.clear();
    this.s = seed();
    this.seedRuns();
    for (const t of this.s.tickets) this.recount(t.id, false);
    this.broadcast({ type: 'snapshot', state: this.snapshot() });
    this.save();
  }
  setBots(on: boolean) {
    this.botsOn = on;
    this.presence();
  }
  /** Demo helper: preview the UI as another role. */
  setMyRole(role: User['role']) {
    const me = this.me();
    if (!me) return;
    me.role = role;
    this.upsert({ entity: 'user', data: this.pub(me) });
    this.broadcast({ type: 'snapshot', state: this.snapshot() });
  }

  // ---------------------------------------------------------------- ws
  openSocket(h: SocketHandlers): SocketHandle {
    const c: Client = { h, subs: new Set(), viewing: null };
    let closed = false;
    setTimeout(() => {
      if (closed) return;
      if (!this.me()) {
        h.onClose();
        return;
      }
      this.clients.add(c);
      h.onOpen();
      h.onMessage({ type: 'snapshot', state: this.snapshot() });
      this.presence();
    }, 80);
    return {
      send: (m) => this.onClient(c, m),
      close: () => {
        closed = true;
        this.clients.delete(c);
        this.presence();
        setTimeout(() => h.onClose(), 0);
      },
    };
  }
  private onClient(c: Client, m: ClientMessage) {
    switch (m.type) {
      case 'resync':
        c.h.onMessage({ type: 'snapshot', state: this.snapshot() });
        break;
      case 'subscribe':
        for (const id of m.run_ids) {
          c.subs.add(id);
          for (const line of (this.s.logs[id] ?? []).slice(-200)) c.h.onMessage({ type: 'run_log', line });
        }
        break;
      case 'unsubscribe':
        m.run_ids.forEach((id) => c.subs.delete(id));
        break;
      case 'viewing':
        c.viewing = m.tab_id;
        this.presence();
        break;
      case 'ping':
        break;
    }
  }
  private broadcast(m: ServerMessage) {
    for (const c of this.clients) {
      if (m.type === 'run_log' && !c.subs.has(m.line.run_id)) continue;
      // Async like a real socket so React batches nicely.
      queueMicrotask(() => c.h.onMessage(m));
    }
  }
  private presence() {
    const me = this.me();
    const users: PresenceUser[] = [];
    const first = [...this.clients][0];
    if (me && first) users.push({ user_id: me.id, name: me.name, avatar: me.avatar, tab_id: first.viewing });
    if (this.botsOn) users.push(...this.botPresence);
    this.broadcast({ type: 'presence', users });
  }
  private upsert(item: Entity) {
    this.broadcast({ type: 'upsert', item });
    this.save();
  }
  private del(kind: EntityKind, id: string) {
    this.broadcast({ type: 'delete', kind, id });
    this.save();
  }
  private fx(fx: Fx) {
    this.broadcast({ type: 'fx', fx });
  }
  private hist(kind: HistoryKind, message: string, e: Partial<HistoryEvent> = {}): HistoryEvent {
    const me = this.me();
    const ev: HistoryEvent = {
      id: mkId('h'),
      ts: now(),
      kind,
      actor_kind: 'user',
      actor_id: me?.id ?? null,
      ticket_id: null,
      agent_id: null,
      run_id: null,
      tab_id: null,
      message,
      data: {},
      ...e,
    };
    if (ev.ticket_id && !ev.tab_id) ev.tab_id = this.ticket(ev.ticket_id)?.tab_id ?? null;
    this.s.history.unshift(ev);
    if (this.s.history.length > 2000) this.s.history.length = 2000;
    this.broadcast({ type: 'history', event: ev });
    this.save();
    return ev;
  }

  // ---------------------------------------------------------------- lookups
  private me(): MockUser | undefined {
    return this.s.users.find((u) => u.id === this.s.sessionUserId);
  }
  private pub(u: MockUser): User {
    const { activated: _a, ...rest } = u;
    return rest;
  }
  private ticket(id: string | null | undefined) {
    return id ? this.s.tickets.find((t) => t.id === id) : undefined;
  }
  private agent(id: string | null | undefined) {
    return this.s.agents.find((a) => a.id === id);
  }
  private tab(id: string) {
    return this.s.tabs.find((t) => t.id === id);
  }
  private column(id: string): Column | undefined {
    for (const t of this.s.tabs) for (const c of t.columns) if (c.id === id) return c;
    return undefined;
  }
  private firstCol(tabId: string, kind: Column['kind']): Column | undefined {
    return this.tab(tabId)?.columns.filter((c) => c.kind === kind).sort((a, b) => a.position - b.position)[0];
  }
  private endPos(colId: string) {
    const ps = this.s.tickets.filter((t) => t.column_id === colId).map((t) => t.position);
    return (ps.length ? Math.max(...ps) : 0) + 1024;
  }
  private activeRun(ticketId: string) {
    return this.s.runs.find((r) => r.ticket_id === ticketId && (r.status === 'queued' || r.status === 'running' || r.status === 'waiting'));
  }
  snapshot(): Snapshot {
    const me = this.me();
    const latest = new Map<string, Run>();
    for (const r of this.s.runs) {
      if (!r.ticket_id) continue;
      const cur = latest.get(r.ticket_id);
      if (!cur || cur.created_at < r.created_at) latest.set(r.ticket_id, r);
    }
    const runs = new Map<string, Run>();
    for (const r of this.s.runs) if (r.status === 'queued' || r.status === 'running' || r.status === 'waiting') runs.set(r.id, r);
    for (const r of latest.values()) runs.set(r.id, r);
    return {
      me: me ? this.pub(me) : null,
      tabs: this.s.tabs,
      agents: this.s.agents,
      tickets: this.s.tickets,
      runs: [...runs.values()],
      skills: this.s.skills,
      notifications: this.s.notifications.filter((n) => n.user_id === me?.id).slice(0, 80),
      repos: this.s.repos,
      users: this.s.users.map((u) => this.pub(u)),
      runners: this.s.runners.map((r) => ({ ...r, active_runs: [...this.sims.keys()].filter((id) => this.s.runs.find((x) => x.id === id)?.runner_id === r.id).length, last_seen_at: r.online ? now() : r.last_seen_at })),
      jobs: this.s.jobs,
      connectors: this.s.connectors,
      history: this.s.history.slice(0, 100),
      settings: this.s.settings,
      stats: this.s.stats,
    };
  }

  // ---------------------------------------------------------------- REST
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    await wait(60 + Math.random() * 120);
    const url = new URL(path, 'http://mock');
    const res = this.route(method, url.pathname, url.searchParams, (body ?? {}) as Record<string, unknown>);
    return structuredClone(res) as T;
  }

  async upload<T>(path: string, _body: Blob): Promise<T> {
    return this.request<T>('POST', path, {});
  }

  private need(p: Parameters<typeof grants>[1]) {
    const me = this.me();
    if (!me) throw new ApiFailure(401, 'Please log in first.', 'unauthorized');
    if (!grants(me.role, p)) throw new ApiFailure(403, 'Your role does not allow this action.', 'forbidden');
    return me;
  }

  private route(method: string, path: string, q: URLSearchParams, b: Record<string, unknown>): unknown {
    const m = (re: RegExp) => path.match(re);
    let x: RegExpMatchArray | null;
    const key = `${method} ${path}`;

    // auth
    if (key === 'GET /api/auth/status') {
      const me = this.me();
      return { setup_done: this.s.users.length > 0, me: me ? this.pub(me) : null };
    }
    if (key === 'POST /api/auth/setup') {
      if (this.s.users.length) throw new ApiFailure(409, 'Setup is already done.', 'conflict');
      const fresh = seed();
      const owner = fresh.users[0] as MockUser;
      Object.assign(owner, { name: String(b.name), email: String(b.email) });
      fresh.settings.name = String(b.workspace_name || 'Boba HQ');
      this.s = fresh;
      this.s.sessionUserId = owner.id;
      this.save();
      return this.pub(owner);
    }
    if (key === 'POST /api/auth/login') {
      const u = this.s.users.find((u) => u.email.toLowerCase() === String(b.email).toLowerCase());
      if (!u || !u.activated || !String(b.password ?? '')) throw new ApiFailure(401, 'Wrong email or password.', 'unauthorized');
      this.s.sessionUserId = u.id;
      this.save();
      return this.pub(u);
    }
    if (key === 'POST /api/auth/logout') {
      this.s.sessionUserId = null;
      this.save();
      for (const c of [...this.clients]) {
        this.clients.delete(c);
        c.h.onClose();
      }
      return undefined;
    }
    if (key === 'POST /api/users/invite') {
      this.need('manage_users');
      if (this.s.users.some((u) => u.email === b.email)) throw new ApiFailure(409, 'This email is already a member.', 'conflict');
      const u: MockUser = { id: mkId('u'), name: String(b.name), email: String(b.email), role: b.role as User['role'], avatar: pick(['boba_avatar', 'pinky_boba_avatar', 'golden_boba_avatar', 'black_boba_avatar', 'diamond_boba_avatar']), created_at: now(), activated: false };
      this.s.users.push(u);
      const token = mkId('inv');
      this.s.invites[token] = u.id;
      this.upsert({ entity: 'user', data: this.pub(u) });
      return { user: this.pub(u), invite_url: `${location.origin}${location.pathname}?invite=${token}` };
    }
    if (key === 'POST /api/auth/accept-invite') {
      const uid = this.s.invites[String(b.token)];
      const u = this.s.users.find((x) => x.id === uid);
      if (!u) throw new ApiFailure(404, 'This invite link is invalid or was already used.', 'not_found');
      u.activated = true;
      delete this.s.invites[String(b.token)];
      this.s.sessionUserId = u.id;
      this.hist('user_joined', `${u.name} joined as ${u.role[0]?.toUpperCase()}${u.role.slice(1)}`, { actor_id: u.id });
      return this.pub(u);
    }
    if ((x = m(/^\/api\/users\/([^/]+)$/)) && method === 'PATCH') {
      this.need('manage_users');
      const u = this.s.users.find((u) => u.id === x![1]);
      if (!u) throw new ApiFailure(404, 'User not found.', 'not_found');
      if (u.role === 'owner') throw new ApiFailure(403, 'The owner role cannot be changed here.', 'forbidden');
      u.role = b.role as User['role'];
      this.upsert({ entity: 'user', data: this.pub(u) });
      return this.pub(u);
    }

    // state
    if (key === 'GET /api/snapshot') {
      this.need('view');
      return this.snapshot();
    }
    if (key === 'GET /api/history') {
      this.need('view');
      const f = (k: string) => q.get(k);
      const before = f('before');
      const limit = Math.min(500, Number(f('limit') ?? 50));
      return this.s.history
        .filter(
          (e) =>
            (!f('ticket_id') || e.ticket_id === f('ticket_id')) &&
            (!f('agent_id') || e.agent_id === f('agent_id') || (e.actor_kind === 'agent' && e.actor_id === f('agent_id'))) &&
            (!f('run_id') || e.run_id === f('run_id')) &&
            (!f('tab_id') || e.tab_id === f('tab_id')) &&
            (!before || e.ts < before),
        )
        .slice(0, limit);
    }
    if (key === 'PATCH /api/settings') {
      this.need('manage_workspace');
      Object.assign(this.s.settings, b);
      this.broadcast({ type: 'snapshot', state: this.snapshot() });
      this.save();
      return this.s.settings;
    }

    // tabs
    if (key === 'POST /api/tabs') {
      this.need('manage_workspace');
      return this.createTab(b as unknown as CreateTab);
    }
    if ((x = m(/^\/api\/tabs\/([^/]+)$/))) {
      this.need('manage_workspace');
      const tab = this.tab(x[1] as string);
      if (!tab) throw new ApiFailure(404, 'Tab not found.', 'not_found');
      if (method === 'PATCH') return this.updateTab(tab, b as Partial<UpdateTab>);
      if (method === 'DELETE') {
        if (this.s.tickets.some((t) => t.tab_id === tab.id && this.activeRun(t.id)))
          throw new ApiFailure(409, 'This tab still has running tickets.', 'conflict');
        this.s.tabs = this.s.tabs.filter((t) => t !== tab);
        for (const t of this.s.tickets.filter((t) => t.tab_id === tab.id)) this.del('ticket', t.id);
        this.s.tickets = this.s.tickets.filter((t) => t.tab_id !== tab.id);
        this.del('tab', tab.id);
        this.hist('tab_deleted', `${this.me()?.name} deleted the tab ${tab.name}`);
        return undefined;
      }
    }

    // agents
    if (key === 'POST /api/agents') {
      this.need('manage_agents');
      const c = b as unknown as CreateAgent;
      if (!c.name?.trim()) throw new ApiFailure(400, 'An agent needs a name.', 'bad_request');
      if (c.permission_mode === 'bypassPermissions') throw new ApiFailure(403, 'bypassPermissions needs BOBA_ALLOW_BYPASS=1 on a containerised server.', 'forbidden');
      const a: Agent = {
        id: mkId('a'),
        name: c.name.trim(),
        role: c.role || 'Dev',
        sprite: c.sprite || 'boba',
        color: c.color || '#f39c12',
        tagline: c.tagline || '',
        system_prompt: c.system_prompt || '',
        skill_ids: c.skill_ids ?? [],
        connector_ids: c.connector_ids ?? [],
        board_tools: c.board_tools ?? false,
        model: c.model ?? null,
        permission_mode: c.permission_mode ?? 'acceptEdits',
        allowed_tools: c.allowed_tools ?? [],
        max_turns: c.max_turns ?? null,
        xp: 0,
        level: 1,
        runs_completed: 0,
        prs_approved: 0,
        status: 'idle',
        current_run_id: null,
        created_by: this.me()?.id ?? null,
        created_at: now(),
      };
      this.s.agents.push(a);
      this.upsert({ entity: 'agent', data: a });
      this.hist('agent_created', `${this.me()?.name} recruited ${a.name}`, { agent_id: a.id });
      return a;
    }
    if ((x = m(/^\/api\/agents\/([^/]+)$/))) {
      this.need('manage_agents');
      const a = this.agent(x[1]);
      if (!a) throw new ApiFailure(404, 'Agent not found.', 'not_found');
      if (method === 'PATCH') {
        const u = b as Partial<UpdateAgent>;
        if (u.permission_mode === 'bypassPermissions') throw new ApiFailure(403, 'bypassPermissions needs BOBA_ALLOW_BYPASS=1 on a containerised server.', 'forbidden');
        for (const [k, v] of Object.entries(u)) if (v !== undefined) (a as Record<string, unknown>)[k] = v;
        this.upsert({ entity: 'agent', data: a });
        this.hist('agent_updated', `${this.me()?.name} updated ${a.name}`, { agent_id: a.id });
        return a;
      }
      if (method === 'DELETE') {
        if (a.status !== 'idle') throw new ApiFailure(409, `${a.name} is working. Cancel the run first.`, 'conflict');
        this.s.agents = this.s.agents.filter((y) => y !== a);
        for (const t of this.s.tabs)
          for (const c of t.columns)
            if (c.auto_reviewer_agent_id === a.id) {
              c.auto_reviewer_agent_id = null;
              this.upsert({ entity: 'tab', data: t });
            }
        this.del('agent', a.id);
        this.hist('agent_deleted', `${a.name} left the tavern`, { agent_id: a.id });
        return undefined;
      }
    }

    // tickets
    if (key === 'POST /api/tickets') {
      this.need('edit_tickets');
      const c = b as unknown as CreateTicket;
      if (!c.title?.trim()) throw new ApiFailure(400, 'A ticket needs a title.', 'bad_request');
      const col = (c.column_id && this.column(c.column_id)) || this.firstCol(c.tab_id, 'todo') || this.tab(c.tab_id)?.columns[0];
      if (!col) throw new ApiFailure(400, 'This tab has no column.', 'bad_request');
      const t: Ticket = {
        id: mkId('tk'),
        number: ++this.s.seq,
        tab_id: c.tab_id,
        comment_count: 0,
        open_thread_count: 0,
        column_id: col.id,
        position: this.endPos(col.id),
        title: c.title.trim(),
        body: c.body ?? '',
        tags: c.tags ?? [],
        priority: c.priority ?? 'normal',
        color: c.color ?? null,
        repo_id: c.repo_id ?? null,
        agent_id: null,
        branch: null,
        pr_url: null,
        pr_number: null,
        review_feedback: null,
        review_rounds: 0,
        source: { kind: 'manual' },
        created_by: this.me()?.id ?? null,
        created_at: now(),
        updated_at: now(),
      };
      this.s.tickets.push(t);
      this.upsert({ entity: 'ticket', data: t });
      this.hist('ticket_created', `${this.me()?.name} pinned T-${t.number} ${t.title}`, { ticket_id: t.id });
      return t;
    }
    if ((x = m(/^\/api\/tickets\/([^/]+)(?:\/(\w+))?$/))) {
      const t = this.ticket(x[1] as string);
      if (!t) throw new ApiFailure(404, 'Ticket not found.', 'not_found');
      const sub = x[2];
      if (!sub && method === 'PATCH') {
        this.need('edit_tickets');
        for (const [k, v] of Object.entries(b as Partial<UpdateTicket>)) if (v !== undefined) (t as Record<string, unknown>)[k] = v;
        t.updated_at = now();
        this.upsert({ entity: 'ticket', data: t });
        this.hist('ticket_updated', `${this.me()?.name} edited T-${t.number}`, { ticket_id: t.id });
        return t;
      }
      if (!sub && method === 'DELETE') {
        this.need('edit_tickets');
        const r = this.activeRun(t.id);
        if (r) this.finish(r, 'cancelled');
        this.s.tickets = this.s.tickets.filter((y) => y !== t);
        this.del('ticket', t.id);
        this.hist('ticket_deleted', `${this.me()?.name} deleted T-${t.number} ${t.title}`, { ticket_id: t.id });
        return undefined;
      }
      if (sub === 'move' && method === 'POST') {
        this.need('edit_tickets');
        const col = this.column(String(b.column_id));
        if (!col || col.tab_id !== t.tab_id) throw new ApiFailure(400, 'Unknown column.', 'bad_request');
        const from = this.column(t.column_id);
        t.column_id = col.id;
        t.position = Number(b.position);
        t.updated_at = now();
        this.upsert({ entity: 'ticket', data: t });
        if (from?.id !== col.id) {
          this.hist('ticket_moved', `${this.me()?.name} moved T-${t.number} to ${col.name}`, { ticket_id: t.id, data: { from: from?.name, to: col.name } });
          this.autoReview();
        }
        return t;
      }
      if (sub === 'assign' && method === 'POST') {
        this.need('run_agents');
        return this.assign(t, (b as unknown as AssignAgent).agent_id, (b as unknown as AssignAgent).instructions ?? null, this.me()?.id ?? null);
      }
      if (sub === 'comment' && method === 'POST') {
        this.need('edit_tickets');
        return this.hist('comment', `${this.me()?.name} commented on T-${t.number}: "${String(b.text)}"`, { ticket_id: t.id, data: { text: b.text } });
      }
      if (sub === 'runs' && method === 'GET') {
        this.need('view');
        return this.s.runs.filter((r) => r.ticket_id === t.id).sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
      }
    }

    // runs
    if ((x = m(/^\/api\/runs\/([^/]+)(?:\/(\w+))?$/))) {
      const r = this.s.runs.find((r) => r.id === x![1]);
      if (!r) throw new ApiFailure(404, 'Run not found.', 'not_found');
      const sub = x[2];
      if (!sub && method === 'GET') return r;
      if (sub === 'logs') {
        const after = q.get('after_seq');
        const limit = Number(q.get('limit') ?? 500);
        return (this.s.logs[r.id] ?? []).filter((l) => after === null || l.seq > Number(after)).slice(0, limit);
      }
      if (sub === 'cancel') {
        this.need('run_agents');
        if (r.status === 'running' || r.status === 'waiting' || r.status === 'queued') this.finish(r, 'cancelled');
        return r;
      }
      if (sub === 'message') {
        const me = this.need('run_agents');
        const text = String(b.text ?? '').trim();
        if (!text) throw new ApiFailure(400, 'Message is empty.', 'bad_request');
        if (r.status === 'running' || r.status === 'waiting') {
          this.log(r, 'user', text, { user_id: me.id });
          const sim = this.sims.get(r.id);
          if (sim) sim.steps.splice(sim.idx, 0, { kind: 'text', text: `Got it, ${me.name}. I'll take that into account: ${text.slice(0, 80)}`, wait: 1200 });
          return r;
        }
        const t = this.ticket(r.ticket_id);
        if (!t) throw new ApiFailure(404, 'Ticket not found.', 'not_found');
        return this.assign(t, r.agent_id, text, me.id, r);
      }
      if (sub === 'permission') {
        this.need('run_agents');
        const sim = this.sims.get(r.id);
        if (!sim || sim.waiting !== b.request_id) throw new ApiFailure(409, 'No pending permission request.', 'conflict');
        sim.waiting = null;
        r.status = 'running';
        r.activity = b.allow ? 'Permission granted, continuing' : 'Permission denied, finding another way';
        this.log(r, 'system', b.allow ? `${this.me()?.name} allowed the tool call` : `${this.me()?.name} denied the tool call`, {
          permission_resolved: { request_id: b.request_id, allow: !!b.allow, user_id: this.me()?.id },
        });
        this.upsert({ entity: 'run', data: r });
        return undefined;
      }
    }

    // skills
    if ((x = m(/^\/api\/skills\/([^/]+)$/)) && method === 'GET' && x[1] !== 'rescan') {
      const sk = this.s.skills.find((s) => s.id === x![1]);
      if (!sk) throw new ApiFailure(404, 'Skill not found.', 'not_found');
      return { skill: sk, body: this.s.skillBodies[sk.id] ?? `# ${sk.name}\n\n${sk.description}`, files: sk.source === 'boba' ? ['SKILL.md'] : ['SKILL.md', 'reference.md'] };
    }
    if (key === 'POST /api/skills') {
      this.need('manage_skills');
      const u = b as unknown as UpsertSkill;
      const name = slug(u.name || '');
      if (!name) throw new ApiFailure(400, 'A skill needs a name.', 'bad_request');
      if (this.s.skills.some((s) => s.name === name)) throw new ApiFailure(409, 'A skill with this name exists.', 'conflict');
      const sk = { id: mkId('sk'), name, description: u.description, source: 'boba' as const, path: `/data/skills/${name}`, editable: true };
      this.s.skills.push(sk);
      this.s.skillBodies[sk.id] = u.body;
      this.upsert({ entity: 'skill', data: sk });
      this.hist('skill_created', `${this.me()?.name} created the skill ${name}`);
      return sk;
    }
    if ((x = m(/^\/api\/skills\/([^/]+)$/)) && method === 'PUT') {
      this.need('manage_skills');
      const sk = this.s.skills.find((s) => s.id === x![1]);
      if (!sk) throw new ApiFailure(404, 'Skill not found.', 'not_found');
      if (!sk.editable) throw new ApiFailure(403, 'Only Boba-managed skills can be edited.', 'forbidden');
      const u = b as unknown as UpsertSkill;
      sk.description = u.description;
      this.s.skillBodies[sk.id] = u.body;
      this.upsert({ entity: 'skill', data: sk });
      this.hist('skill_updated', `${this.me()?.name} updated the skill ${sk.name}`);
      return sk;
    }
    if (key === 'POST /api/skills/rescan') {
      this.need('manage_skills');
      return this.s.skills;
    }

    // repos
    if (key === 'POST /api/repos') {
      this.need('manage_workspace');
      const c = b as unknown as CreateRepo;
      const r = { id: mkId('r'), name: c.name, path: c.path, remote_url: c.remote_url ?? null, default_branch: c.default_branch || 'main', runner_id: c.runner_id ?? null, ai_attribution: c.ai_attribution ?? true, sync: c.sync ?? defaultSync() };
      this.s.repos.push(r);
      this.upsert({ entity: 'repo', data: r });
      return r;
    }
    if ((x = m(/^\/api\/repos\/([^/]+)$/)) && method === 'DELETE') {
      this.need('manage_workspace');
      this.s.repos = this.s.repos.filter((r) => r.id !== x![1]);
      this.del('repo', x[1] as string);
      return undefined;
    }

    const auto = this.routeAutomation(method, path, b);
    if (auto !== NOPE) return auto;
    const team = this.routeTeam(method, path, q, b);
    if (team !== NOPE) return team;
    throw new ApiFailure(404, `No mock route for ${method} ${path}`, 'not_found');
  }

  // ---------------------------------------------------------------- tabs
  private specsToColumns(tabId: string, specs: ColumnSpec[], prev: Column[]): Column[] {
    return specs.map((s, i) => ({
      id: s.id ?? `${tabId}_${mkId('c')}`,
      tab_id: tabId,
      name: s.name,
      kind: s.kind,
      color: s.color ?? prev.find((p) => p.id === s.id)?.color ?? '#95a5a6',
      position: i,
      auto_reviewer_agent_id: s.auto_reviewer_agent_id,
      on_approve_column_id: s.on_approve_column_id,
      on_changes_column_id: s.on_changes_column_id,
    }));
  }
  private createTab(c: CreateTab): Tab {
    const id = mkId('tab');
    const reviewer = this.s.agents.find((a) => a.role.toLowerCase() === 'reviewer')?.id ?? null;
    const specs: ColumnSpec[] =
      c.columns ??
      ([
        ['To do', 'todo', '#e74c3c'],
        ['Doing', 'doing', '#f39c12'],
        ['Review', 'review', '#3498db'],
        ['Done', 'done', '#2ecc71'],
      ] as const).map(([name, kind, color]) => ({ id: null, name, kind, color, auto_reviewer_agent_id: kind === 'review' ? reviewer : null, on_approve_column_id: null, on_changes_column_id: null }));
    const tab: Tab = {
      id,
      name: c.name,
      icon: c.icon || '📋',
      color: c.color || '#f39c12',
      position: Math.max(-1, ...this.s.tabs.map((t) => t.position)) + 1,
      repo_id: c.repo_id,
      columns: this.specsToColumns(id, specs, []),
    };
    this.s.tabs.push(tab);
    this.upsert({ entity: 'tab', data: tab });
    this.hist('tab_created', `${this.me()?.name} opened the tab ${tab.name}`, { tab_id: tab.id });
    return tab;
  }
  private updateTab(tab: Tab, u: Partial<UpdateTab>): Tab {
    if (u.name != null) tab.name = u.name;
    if (u.icon != null) tab.icon = u.icon;
    if (u.color != null) tab.color = u.color;
    if (u.repo_id !== undefined) tab.repo_id = u.repo_id;
    if (u.position != null && u.position !== tab.position) {
      const sorted = this.s.tabs.filter((t) => t !== tab).sort((a, b) => a.position - b.position);
      sorted.splice(Math.max(0, Math.min(sorted.length, u.position)), 0, tab);
      sorted.forEach((t, i) => {
        if (t.position !== i) {
          t.position = i;
          if (t !== tab) this.upsert({ entity: 'tab', data: t });
        }
      });
    }
    if (u.columns) {
      const next = this.specsToColumns(tab.id, u.columns, tab.columns);
      const kept = new Set(next.map((c) => c.id));
      const fallback = next.find((c) => c.kind === 'todo') ?? next[0];
      for (const t of this.s.tickets)
        if (t.tab_id === tab.id && !kept.has(t.column_id) && fallback) {
          t.column_id = fallback.id;
          this.upsert({ entity: 'ticket', data: t });
        }
      tab.columns = next;
    }
    this.upsert({ entity: 'tab', data: tab });
    this.hist('tab_updated', `${this.me()?.name} customised the tab ${tab.name}`, { tab_id: tab.id });
    this.autoReview();
    return tab;
  }

  // ---------------------------------------------------------------- runs
  private assign(t: Ticket, agentId: string, instructions: string | null, by: string | null, resumeOf?: Run): Run {
    const a = this.agent(agentId);
    if (!a) throw new ApiFailure(404, 'Agent not found.', 'not_found');
    if (a.status !== 'idle') throw new ApiFailure(409, `${a.name} is already busy.`, 'conflict');
    if (this.activeRun(t.id)) throw new ApiFailure(409, `T-${t.number} already has a running agent.`, 'conflict');
    const col = this.column(t.column_id);
    const kind: RunKind = resumeOf?.kind ?? (col?.kind === 'review' ? 'review' : 'work');
    if (kind === 'work') {
      const doing = this.firstCol(t.tab_id, 'doing');
      if (doing && doing.id !== t.column_id) {
        t.column_id = doing.id;
        t.position = this.endPos(doing.id);
      }
      t.agent_id = a.id;
    }
    t.updated_at = now();
    return this.startRun(t, a, kind, by, instructions, resumeOf);
  }

  private startRun(t: Ticket, a: Agent, kind: RunKind, by: string | null, instructions: string | null, resumeOf?: Run): Run {
    const branch = t.branch ?? `boba/T-${t.number}-${slug(t.title)}`;
    const r: Run = {
      id: mkId('run'),
      ticket_id: t.id,
      job_id: null,
      agent_id: a.id,
      kind,
      status: 'running',
      runner_id: this.pickRunner(by),
      branch,
      worktree_path: `/data/worktrees/T-${t.number}`,
      session_id: resumeOf?.session_id ?? mkId('sess'),
      progress: 0,
      activity: kind === 'review' ? 'Opening the pull request' : 'Creating the worktree',
      summary: null,
      verdict: null,
      cost_usd: 0,
      input_tokens: 0,
      output_tokens: 0,
      started_by: by,
      created_at: now(),
      started_at: now(),
      ended_at: null,
      error: null,
    };
    this.s.runs.push(r);
    if (this.s.runs.length > 300) this.s.runs.splice(0, this.s.runs.length - 300);
    a.status = kind === 'review' ? 'reviewing' : 'working';
    a.current_run_id = r.id;
    t.branch = branch;
    this.upsert({ entity: 'ticket', data: t });
    this.upsert({ entity: 'agent', data: a });
    this.upsert({ entity: 'run', data: r });
    const who = by ? this.s.users.find((u) => u.id === by)?.name ?? 'Someone' : 'Boba';
    this.hist('run_started', kind === 'review' ? `${a.name} started reviewing T-${t.number}${t.pr_number ? ` (PR #${t.pr_number})` : ''}` : `${who} sent ${a.name} on T-${t.number} ${t.title}`, {
      actor_kind: by ? 'user' : 'system',
      actor_id: by,
      ticket_id: t.id,
      agent_id: a.id,
      run_id: r.id,
    });
    const repo = this.s.repos.find((x) => x.id === (t.repo_id ?? this.tab(t.tab_id)?.repo_id)) ?? this.s.repos[0];
    const skills = a.skill_ids.map((id) => this.s.skills.find((s) => s.id === id)?.name).filter((x): x is string => !!x);
    const ctx = { ticket: t, agent: a, repo: repo?.name ?? 'repo', skills, instructions, resume: !!resumeOf, prNumber: t.pr_number ?? this.s.prSeq + 1 };
    const steps = kind === 'review' ? reviewScript(ctx) : workScript(ctx);
    this.sims.set(r.id, { runId: r.id, steps, idx: 0, nextIn: 300, waiting: null, speed: 0.8 + Math.random() * 0.5 });
    return r;
  }

  private resume(r: Run) {
    const t = this.ticket(r.ticket_id);
    const a = this.agent(r.agent_id);
    if (!t || !a) {
      // Job / command runs are not resumable in the mock: close them.
      r.status = 'cancelled';
      r.ended_at = now();
      this.release(a);
      return;
    }
    const ctx = { ticket: t, agent: a, repo: 'repo', skills: [], instructions: null, resume: true, prNumber: t.pr_number ?? this.s.prSeq + 1 };
    const steps = r.kind === 'review' ? reviewScript(ctx) : workScript(ctx);
    const idx = Math.floor(steps.length * r.progress);
    r.status = 'running';
    this.sims.set(r.id, { runId: r.id, steps, idx, nextIn: 500, waiting: null, speed: 1 });
  }

  private log(r: Run, kind: LogKind, text: string, data: unknown = null) {
    const arr = (this.s.logs[r.id] ??= []);
    const line: RunLogLine = { run_id: r.id, seq: arr.length, ts: now(), kind, text, data };
    arr.push(line);
    this.broadcast({ type: 'run_log', line });
  }

  private tick(dt: number) {
    for (const sim of [...this.sims.values()]) {
      const r = this.s.runs.find((x) => x.id === sim.runId);
      if (!r) {
        this.sims.delete(sim.runId);
        continue;
      }
      if (sim.waiting) continue;
      sim.nextIn -= dt * sim.speed * this.speed;
      if (sim.nextIn > 0) continue;
      const step = sim.steps[sim.idx];
      if (!step) {
        this.sims.delete(r.id);
        this.complete(r);
        continue;
      }
      sim.idx++;
      sim.nextIn = step.wait ?? 900;
      if (step.kind === 'permission') {
        const reqId = mkId('perm');
        sim.waiting = reqId;
        r.status = 'waiting';
        r.activity = `Waiting for permission to run ${step.tool}`;
        this.log(r, 'system', `Permission needed: ${step.tool} ${step.text}`, { permission_request: { request_id: reqId, tool: step.tool, input: step.input } });
        this.upsert({ entity: 'run', data: r });
        const a = this.agent(r.agent_id);
        if (a) {
          a.status = 'waiting';
          this.upsert({ entity: 'agent', data: a });
        }
        this.notify(r.started_by, { kind: 'run_waiting', message: `${a?.name ?? 'An agent'} needs your permission to run ${step.tool} on T-${this.ticket(r.ticket_id)?.number ?? '?'}`, ticket_id: r.ticket_id, run_id: r.id, actor_agent_id: r.agent_id });
        this.hist('run_waiting', `${a?.name ?? 'An agent'} needs permission to run ${step.tool}`, { actor_kind: 'agent', actor_id: r.agent_id, agent_id: r.agent_id, ticket_id: r.ticket_id, run_id: r.id });
        continue;
      }
      if (step.kind === 'fail') {
        this.log(r, 'error', step.text);
        this.sims.delete(r.id);
        this.finish(r, 'failed', step.text);
        continue;
      }
      const a = this.agent(r.agent_id);
      if (a && a.status === 'waiting') {
        a.status = r.kind === 'review' ? 'reviewing' : 'working';
        this.upsert({ entity: 'agent', data: a });
      }
      this.log(r, step.kind, step.text, step.data ?? null);
      r.progress = Math.min(0.99, sim.idx / sim.steps.length);
      if (step.activity) r.activity = step.activity;
      r.input_tokens += 600 + Math.floor(Math.random() * 2400);
      r.output_tokens += 80 + Math.floor(Math.random() * 500);
      r.cost_usd = +(r.input_tokens * 0.000003 + r.output_tokens * 0.000015).toFixed(4);
      if (step.verdict) r.verdict = step.verdict;
      this.upsert({ entity: 'run', data: r });
    }
  }

  private addXp(a: Agent, amount: number) {
    a.xp += amount;
    this.fx({ fx: 'xp_gained', agent_id: a.id, amount });
    while (a.xp >= xpForNextLevel(a.level)) {
      a.xp -= xpForNextLevel(a.level);
      a.level++;
      this.fx({ fx: 'level_up', agent_id: a.id, level: a.level });
      this.hist('agent_level_up', `${a.name} reached level ${a.level}!`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id });
    }
  }

  private release(a: Agent | undefined) {
    if (!a) return;
    a.status = 'idle';
    a.current_run_id = null;
    this.upsert({ entity: 'agent', data: a });
  }

  private finish(r: Run, status: 'failed' | 'cancelled', error: string | null = null) {
    this.finishInner(r, status, error);
    this.afterRun(r);
  }

  private finishInner(r: Run, status: 'failed' | 'cancelled', error: string | null = null) {
    this.sims.delete(r.id);
    r.status = status;
    r.ended_at = now();
    r.error = error;
    r.activity = status === 'cancelled' ? 'Cancelled' : 'Failed';
    const t = this.ticket(r.ticket_id);
    const a = this.agent(r.agent_id);
    this.release(a);
    if (t && r.kind === 'work') {
      const todo = this.firstCol(t.tab_id, 'todo');
      if (todo) {
        t.column_id = todo.id;
        t.position = this.endPos(todo.id);
      }
      if (status === 'failed') t.review_feedback = `Run failed: ${error}`;
      t.updated_at = now();
      this.upsert({ entity: 'ticket', data: t });
    }
    this.upsert({ entity: 'run', data: r });
    if (status === 'failed') {
      if (r.ticket_id) this.fx({ fx: 'run_failed', agent_id: r.agent_id, ticket_id: r.ticket_id });
      this.hist('run_failed', `${a?.name} failed on T-${t?.number}: ${error}`, { actor_kind: 'agent', actor_id: r.agent_id, agent_id: r.agent_id, ticket_id: r.ticket_id, run_id: r.id });
    } else if (r.kind === 'job') {
      this.hist('run_cancelled', `${this.me()?.name ?? 'Someone'} stopped the quest run of ${a?.name}`, { agent_id: r.agent_id, run_id: r.id });
    } else {
      this.hist('run_cancelled', `${this.me()?.name ?? 'Someone'} called ${a?.name} back from T-${t?.number}`, { agent_id: r.agent_id, ticket_id: r.ticket_id, run_id: r.id });
    }
    this.autoReview();
  }

  private complete(r: Run) {
    this.completeInner(r);
    this.afterRun(r);
  }

  private completeInner(r: Run) {
    if (r.kind === 'job') return this.completeJob(r);
    if (r.kind === 'command') return this.completeCommand(r);
    const t = this.ticket(r.ticket_id);
    const a = this.agent(r.agent_id);
    r.status = 'succeeded';
    r.progress = 1;
    r.ended_at = now();
    this.s.stats.runs_completed++;
    this.s.stats.total_cost_usd = +(this.s.stats.total_cost_usd + r.cost_usd).toFixed(4);
    if (!t || !a) {
      this.upsert({ entity: 'run', data: r });
      return;
    }
    a.runs_completed++;
    if (r.kind === 'work') {
      if (!t.pr_number) {
        t.pr_number = ++this.s.prSeq;
        const repo = this.s.repos.find((x) => x.id === (t.repo_id ?? this.tab(t.tab_id)?.repo_id));
        t.pr_url = `https://github.com/${repo?.name ?? 'boba-labs/app'}/pull/${t.pr_number}`;
        this.hist('pr_opened', `${a.name} opened PR #${t.pr_number} for T-${t.number}`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id, ticket_id: t.id, run_id: r.id });
      }
      r.summary = `Implemented "${t.title}", added tests, opened PR #${t.pr_number}.`;
      r.activity = 'Pull request opened';
      t.review_feedback = null;
      const review = this.firstCol(t.tab_id, 'review') ?? this.firstCol(t.tab_id, 'done');
      if (review) {
        t.column_id = review.id;
        t.position = this.endPos(review.id);
      }
      t.updated_at = now();
      this.upsert({ entity: 'run', data: r });
      this.upsert({ entity: 'ticket', data: t });
      this.release(a);
      this.hist('run_succeeded', `${a.name} finished T-${t.number} ${t.title}`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id, ticket_id: t.id, run_id: r.id, data: { cost_usd: r.cost_usd } });
      this.fx({ fx: 'work_finished', agent_id: a.id, ticket_id: t.id });
      this.addXp(a, 40 + Math.round(Math.random() * 20));
      this.upsert({ entity: 'agent', data: a });
      setTimeout(() => this.autoReview(), 1200);
      return;
    }
    // review
    const approved = r.verdict ? r.verdict === 'approved' : Math.random() < (t.review_rounds ? 0.9 : 0.7);
    r.verdict = approved ? 'approved' : 'changes_requested';
    r.activity = approved ? 'Approved' : 'Changes requested';
    const col = this.column(t.column_id);
    this.release(a);
    this.addXp(a, 30);
    this.upsert({ entity: 'agent', data: a });
    if (approved) {
      r.summary = `Approved PR #${t.pr_number}. Clean diff, tests cover the change.`;
      const to = (col?.on_approve_column_id && this.column(col.on_approve_column_id)) || this.firstCol(t.tab_id, 'done');
      if (to) {
        t.column_id = to.id;
        t.position = this.endPos(to.id);
      }
      t.review_feedback = null;
      const author = this.agent(t.agent_id);
      const reward = 10 + (t.review_rounds === 0 ? 5 : 0) + (author ? author.level * 2 : 0);
      this.s.stats.pearls += reward;
      this.s.stats.prs_approved++;
      t.updated_at = now();
      this.upsert({ entity: 'run', data: r });
      this.upsert({ entity: 'ticket', data: t });
      this.hist('review_approved', `${a.name} approved PR #${t.pr_number} (+${reward} pearls)`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id, ticket_id: t.id, run_id: r.id, data: { pearls: reward } });
      this.fx({ fx: 'pr_approved', agent_id: author?.id ?? a.id, ticket_id: t.id, reviewer_id: a.id });
      if (author) {
        author.prs_approved++;
        this.addXp(author, 20);
        this.upsert({ entity: 'agent', data: author });
      }
      this.fx({ fx: 'pearls_earned', ticket_id: t.id, amount: reward, total: this.s.stats.pearls });
    } else {
      const fb = pick([
        'Missing a test for the empty input case.',
        'A console.log was left in src/api/client.ts:42, and one variable is misnamed.',
        'The diff touches a file outside the ticket scope (package-lock.json).',
        'No error handling on the network call in src/fees.ts:120.',
      ]);
      r.summary = `Changes requested: ${fb}`;
      t.review_feedback = fb;
      t.review_rounds++;
      const to = (col?.on_changes_column_id && this.column(col.on_changes_column_id)) || this.firstCol(t.tab_id, 'todo');
      if (to) {
        t.column_id = to.id;
        t.position = this.endPos(to.id);
      }
      t.updated_at = now();
      this.upsert({ entity: 'run', data: r });
      this.upsert({ entity: 'ticket', data: t });
      this.hist('review_changes_requested', `${a.name} requested changes on PR #${t.pr_number}: ${fb}`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id, ticket_id: t.id, run_id: r.id, data: { feedback: fb } });
      this.fx({ fx: 'changes_requested', ticket_id: t.id, reviewer_id: a.id });
    }
    this.autoReview();
  }

  /** Tickets waiting in an auto-review column get picked up by their idle reviewer, oldest first. */
  private autoReview() {
    const waiting = this.s.tickets
      .filter((t) => {
        const c = this.column(t.column_id);
        return c?.kind === 'review' && c.auto_reviewer_agent_id && !this.activeRun(t.id);
      })
      .filter((t) => {
        const last = this.s.runs.filter((r) => r.ticket_id === t.id).sort((a, b) => (a.created_at < b.created_at ? 1 : -1))[0];
        // Don't re-review what was just reviewed in place.
        return !(last && last.kind === 'review' && last.status === 'succeeded' && last.ended_at && last.ended_at > t.updated_at);
      })
      .sort((a, b) => (a.updated_at < b.updated_at ? -1 : 1));
    for (const t of waiting) {
      const c = this.column(t.column_id);
      const rev = this.agent(c?.auto_reviewer_agent_id);
      if (rev && rev.status === 'idle') this.startRun(t, rev, 'review', null, null);
    }
  }

  // ---------------------------------------------------------------- automation (jobs, portals, sync, concierge)
  private nextAt(s: Schedule): string | null {
    return nextRuns(s, new Date(), 1)[0]?.toISOString() ?? null;
  }

  private routeAutomation(method: string, path: string, b: Record<string, unknown>): unknown {
    const m = (re: RegExp) => path.match(re);
    let x: RegExpMatchArray | null;
    const key = `${method} ${path}`;
    const me = () => this.me()?.name ?? 'Someone';

    // repos
    if ((x = m(/^\/api\/repos\/([^/]+)$/)) && method === 'PATCH') {
      this.need('manage_automations');
      const r = this.s.repos.find((y) => y.id === x![1]);
      if (!r) throw new ApiFailure(404, 'Repo not found.', 'not_found');
      const u = b as Partial<UpdateRepo>;
      if (u.name != null) r.name = u.name;
      if (u.path != null) r.path = u.path;
      if (u.remote_url !== undefined) r.remote_url = u.remote_url;
      if (u.default_branch != null) r.default_branch = u.default_branch;
      if (u.sync) r.sync = { ...u.sync, last_synced_at: r.sync.last_synced_at, last_error: r.sync.last_error };
      this.upsert({ entity: 'repo', data: r });
      return r;
    }
    if ((x = m(/^\/api\/repos\/([^/]+)\/sync$/)) && method === 'POST') {
      this.need('run_agents');
      const r = this.s.repos.find((y) => y.id === x![1]);
      if (!r) throw new ApiFailure(404, 'Repo not found.', 'not_found');
      return this.syncRepo(r.id);
    }

    // connectors
    if (key === 'POST /api/connectors') {
      this.need('manage_automations');
      const c = b as unknown as CreateConnector;
      if (!c.name?.trim()) throw new ApiFailure(400, 'A portal needs a name.', 'bad_request');
      const cn: Connector = { id: mkId('cn'), name: c.name.trim(), kind: c.kind, transport: c.transport, secret_keys: Object.entries(c.secrets ?? {}).filter(([, v]) => !!v).map(([k]) => k), allowed_tools: c.allowed_tools ?? [], created_by: this.me()?.id ?? null, created_at: now() };
      this.s.connectors.push(cn);
      this.upsert({ entity: 'connector', data: cn });
      this.hist('connector_created', `${me()} opened the portal ${cn.name}`);
      return cn;
    }
    if ((x = m(/^\/api\/connectors\/([^/]+)(?:\/(test))?$/))) {
      this.need('manage_automations');
      const cn = this.s.connectors.find((y) => y.id === x![1]);
      if (!cn) throw new ApiFailure(404, 'Portal not found.', 'not_found');
      if (x[2] === 'test') {
        const broken = (cn.transport.type === 'stdio' && !cn.transport.command.trim()) || (cn.transport.type === 'http' && !/^https?:\/\//.test(cn.transport.url));
        return broken ? { ok: false, tools: [], error: 'Could not start the MCP server: the command / URL is empty or invalid.' } : { ok: true, tools: (cn.allowed_tools.length ? cn.allowed_tools : TOOLS[cn.kind]), error: null };
      }
      if (method === 'PATCH') {
        const u = b as Partial<UpdateConnector>;
        if (u.name != null) cn.name = u.name;
        if (u.transport) cn.transport = u.transport;
        if (u.allowed_tools) cn.allowed_tools = u.allowed_tools;
        if (u.secrets) {
          const keys = new Set(cn.secret_keys);
          for (const [k, v] of Object.entries(u.secrets)) {
            if (v) keys.add(k);
            else keys.delete(k);
          }
          cn.secret_keys = [...keys];
        }
        this.upsert({ entity: 'connector', data: cn });
        this.hist('connector_updated', `${me()} tuned the portal ${cn.name}`);
        return cn;
      }
      if (method === 'DELETE') {
        this.s.connectors = this.s.connectors.filter((y) => y !== cn);
        for (const a of this.s.agents)
          if (a.connector_ids.includes(cn.id)) {
            a.connector_ids = a.connector_ids.filter((i) => i !== cn.id);
            this.upsert({ entity: 'agent', data: a });
          }
        this.del('connector', cn.id);
        this.hist('connector_deleted', `${me()} closed the portal ${cn.name}`);
        return undefined;
      }
    }

    // jobs
    if (key === 'POST /api/jobs/preview-schedule') {
      this.need('view');
      const s = b as unknown as Schedule;
      const err = s.type === 'cron' ? validateCron(s.expr) : s.type === 'interval' && s.minutes < 5 ? 'The shortest interval is 5 minutes.' : null;
      return { valid: !err, error: err, next: err ? [] : nextRuns(s, new Date(), 5).map((d) => d.toISOString()), description: err ? '' : describeSchedule(s) };
    }
    if (key === 'POST /api/jobs') {
      this.need('manage_automations');
      const c = b as unknown as CreateJob;
      if (!c.name?.trim()) throw new ApiFailure(400, 'A quest needs a name.', 'bad_request');
      if (!this.agent(c.agent_id)) throw new ApiFailure(400, 'Pick an agent for this quest.', 'bad_request');
      if (c.output === 'pull_request' && !c.repo_id) throw new ApiFailure(400, 'A pull request quest needs a repository.', 'bad_request');
      const j: Job = { id: mkId('job'), name: c.name.trim(), description: c.description ?? '', icon: c.icon || '📜', agent_id: c.agent_id, prompt: c.prompt, schedule: c.schedule, output: c.output, enabled: c.enabled ?? true, repo_id: c.repo_id, tab_id: c.tab_id, skip_if_running: c.skip_if_running ?? true, last_run_id: null, last_run_at: null, next_run_at: null, last_result: null, runs_count: 0, created_by: this.me()?.id ?? null, created_at: now() };
      j.next_run_at = j.enabled ? this.nextAt(j.schedule) : null;
      this.s.jobs.push(j);
      this.upsert({ entity: 'job', data: j });
      this.hist('job_created', `${me()} pinned the quest "${j.name}" for ${this.agent(j.agent_id)?.name}`, { agent_id: j.agent_id });
      return j;
    }
    if ((x = m(/^\/api\/jobs\/([^/]+)(?:\/(run|runs))?$/))) {
      const j = this.s.jobs.find((y) => y.id === x![1]);
      if (!j) throw new ApiFailure(404, 'Quest not found.', 'not_found');
      if (x[2] === 'runs') {
        this.need('view');
        return this.s.runs.filter((r) => r.job_id === j.id).sort((a, b2) => (a.created_at < b2.created_at ? 1 : -1));
      }
      if (x[2] === 'run') {
        this.need('run_agents');
        return this.startJobRun(j, this.me()?.id ?? null);
      }
      this.need('manage_automations');
      if (method === 'PATCH') {
        for (const [k, v] of Object.entries(b as Partial<UpdateJob>)) if (v !== undefined) (j as Record<string, unknown>)[k] = v;
        j.next_run_at = j.enabled ? this.nextAt(j.schedule) : null;
        this.upsert({ entity: 'job', data: j });
        this.hist('job_updated', `${me()} updated the quest "${j.name}"`, { agent_id: j.agent_id });
        return j;
      }
      if (method === 'DELETE') {
        this.s.jobs = this.s.jobs.filter((y) => y !== j);
        this.del('job', j.id);
        this.hist('job_deleted', `${me()} tore down the quest "${j.name}"`);
        return undefined;
      }
    }

    // concierge
    if (key === 'POST /api/command') {
      this.need('view');
      const text = String(b.text ?? '').trim();
      if (!text) throw new ApiFailure(400, 'Say something first.', 'bad_request');
      const r: Run = {
        id: mkId('run'), ticket_id: null, job_id: null, agent_id: 'concierge', kind: 'command', status: 'running', runner_id: 'local', branch: null, worktree_path: null,
        session_id: mkId('sess'), progress: 0, activity: 'Listening', summary: null, verdict: null, cost_usd: 0, input_tokens: 0, output_tokens: 0,
        started_by: this.me()?.id ?? null, created_at: now(), started_at: now(), ended_at: null, error: null,
      };
      this.s.runs.push(r);
      this.pendingCommands.set(r.id, { text, tab_id: (b.tab_id as string) ?? null, ticket_id: (b.ticket_id as string) ?? null, agent_id: (b.agent_id as string) ?? null });
      this.upsert({ entity: 'run', data: r });
      this.hist('command_issued', `${me()} asked the Concierge${b.voice ? ' (voice)' : ''}: "${text}"`, { run_id: r.id });
      const steps: Step[] = [
        { kind: 'system', text: 'Concierge · read-only · board digest loaded', activity: 'Reading the board', wait: 500 },
        { kind: 'thinking', text: `Resolving names and ids for: ${text}`, activity: 'Thinking', wait: 900 },
      ];
      this.sims.set(r.id, { runId: r.id, steps, idx: 0, nextIn: 200, waiting: null, speed: 1 });
      return r;
    }
    if (key === 'POST /api/command/execute') {
      const runId = String(b.run_id);
      const actions = (b.actions ?? []) as CommandAction[];
      const results = actions.map((a) => {
        try {
          return { ok: true, error: null, entity: this.executeAction(a) };
        } catch (e) {
          return { ok: false, error: e instanceof Error ? e.message : String(e), entity: null };
        }
      });
      const ok = results.filter((r) => r.ok).length;
      this.hist('command_executed', `${me()} confirmed ${ok}/${actions.length} Concierge action${actions.length > 1 ? 's' : ''}`, { run_id: runId });
      this.plans.delete(runId);
      return results;
    }
    if (key === 'POST /api/speech/transcribe') throw new ApiFailure(400, 'The demo uses your browser for speech.', 'bad_request');
    return NOPE;
  }

  private executeAction(a: CommandAction): Entity | null {
    const call = <T>(method: string, path: string, body?: unknown) => this.route(method, path, new URLSearchParams(), (body ?? {}) as Record<string, unknown>) as T;
    switch (a.action) {
      case 'create_ticket':
        return { entity: 'ticket', data: call<Ticket>('POST', '/api/tickets', a.ticket) };
      case 'update_ticket':
        return { entity: 'ticket', data: call<Ticket>('PATCH', `/api/tickets/${a.ticket_id}`, a.patch) };
      case 'move_ticket': {
        const pos = this.endPos(a.column_id);
        return { entity: 'ticket', data: call<Ticket>('POST', `/api/tickets/${a.ticket_id}/move`, { column_id: a.column_id, position: pos }) };
      }
      case 'assign_agent':
        return { entity: 'run', data: call<Run>('POST', `/api/tickets/${a.ticket_id}/assign`, a.assign) };
      case 'create_agent':
        return { entity: 'agent', data: call<Agent>('POST', '/api/agents', a.agent) };
      case 'update_agent':
        return { entity: 'agent', data: call<Agent>('PATCH', `/api/agents/${a.agent_id}`, a.patch) };
      case 'create_job':
        return { entity: 'job', data: call<Job>('POST', '/api/jobs', a.job) };
      case 'update_job':
        return { entity: 'job', data: call<Job>('PATCH', `/api/jobs/${a.job_id}`, a.patch) };
      case 'run_job':
        return { entity: 'run', data: call<Run>('POST', `/api/jobs/${a.job_id}/run`) };
      case 'create_tab':
        return { entity: 'tab', data: call<Tab>('POST', '/api/tabs', a.tab) };
      case 'sync_repo':
        call('POST', `/api/repos/${a.repo_id}/sync`);
        return null;
      case 'cancel_run':
        return { entity: 'run', data: call<Run>('POST', `/api/runs/${a.run_id}/cancel`) };
    }
  }

  private completeCommand(r: Run) {
    r.status = 'succeeded';
    r.progress = 1;
    r.ended_at = now();
    const req = this.pendingCommands.get(r.id);
    this.pendingCommands.delete(r.id);
    const me = this.me();
    const p = plan({
      text: req?.text ?? '',
      tab_id: req?.tab_id ?? null,
      ticket_id: req?.ticket_id ?? null,
      agent_id: req?.agent_id ?? null,
      tabs: this.s.tabs,
      agents: this.s.agents,
      tickets: this.s.tickets,
      jobs: this.s.jobs,
      repos: this.s.repos,
      grants: (perm) => grants(me?.role, perm),
    });
    r.summary = p.question ?? p.reply;
    r.activity = 'Plan ready';
    this.plans.set(r.id, p.actions);
    this.upsert({ entity: 'run', data: r });
    const cp: CommandPlan = { run_id: r.id, reply: p.reply, actions: p.actions, question: p.question };
    this.broadcast({ type: 'command_plan', plan: cp });
  }

  private startJobRun(j: Job, by: string | null): Run {
    const a = this.agent(j.agent_id);
    if (!a) throw new ApiFailure(400, 'This quest has no agent.', 'bad_request');
    if (a.status !== 'idle') throw new ApiFailure(409, `${a.name} is busy. Try again when it is back in the tavern.`, 'conflict');
    if (j.skip_if_running && this.s.runs.some((r) => r.job_id === j.id && (r.status === 'running' || r.status === 'waiting'))) throw new ApiFailure(409, 'This quest is already running.', 'conflict');
    const r: Run = {
      id: mkId('run'), ticket_id: null, job_id: j.id, agent_id: a.id, kind: 'job', status: 'running', runner_id: 'local', branch: j.output === 'pull_request' ? `boba/job-${j.name.toLowerCase().replace(/[^a-z0-9]+/g, '-')}` : null,
      worktree_path: null, session_id: mkId('sess'), progress: 0, activity: 'Reading the quest', summary: null, verdict: null, cost_usd: 0, input_tokens: 0, output_tokens: 0,
      started_by: by, created_at: now(), started_at: now(), ended_at: null, error: null,
    };
    this.s.runs.push(r);
    a.status = 'working';
    a.current_run_id = r.id;
    j.last_run_id = r.id;
    this.upsert({ entity: 'agent', data: a });
    this.upsert({ entity: 'run', data: r });
    this.upsert({ entity: 'job', data: j });
    const who = by ? this.s.users.find((u) => u.id === by)?.name ?? 'Someone' : 'The scheduler';
    this.hist('job_run_started', `${who} sent ${a.name} on the quest "${j.name}"`, { actor_kind: by ? 'user' : 'system', actor_id: by, agent_id: a.id, run_id: r.id });
    const cns = a.connector_ids.map((id) => this.s.connectors.find((c) => c.id === id)).filter((c): c is Connector => !!c);
    const { steps } = jobScript(j, a, cns);
    this.sims.set(r.id, { runId: r.id, steps, idx: 0, nextIn: 300, waiting: null, speed: 1 });
    return r;
  }

  private completeJob(r: Run) {
    const j = this.s.jobs.find((y) => y.id === r.job_id);
    const a = this.agent(r.agent_id);
    r.status = 'succeeded';
    r.progress = 1;
    r.ended_at = now();
    this.s.stats.runs_completed++;
    this.s.stats.total_cost_usd = +(this.s.stats.total_cost_usd + r.cost_usd).toFixed(4);
    if (j && a) {
      const cns = a.connector_ids.map((id) => this.s.connectors.find((c) => c.id === id)).filter((c): c is Connector => !!c);
      const out = jobScript(j, a, cns);
      for (const tk of out.tickets) {
        const tab = this.tab(j.tab_id ?? '') ?? this.s.tabs[0];
        const col = tab && (this.firstCol(tab.id, 'todo') ?? tab.columns[0]);
        if (!tab || !col) continue;
        const t: Ticket = {
          id: mkId('tk'), number: ++this.s.seq, tab_id: tab.id, column_id: col.id, position: this.endPos(col.id), title: tk.title, body: tk.body, tags: tk.tags, priority: 'normal', color: null, repo_id: null,
          agent_id: null, branch: null, pr_url: null, pr_number: null, review_feedback: null, review_rounds: 0, source: { kind: 'job', job_id: j.id, run_id: r.id }, comment_count: 0, open_thread_count: 0, created_by: null, created_at: now(), updated_at: now(),
        };
        this.s.tickets.push(t);
        this.upsert({ entity: 'ticket', data: t });
        this.hist('ticket_created', `${a.name} filed T-${t.number} ${t.title} (quest "${j.name}")`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id, ticket_id: t.id, run_id: r.id });
      }
      j.last_result = out.report;
      j.last_run_at = now();
      j.runs_count++;
      j.next_run_at = j.enabled ? this.nextAt(j.schedule) : null;
      r.summary = out.report.split('\n').find((l) => l.trim() && !l.startsWith('#')) ?? out.report;
      r.activity = 'Quest complete';
      a.runs_completed++;
      this.release(a);
      this.addXp(a, 25);
      this.upsert({ entity: 'agent', data: a });
      this.upsert({ entity: 'job', data: j });
      this.hist('job_run_finished', `${a.name} completed the quest "${j.name}": ${r.summary}`, { actor_kind: 'agent', actor_id: a.id, agent_id: a.id, run_id: r.id });
    }
    this.upsert({ entity: 'run', data: r });
  }

  private schedulerTick() {
    if (!this.me()) return;
    const t = new Date().toISOString();
    for (const j of this.s.jobs) {
      if (!j.enabled || !j.next_run_at || j.next_run_at > t) continue;
      try {
        this.startJobRun(j, null);
      } catch {
        // busy agent / already running: skip this tick
        j.next_run_at = this.nextAt(j.schedule);
        this.upsert({ entity: 'job', data: j });
      }
    }
  }

  private ghSeq = 1300;
  private syncRepo(repoId: string): SyncReport {
    const r = this.s.repos.find((y) => y.id === repoId);
    if (!r) throw new ApiFailure(404, 'Repo not found.', 'not_found');
    const report: SyncReport = { imported_issues: 0, imported_prs: 0, updated: 0, closed: 0, errors: [] };
    const tab = this.tab(r.sync.target_tab_id ?? '') ?? this.s.tabs.find((t) => t.repo_id === r.id);
    if (!tab) {
      report.errors.push('No target tab: choose where imported tickets go.');
    } else {
      const issues = [
        ['Dark mode flickers on first load', 'octo-lea', ['bug', 'ui']],
        ['Export trades as CSV', 'marta-dev', ['feature']],
        ['Docs: explain the fee tiers', 'nico42', ['docs']],
      ] as const;
      const pick = issues[Math.floor(Math.random() * issues.length)] as (typeof issues)[number];
      if (r.sync.import_issues && (!r.sync.issue_labels.length || r.sync.issue_labels.some((l) => (pick[2] as readonly string[]).includes(l)))) {
        const col = this.firstCol(tab.id, 'todo') ?? tab.columns[0];
        if (col) {
          const n = ++this.ghSeq;
          const t: Ticket = {
            id: mkId('tk'), number: ++this.s.seq, tab_id: tab.id, column_id: col.id, position: this.endPos(col.id), title: pick[0], body: `Imported from GitHub issue #${n} by @${pick[1]}.`, tags: [...pick[2]], priority: 'normal', color: null, repo_id: r.id,
            agent_id: null, branch: null, pr_url: null, pr_number: null, review_feedback: null, review_rounds: 0, source: { kind: 'github_issue', repo_id: r.id, number: n, url: `https://github.com/${r.name}/issues/${n}`, author: pick[1] }, comment_count: 0, open_thread_count: 0, created_by: null, created_at: now(), updated_at: now(),
          };
          this.s.tickets.push(t);
          this.upsert({ entity: 'ticket', data: t });
          this.hist('ticket_imported', `Imported issue #${n} "${t.title}" from ${r.name} as T-${t.number}`, { actor_kind: 'system', ticket_id: t.id });
          report.imported_issues++;
        }
      }
      if (r.sync.import_prs && Math.random() < 0.6) {
        const col = this.firstCol(tab.id, 'review');
        if (col) {
          const n = ++this.ghSeq;
          const t: Ticket = {
            id: mkId('tk'), number: ++this.s.seq, tab_id: tab.id, column_id: col.id, position: this.endPos(col.id), title: 'Add Turkish translation', body: `External PR #${n} by @ext-contrib.`, tags: ['i18n'], priority: 'normal', color: null, repo_id: r.id,
            agent_id: null, branch: 'i18n-tr', pr_url: `https://github.com/${r.name}/pull/${n}`, pr_number: n, review_feedback: null, review_rounds: 0, source: { kind: 'github_pr', repo_id: r.id, number: n, url: `https://github.com/${r.name}/pull/${n}`, author: 'ext-contrib', head_branch: 'i18n-tr' }, comment_count: 0, open_thread_count: 0, created_by: null, created_at: now(), updated_at: now(),
          };
          this.s.tickets.push(t);
          this.upsert({ entity: 'ticket', data: t });
          this.hist('ticket_imported', `Imported PR #${n} "${t.title}" from ${r.name} into ${col.name}`, { actor_kind: 'system', ticket_id: t.id });
          report.imported_prs++;
          setTimeout(() => this.autoReview(), 800);
        }
      }
      report.updated = Math.floor(Math.random() * 3);
    }
    r.sync.last_synced_at = now();
    r.sync.last_error = report.errors[0] ?? null;
    this.upsert({ entity: 'repo', data: r });
    this.hist('repo_synced', `Synced ${r.name}: ${report.imported_issues} issue(s), ${report.imported_prs} PR(s) imported`, { actor_kind: 'system' });
    return report;
  }

  // ---------------------------------------------------------------- teamwork (accounts, comments, notifications, runners)
  private mentionReplies = new Map<string, string>(); // run id -> thread root id

  private pickRunner(by: string | null): string {
    const mine = this.s.runners.find((r) => r.scope === 'personal' && r.owner_user_id === by && r.online);
    return mine?.id ?? 'local';
  }

  private notify(userId: string | null | undefined, n: Partial<Notification> & { kind: Notification['kind']; message: string }) {
    if (!userId) return;
    const full: Notification = { id: mkId('nt'), user_id: userId, ticket_id: null, comment_id: null, run_id: null, job_id: null, actor_user_id: null, actor_agent_id: null, read: false, created_at: now(), ...n };
    this.s.notifications.unshift(full);
    if (this.s.notifications.length > 300) this.s.notifications.length = 300;
    // Sent only to its owner: in the mock, only "me" has a socket.
    if (userId === this.me()?.id) this.upsert({ entity: 'notification', data: full });
    else this.save();
  }

  private afterRun(r: Run) {
    const a = this.agent(r.agent_id);
    const t = this.ticket(r.ticket_id);
    if (r.kind === 'work' || r.kind === 'review') {
      const what = r.status === 'succeeded' ? (r.kind === 'review' ? `${a?.name} ${r.verdict === 'approved' ? 'approved' : 'requested changes on'} T-${t?.number}` : `${a?.name} finished T-${t?.number}`) : `${a?.name} ${r.status === 'failed' ? 'failed' : 'was stopped'} on T-${t?.number}`;
      this.notify(r.started_by, { kind: r.status === 'failed' ? 'run_failed' : 'run_finished', message: what, ticket_id: r.ticket_id, run_id: r.id, actor_agent_id: r.agent_id });
      if (r.kind === 'review' && r.status === 'succeeded' && t?.created_by && t.created_by !== r.started_by)
        this.notify(t.created_by, { kind: 'review_verdict', message: `${a?.name} ${r.verdict === 'approved' ? 'approved' : 'requested changes on'} T-${t.number}`, ticket_id: t.id, run_id: r.id, actor_agent_id: r.agent_id });
    }
    if (r.kind === 'job' && r.status === 'succeeded') {
      const j = this.s.jobs.find((x) => x.id === r.job_id);
      if (j) this.notify(j.created_by, { kind: 'job_result', message: `Quest "${j.name}" finished: ${r.summary ?? 'done'}`, job_id: j.id, run_id: r.id, actor_agent_id: r.agent_id });
    }
    // The agent answers in the thread that mentioned it.
    const root = this.mentionReplies.get(r.id);
    if (root) {
      this.mentionReplies.delete(r.id);
      const rootC = this.s.comments.find((c) => c.id === root);
      if (rootC) {
        const body = r.status === 'succeeded' ? `Done ✅ ${r.summary ?? ''}`.trim() : r.status === 'failed' ? `I got stuck: ${r.error ?? 'unknown error'}` : 'Someone called me back before I finished.';
        const c: Comment = { ...this.blankComment(), ticket_id: rootC.ticket_id, job_id: rootC.job_id, anchor: rootC.anchor, parent_id: root, author_kind: 'agent', author_id: r.agent_id, body };
        this.s.comments.push(c);
        this.upsert({ entity: 'comment', data: c });
        this.recount(c.ticket_id);
        for (const uid of this.threadPeople(root)) this.notify(uid, { kind: 'reply', message: `${a?.name} replied in a thread on T-${t?.number}`, ticket_id: c.ticket_id, comment_id: c.id, actor_agent_id: r.agent_id });
      }
    }
  }

  /** Server-maintained badge counters on the ticket paper. */
  private recount(ticketId: string | null, broadcast = true) {
    const t = this.ticket(ticketId);
    if (!t) return;
    const cs = this.s.comments.filter((c) => c.ticket_id === t.id && !c.deleted);
    const next = { comment_count: cs.length, open_thread_count: cs.filter((c) => !c.parent_id && !c.resolved).length };
    if (next.comment_count === t.comment_count && next.open_thread_count === t.open_thread_count) return;
    Object.assign(t, next);
    if (broadcast) this.upsert({ entity: 'ticket', data: t });
  }

  private blankComment(): Comment {
    return { id: mkId('cm'), ticket_id: null, job_id: null, anchor: { on: 'ticket' }, parent_id: null, author_kind: 'user', author_id: this.me()?.id ?? '', body: '', mentioned_user_ids: [], mentioned_agent_ids: [], triggered_run_ids: [], reactions: [], resolved: false, resolved_by: null, created_at: now(), edited_at: null, deleted: false };
  }

  private threadPeople(rootId: string): string[] {
    const ids = new Set<string>();
    for (const c of this.s.comments) if ((c.id === rootId || c.parent_id === rootId) && c.author_kind === 'user') ids.add(c.author_id);
    return [...ids];
  }

  private postComment(authorId: string, b: CreateComment): Comment {
    const author = this.s.users.find((u) => u.id === authorId);
    const body = String(b.body ?? '').trim();
    if (!body) throw new ApiFailure(400, 'Write something first.', 'bad_request');
    const t = this.ticket(b.ticket_id);
    if (b.ticket_id && !t) throw new ApiFailure(404, 'Ticket not found.', 'not_found');
    const users = new Set<string>();
    const agents = new Set<string>();
    for (const m of body.matchAll(/@\[[^\]]+\]\((user|agent):([^)]+)\)/g)) (m[1] === 'user' ? users : agents).add(m[2] as string);
    const c: Comment = { ...this.blankComment(), author_id: authorId, ticket_id: b.ticket_id, job_id: b.job_id, anchor: b.anchor, parent_id: b.parent_id, body, mentioned_user_ids: [...users], mentioned_agent_ids: [...agents] };
    const root = b.parent_id ?? c.id;
    // Agent mentions start runs (needs RunAgents), with the comment as instructions.
    if (t && agents.size && grants(author?.role, 'run_agents')) {
      for (const aid of agents) {
        try {
          const run = this.assign(t, aid, body.replace(/@\[([^\]]+)\]\([^)]+\)/g, '@$1'), authorId);
          c.triggered_run_ids.push(run.id);
          this.mentionReplies.set(run.id, root);
        } catch (e) {
          // Busy agent: say so in the thread right away.
          const a = this.agent(aid);
          setTimeout(() => {
            const reply: Comment = { ...this.blankComment(), ticket_id: c.ticket_id, anchor: c.anchor, parent_id: root, author_kind: 'agent', author_id: aid, body: `I'm busy right now (${e instanceof Error ? e.message : 'busy'}). Mention me again when I'm back in the tavern.` };
            this.s.comments.push(reply);
            this.upsert({ entity: 'comment', data: reply });
          }, 600);
          void a;
        }
      }
    }
    this.s.comments.push(c);
    this.upsert({ entity: 'comment', data: c });
    this.recount(c.ticket_id);
    const where = t ? `T-${t.number} ${t.title}` : 'a quest';
    for (const uid of users) if (uid !== authorId) this.notify(uid, { kind: 'mention', message: `${author?.name ?? 'Someone'} mentioned you on ${where}`, ticket_id: c.ticket_id, job_id: c.job_id, comment_id: c.id, actor_user_id: authorId });
    if (b.parent_id) for (const uid of this.threadPeople(b.parent_id)) if (uid !== authorId && !users.has(uid)) this.notify(uid, { kind: 'reply', message: `${author?.name ?? 'Someone'} replied in a thread on ${where}`, ticket_id: c.ticket_id, job_id: c.job_id, comment_id: c.id, actor_user_id: authorId });
    this.hist('comment', `${author?.name ?? 'Someone'} commented on ${where}`, { actor_id: authorId, ticket_id: c.ticket_id, data: { comment_id: c.id } });
    return c;
  }

  private routeTeam(method: string, path: string, q: URLSearchParams, b: Record<string, unknown>): unknown {
    const m = (re: RegExp) => path.match(re);
    let x: RegExpMatchArray | null;
    const key = `${method} ${path}`;

    if (key === 'PATCH /api/me') {
      const me = this.me();
      if (!me) throw new ApiFailure(401, 'Please log in first.', 'unauthorized');
      const u = b as Partial<UpdateMe>;
      if (u.new_password && !u.current_password) throw new ApiFailure(400, 'Type your current password to change it.', 'bad_request');
      if (u.name) me.name = u.name.trim();
      if (u.avatar) me.avatar = u.avatar;
      this.upsert({ entity: 'user', data: this.pub(me) });
      this.presence();
      return this.pub(me);
    }
    if ((x = m(/^\/api\/tickets\/([^/]+)\/comments$/)) && method === 'GET') {
      this.need('view');
      return this.s.comments.filter((c) => c.ticket_id === x![1]).sort((a, b2) => a.created_at.localeCompare(b2.created_at));
    }
    if (key === 'POST /api/comments') {
      const me = this.need('edit_tickets');
      return this.postComment(me.id, b as unknown as CreateComment);
    }
    if ((x = m(/^\/api\/comments\/([^/]+)(?:\/(react))?$/))) {
      const me = this.need('view');
      const c = this.s.comments.find((y) => y.id === x![1]);
      if (!c) throw new ApiFailure(404, 'Comment not found.', 'not_found');
      if (x[2] === 'react') {
        const emoji = String(b.emoji ?? '👍');
        let r = c.reactions.find((y) => y.emoji === emoji);
        if (!r) c.reactions.push((r = { emoji, user_ids: [] }));
        r.user_ids = r.user_ids.includes(me.id) ? r.user_ids.filter((u) => u !== me.id) : [...r.user_ids, me.id];
        c.reactions = c.reactions.filter((y) => y.user_ids.length);
        this.upsert({ entity: 'comment', data: c });
        return c;
      }
      if (method === 'PATCH') {
        const u = b as Partial<UpdateComment>;
        if (u.body != null) {
          if (c.author_id !== me.id) throw new ApiFailure(403, 'You can only edit your own comments.', 'forbidden');
          c.body = u.body;
          c.edited_at = now();
        }
        if (u.resolved != null) {
          this.need('edit_tickets');
          c.resolved = u.resolved;
          c.resolved_by = u.resolved ? me.id : null;
        }
        this.upsert({ entity: 'comment', data: c });
        this.recount(c.ticket_id);
        return c;
      }
      if (method === 'DELETE') {
        if (c.author_id !== me.id && !grants(me.role, 'manage_users')) throw new ApiFailure(403, 'You can only delete your own comments.', 'forbidden');
        c.deleted = true;
        c.body = '';
        this.upsert({ entity: 'comment', data: c });
        this.recount(c.ticket_id);
        return undefined;
      }
    }
    if (key === 'GET /api/notifications') {
      const me = this.need('view');
      const before = q.get('before');
      return this.s.notifications.filter((n) => n.user_id === me.id && (!before || n.created_at < before)).slice(0, Number(q.get('limit') ?? 50));
    }
    if (key === 'POST /api/notifications/read') {
      const me = this.need('view');
      const ids = (b.ids as string[] | null) ?? null;
      for (const n of this.s.notifications) if (n.user_id === me.id && (!ids || ids.includes(n.id))) n.read = true;
      this.save();
      return undefined;
    }
    if (key === 'POST /api/runners') {
      const me = this.need('view');
      const c = b as unknown as CreateRunner;
      if (c.scope === 'shared') this.need('manage_workspace');
      else this.need('run_agents');
      const r: RunnerInfo = { id: mkId('rn'), name: c.name, online: false, max_concurrent_runs: c.max_concurrent_runs ?? 2, active_runs: 0, last_seen_at: null, scope: c.scope, owner_user_id: c.scope === 'personal' ? me.id : null, auth: 'unknown', host: null, claude_version: null };
      this.s.runners.push(r);
      this.upsert({ entity: 'runner', data: r });
      const token = `brt_${Math.random().toString(36).slice(2)}${Math.random().toString(36).slice(2)}`;
      // Simulate the machine connecting a few seconds later.
      setTimeout(() => {
        r.online = true;
        r.auth = c.scope === 'personal' ? 'subscription' : 'api_key';
        r.host = c.scope === 'personal' ? `${me.name.toLowerCase().replace(/[^a-z0-9]+/g, '-')}-laptop` : 'vps-2';
        r.claude_version = '2.1.319';
        r.last_seen_at = now();
        this.upsert({ entity: 'runner', data: r });
        this.notify(me.id, { kind: 'account', message: `${r.name} is connected. Your runs now use your own Claude Code.` });
      }, 4000);
      return { runner: r, token, command: `boba-agent runner --server ${location.origin} --token ${token}` };
    }
    if ((x = m(/^\/api\/runners\/([^/]+)$/)) && method === 'DELETE') {
      const me = this.need('view');
      const r = this.s.runners.find((y) => y.id === x![1]);
      if (!r) throw new ApiFailure(404, 'Runner not found.', 'not_found');
      if (r.owner_user_id !== me.id) this.need('manage_workspace');
      this.s.runners = this.s.runners.filter((y) => y !== r);
      this.del('runner', r.id);
      return undefined;
    }
    return NOPE;
  }

  // ---------------------------------------------------------------- demo teammates
  private botTick() {
    if (!this.botsOn) return;
    const b = pick(this.botPresence);
    if (Math.random() < 0.5) b.tab_id = pick(this.s.tabs).id;
    this.presence();
    if (Math.random() < 0.18) this.botAssign(false);
    else if (Math.random() < 0.08) this.botComment();
  }
  private botComment() {
    const me = this.me();
    const t = pick(this.s.tickets.filter((x) => x.tab_id === 'tab_dev'));
    if (!me || !t) return;
    const lines = [
      `@[${me.name}](user:${me.id}) can you take a look at this one? I think it blocks the release.`,
      `Looks good to me. @[${me.name}](user:${me.id}) ok to ship?`,
      `I added more context in the description, @[${me.name}](user:${me.id}).`,
    ];
    try {
      this.postComment('u_mika', { ticket_id: t.id, job_id: null, anchor: { on: 'ticket' }, parent_id: null, body: pick(lines) });
    } catch {
      /* ignore */
    }
  }

  private botAssign(force: boolean) {
    if (!this.botsOn || !this.me()) return;
    const busy = this.s.runs.filter((r) => r.status === 'running' || r.status === 'waiting').length;
    if (busy >= 2 && !force) return;
    const todo = this.s.tickets.filter((t) => this.column(t.column_id)?.kind === 'todo' && !this.activeRun(t.id) && !t.review_feedback);
    const idle = this.s.agents.filter((a) => a.status === 'idle' && a.role.toLowerCase() !== 'reviewer');
    if (!todo.length || !idle.length) return;
    const t = pick(todo);
    const a = pick(idle);
    const bot = pick(this.botPresence);
    bot.tab_id = t.tab_id;
    this.presence();
    try {
      this.assign(t, a.id, null, bot.user_id);
    } catch {
      /* raced with the user */
    }
  }
}

export const mockServer = new MockServer();

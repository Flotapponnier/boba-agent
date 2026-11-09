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

const STORE_KEY = 'boba-mock-v4';
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
    { user_id: 'u_mika', name: 'Mika', avatar: 'pinky_boba', tab_id: 'tab_dev' },
    { user_id: 'u_sam', name: 'Sam', avatar: 'golden_boba', tab_id: 'tab_seo' },
  ];
  speed = 1;

  constructor() {
    this.s = this.load();
    if (this.s.runs.length === 0 && this.s.users.length) this.seedRuns();
    for (const r of this.s.runs) if (r.status === 'running' || r.status === 'queued' || r.status === 'waiting') this.resume(r);
    setInterval(() => this.tick(250), 250);
    setInterval(() => this.botTick(), 9000);
    setTimeout(() => this.botAssign(true), 4500);
    setTimeout(() => this.autoReview(), 6000);
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
      repos: this.s.repos,
      users: this.s.users.map((u) => this.pub(u)),
      runners: this.s.runners.map((r) => ({ ...r, active_runs: this.sims.size, last_seen_at: now() })),
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
      const u: MockUser = { id: mkId('u'), name: String(b.name), email: String(b.email), role: b.role as User['role'], avatar: pick(['boba', 'pinky_boba', 'golden_boba', 'black_boba', 'boba_diamond']), created_at: now(), activated: false };
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
      const r = { id: mkId('r'), name: c.name, path: c.path, remote_url: c.remote_url ?? null, default_branch: c.default_branch || 'main', runner_id: c.runner_id ?? null, sync: c.sync ?? defaultSync() };
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
      runner_id: 'local',
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
    if (!t || !a) return;
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
    } else {
      this.hist('run_cancelled', `${this.me()?.name ?? 'Someone'} called ${a?.name} back from T-${t?.number}`, { agent_id: r.agent_id, ticket_id: r.ticket_id, run_id: r.id });
    }
    this.autoReview();
  }

  private complete(r: Run) {
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

  // ---------------------------------------------------------------- demo teammates
  private botTick() {
    if (!this.botsOn) return;
    const b = pick(this.botPresence);
    if (Math.random() < 0.5) b.tab_id = pick(this.s.tabs).id;
    this.presence();
    if (Math.random() < 0.18) this.botAssign(false);
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

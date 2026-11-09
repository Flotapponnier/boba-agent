import { create } from 'zustand';
import type {
  Agent,
  Comment,
  Connector,
  Notification,
  Job,
  Entity,
  EntityKind,
  HistoryEvent,
  PresenceUser,
  Repo,
  Run,
  RunLogLine,
  RunnerInfo,
  ServerMessage,
  Skill,
  Snapshot,
  Tab,
  Ticket,
  User,
  WorkspaceSettings,
  WorkspaceStats,
} from '../api/types';
import { fxBus, notifyBus, planBus, toast } from './bus';

export type ConnState = 'idle' | 'connecting' | 'online' | 'offline';

type Dict<T> = Record<string, T>;

export interface EntityMaps {
  tabs: Dict<Tab>;
  agents: Dict<Agent>;
  tickets: Dict<Ticket>;
  runs: Dict<Run>;
  skills: Dict<Skill>;
  repos: Dict<Repo>;
  users: Dict<User>;
  runners: Dict<RunnerInfo>;
  jobs: Dict<Job>;
  connectors: Dict<Connector>;
  comments: Dict<Comment>;
  notifications: Dict<Notification>;
}

const KEY: Record<EntityKind, keyof EntityMaps> = {
  tab: 'tabs',
  agent: 'agents',
  ticket: 'tickets',
  run: 'runs',
  skill: 'skills',
  repo: 'repos',
  user: 'users',
  runner: 'runners',
  job: 'jobs',
  connector: 'connectors',
  comment: 'comments',
  notification: 'notifications',
};

export interface BoardState extends EntityMaps {
  conn: ConnState;
  /** True once the first snapshot arrived. */
  ready: boolean;
  me: User | null;
  history: HistoryEvent[];
  logs: Dict<RunLogLine[]>;
  presence: PresenceUser[];
  settings: WorkspaceSettings | null;
  stats: WorkspaceStats | null;
  /** Bumped on every history event so history views can refetch lazily. */
  historyTick: number;

  setConn(c: ConnState): void;
  setMe(u: User | null): void;
  apply(msg: ServerMessage): void;
  loadSnapshot(s: Snapshot): void;
  upsert(item: Entity): void;
  remove(kind: EntityKind, id: string): void;
  mergeLogs(runId: string, lines: RunLogLine[]): void;
  mergeHistory(events: HistoryEvent[]): void;
  mergeComments(cs: Comment[]): void;
  reset(): void;
}

const byId = <T extends { id: string }>(xs: T[]): Dict<T> => {
  const o: Dict<T> = {};
  for (const x of xs) o[x.id] = x;
  return o;
};

const empty: EntityMaps = { tabs: {}, agents: {}, tickets: {}, runs: {}, skills: {}, repos: {}, users: {}, runners: {}, jobs: {}, connectors: {}, comments: {}, notifications: {} };

function mergeSortedLogs(cur: RunLogLine[] | undefined, add: RunLogLine[]): RunLogLine[] {
  if (!cur || cur.length === 0) return [...add].sort((a, b) => a.seq - b.seq);
  const last = cur[cur.length - 1] as RunLogLine;
  // Fast path: live append.
  if (add.length === 1 && (add[0] as RunLogLine).seq > last.seq) return [...cur, add[0] as RunLogLine];
  const m = new Map<number, RunLogLine>();
  for (const l of cur) m.set(l.seq, l);
  for (const l of add) m.set(l.seq, l);
  return [...m.values()].sort((a, b) => a.seq - b.seq);
}

export const useBoard = create<BoardState>()((set, get) => ({
  ...empty,
  conn: 'idle',
  ready: false,
  me: null,
  history: [],
  logs: {},
  presence: [],
  settings: null,
  stats: null,
  historyTick: 0,

  setConn: (conn) => set({ conn }),
  setMe: (me) => set({ me }),

  loadSnapshot: (s) =>
    set((st) => ({
      ready: true,
      me: s.me ?? st.me,
      tabs: byId(s.tabs),
      agents: byId(s.agents),
      tickets: byId(s.tickets),
      // Keep runs we already know (history views) but let the snapshot win.
      runs: { ...st.runs, ...byId(s.runs) },
      skills: byId(s.skills),
      repos: byId(s.repos),
      users: byId(s.users),
      runners: byId(s.runners),
      jobs: byId(s.jobs ?? []),
      connectors: byId(s.connectors ?? []),
      notifications: byId(s.notifications ?? []),
      history: s.history,
      settings: s.settings,
      stats: s.stats,
      historyTick: st.historyTick + 1,
    })),

  upsert: (item) =>
    set((st) => {
      const k = KEY[item.entity];
      const data = item.data as { id: string };
      if (item.entity === 'notification' && !st.notifications[item.data.id] && !item.data.read) {
        const n = item.data;
        queueMicrotask(() => notifyBus.emit(n));
      }
      if (item.entity === 'job') {
        const prev = st.jobs[item.data.id];
        if (prev && item.data.runs_count > prev.runs_count) queueMicrotask(() => fxBus.emit({ fx: 'quest_done', job_id: item.data.id }));
      }
      const patch: Partial<BoardState> = { [k]: { ...st[k], [data.id]: data } } as Partial<BoardState>;
      if (item.entity === 'user' && st.me && st.me.id === data.id) patch.me = item.data as User;
      return patch;
    }),

  remove: (kind, id) =>
    set((st) => {
      const k = KEY[kind];
      if (!(id in st[k])) return {};
      const next = { ...st[k] };
      delete next[id];
      return { [k]: next } as Partial<BoardState>;
    }),

  mergeLogs: (runId, lines) => {
    if (lines.length === 0) return;
    set((st) => ({ logs: { ...st.logs, [runId]: mergeSortedLogs(st.logs[runId], lines) } }));
  },

  mergeHistory: (events) =>
    set((st) => {
      const seen = new Set(st.history.map((e) => e.id));
      const fresh = events.filter((e) => !seen.has(e.id));
      if (fresh.length === 0) return {};
      const all = [...fresh, ...st.history].sort((a, b) => (a.ts < b.ts ? 1 : a.ts > b.ts ? -1 : 0));
      return { history: all.slice(0, 500), historyTick: st.historyTick + 1 };
    }),

  mergeComments: (cs) => {
    if (!cs.length) return;
    set((st) => {
      const next = { ...st.comments };
      for (const c of cs) next[c.id] = c;
      return { comments: next };
    });
  },

  apply: (msg) => {
    const st = get();
    switch (msg.type) {
      case 'snapshot':
        st.loadSnapshot(msg.state);
        break;
      case 'upsert':
        st.upsert(msg.item);
        break;
      case 'delete':
        st.remove(msg.kind, msg.id);
        break;
      case 'history':
        st.mergeHistory([msg.event]);
        break;
      case 'run_log':
        st.mergeLogs(msg.line.run_id, [msg.line]);
        break;
      case 'fx':
        if (msg.fx.fx === 'pearls_earned') {
          const total = msg.fx.total;
          set((s) => ({ stats: s.stats ? { ...s.stats, pearls: total } : s.stats }));
        }
        fxBus.emit(msg.fx);
        break;
      case 'presence':
        set({ presence: msg.users });
        break;
      case 'command_plan':
        planBus.emit(msg.plan);
        break;
      case 'error':
        toast(msg.message, 'bad');
        break;
    }
  },

  reset: () =>
    set({ ...empty, ready: false, history: [], logs: {}, presence: [], settings: null, stats: null }),
}));

/** Snapshot the previous value of an entity so an optimistic write can be rolled back. */
export function optimistic<K extends keyof EntityMaps>(
  key: K,
  id: string,
  next: EntityMaps[K][string] | null,
): () => void {
  const prev = useBoard.getState()[key][id];
  useBoard.setState((st) => {
    const m = { ...st[key] } as Dict<unknown>;
    if (next === null) delete m[id];
    else m[id] = next;
    return { [key]: m } as Partial<BoardState>;
  });
  return () =>
    useBoard.setState((st) => {
      const m = { ...st[key] } as Dict<unknown>;
      if (prev === undefined) delete m[id];
      else m[id] = prev;
      return { [key]: m } as Partial<BoardState>;
    });
}

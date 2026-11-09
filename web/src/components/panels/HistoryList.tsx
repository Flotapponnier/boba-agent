import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { HistoryEvent, HistoryKind, HistoryQuery } from '../../api/types';
import { avatarUrl, bodyUrl } from '../../lib/game';
import { clock, dayLabel, timeAgo } from '../../lib/format';
import { errorText } from '../../store/actions';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Icon } from '../ui/Icon';
import type { IconName } from '../../lib/icons';

export const KIND_ICON: Partial<Record<HistoryKind, IconName>> = {
  ticket_created: 'note',
  ticket_updated: 'edit',
  ticket_moved: 'arrow-right',
  ticket_deleted: 'trash',
  agent_created: 'user-plus',
  agent_updated: 'tools',
  agent_deleted: 'logout',
  agent_level_up: 'star',
  run_started: 'play',
  run_waiting: 'hand',
  run_succeeded: 'check',
  run_failed: 'warn',
  run_cancelled: 'close',
  pr_opened: 'pr',
  review_approved: 'check',
  review_changes_requested: 'refresh',
  skill_created: 'book',
  skill_updated: 'book',
  tab_created: 'layout',
  tab_updated: 'layout',
  tab_deleted: 'layout',
  user_joined: 'user-plus',
  comment: 'chat',
  command_issued: 'mic',
  command_executed: 'sparkle',
  job_created: 'quest',
  job_updated: 'quest',
  job_deleted: 'quest',
  job_run_started: 'sword',
  job_run_finished: 'trophy',
  ticket_imported: 'repo',
  repo_synced: 'refresh',
  connector_created: 'portal',
  connector_updated: 'portal',
  connector_deleted: 'portal',
};

export type Category = 'all' | 'tickets' | 'runs' | 'reviews' | 'agents' | 'automation' | 'team';
export const CATEGORY: Record<Exclude<Category, 'all'>, HistoryKind[]> = {
  tickets: ['ticket_created', 'ticket_updated', 'ticket_moved', 'ticket_deleted', 'comment'],
  runs: ['run_started', 'run_waiting', 'run_succeeded', 'run_failed', 'run_cancelled', 'pr_opened'],
  reviews: ['review_approved', 'review_changes_requested'],
  agents: ['agent_created', 'agent_updated', 'agent_deleted', 'agent_level_up'],
  automation: ['job_created', 'job_updated', 'job_deleted', 'job_run_started', 'job_run_finished', 'ticket_imported', 'repo_synced', 'connector_created', 'connector_updated', 'connector_deleted', 'command_issued', 'command_executed'],
  team: ['user_joined', 'skill_created', 'skill_updated', 'tab_created', 'tab_updated', 'tab_deleted'],
};

type Q = Partial<Pick<HistoryQuery, 'ticket_id' | 'agent_id' | 'tab_id' | 'run_id'>>;

function fits(e: HistoryEvent, q: Q, cat: Category) {
  if (q.ticket_id && e.ticket_id !== q.ticket_id) return false;
  if (q.agent_id && e.agent_id !== q.agent_id && !(e.actor_kind === 'agent' && e.actor_id === q.agent_id)) return false;
  if (q.tab_id && e.tab_id !== q.tab_id) return false;
  if (q.run_id && e.run_id !== q.run_id) return false;
  if (cat !== 'all' && !CATEGORY[cat].includes(e.kind)) return false;
  return true;
}

function Actor({ e }: { e: HistoryEvent }) {
  const sprite = useBoard((s) =>
    e.actor_kind === 'agent' ? s.agents[e.actor_id ?? '']?.sprite : e.actor_kind === 'user' ? s.users[e.actor_id ?? '']?.avatar : undefined,
  );
  return (
    <span className="h-icon" aria-hidden="true">
      {sprite ? <img src={e.actor_kind === 'agent' ? bodyUrl(sprite) : avatarUrl(sprite)} alt="" /> : <Icon name={KIND_ICON[e.kind] ?? 'info'} size={18} />}
      {sprite && KIND_ICON[e.kind] && (
        <span className="k">
          <Icon name={KIND_ICON[e.kind] as IconName} size={12} />
        </span>
      )}
    </span>
  );
}

/** Infinite, live history timeline (GET /api/history + WS History events). */
export function HistoryList({ query = {}, category = 'all', compact = false }: { query?: Q; category?: Category; compact?: boolean }) {
  const key = JSON.stringify(query);
  const [items, setItems] = useState<HistoryEvent[]>([]);
  const [done, setDone] = useState(false);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const live = useBoard((s) => s.history);
  const open = useUi((u) => u.open);
  const sentinel = useRef<HTMLDivElement>(null);
  const loadingRef = useRef(false);

  const load = useCallback(
    async (before: string | null) => {
      if (loadingRef.current) return;
      loadingRef.current = true;
      setLoading(true);
      try {
        const page = await api.history({ ...(JSON.parse(key) as Q), before, limit: 50 });
        setItems((xs) => {
          const seen = new Set(xs.map((x) => x.id));
          return [...xs, ...page.filter((p) => !seen.has(p.id))];
        });
        if (page.length < 50) setDone(true);
        setErr(null);
      } catch (e) {
        setErr(errorText(e));
      } finally {
        loadingRef.current = false;
        setLoading(false);
      }
    },
    [key],
  );

  useEffect(() => {
    setItems([]);
    setDone(false);
    void load(null);
  }, [load]);

  const oldest = items[items.length - 1]?.ts ?? null;
  useEffect(() => {
    const el = sentinel.current;
    if (!el || done) return;
    const io = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting) && oldest) void load(oldest);
    });
    io.observe(el);
    return () => io.disconnect();
  }, [load, oldest, done]);

  // Merge live events (newest first) with the fetched pages.
  const merged = useMemo(() => {
    const q = JSON.parse(key) as Q;
    const map = new Map<string, HistoryEvent>();
    const newestFetched = items[0]?.ts ?? '';
    for (const e of live) if (e.ts >= newestFetched && fits(e, q, 'all')) map.set(e.id, e);
    for (const e of items) map.set(e.id, e);
    return [...map.values()].filter((e) => fits(e, q, category)).sort((a, b) => (a.ts < b.ts ? 1 : a.ts > b.ts ? -1 : 0));
  }, [live, items, key, category]);

  const groups: { day: string; events: HistoryEvent[] }[] = [];
  for (const e of merged) {
    const d = dayLabel(e.ts);
    const g = groups[groups.length - 1];
    if (g && g.day === d) g.events.push(e);
    else groups.push({ day: d, events: [e] });
  }

  return (
    <div className="history">
      {groups.map((g) => (
        <div key={g.day}>
          {!compact && <div className="h-day">{g.day}</div>}
          {g.events.map((e) => {
            const target = e.ticket_id && !query.ticket_id ? () => open({ kind: 'ticket', id: e.ticket_id as string }) : e.agent_id && !query.agent_id && !e.ticket_id ? () => open({ kind: 'agent', id: e.agent_id as string }) : undefined;
            return (
              <button key={e.id} className={`h-item k-${e.kind} ${target ? 'clickable' : ''}`} onClick={target} disabled={!target} style={{ cursor: target ? 'pointer' : 'default' }}>
                <Actor e={e} />
                <span>
                  <span className="h-msg">{e.message}</span>
                  <br />
                  <span className="h-time" title={new Date(e.ts).toLocaleString()}>
                    {compact ? timeAgo(e.ts) : clock(e.ts)}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      ))}
      {merged.length === 0 && !loading && !err && <div className="empty-state">Nothing happened yet.</div>}
      {err && (
        <div className="empty-state">
          <p className="form-error">{err}</p>
          <button className="btn sm" onClick={() => void load(oldest)}>
            Retry
          </button>
        </div>
      )}
      {!done && !err && (
        <div ref={sentinel} className="sentinel">
          {loading ? 'Loading…' : ''}
        </div>
      )}
    </div>
  );
}

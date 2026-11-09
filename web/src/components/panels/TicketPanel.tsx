import { useEffect, useMemo, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { api } from '../../api/client';
import type { Priority, Ticket } from '../../api/types';
import { PRIORITIES, PRIORITY_LABEL, bodyUrl } from '../../lib/game';
import { timeAgo } from '../../lib/format';
import { assignAgent, attempt, deleteTicket, moveTicket, updateTicket } from '../../store/actions';
import { toast } from '../../store/bus';
import { isActive, sortedColumns, useAgentsList, useCan, useTicketRuns } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Drawer } from '../ui/Overlay';
import { HistoryList } from './HistoryList';
import { RunView, STATUS_LABEL } from './RunView';
import type { CommentAnchor } from '../../api/types';
import { Discussion } from '../comments/Comments';
import { QuotableBody } from '../comments/QuotableBody';
import { Icon } from '../ui/Icon';

type View = 'live' | 'details' | 'history' | 'discussion';

function Details({ t, onQuote }: { t: Ticket; onQuote: (q: string) => void }) {
  const quotes = useBoard(
    useShallow((s) => Object.values(s.comments).filter((c) => c.ticket_id === t.id && !c.parent_id && !c.deleted && !c.resolved && c.anchor.on === 'ticket_body').map((c) => (c.anchor.on === 'ticket_body' ? c.anchor.quote : ''))),
  );
  const canEdit = useCan('edit_tickets');
  const canRun = useCan('run_agents');
  const tab = useBoard((s) => s.tabs[t.tab_id]);
  const repos = useBoard((s) => s.repos);
  const creator = useBoard((s) => (t.created_by ? s.users[t.created_by]?.name : undefined));
  const agents = useAgentsList();
  const [editing, setEditing] = useState(false);
  const [f, setF] = useState({ title: t.title, body: t.body, tags: t.tags.join(', '), priority: t.priority, color: t.color ?? '' });
  const [agentId, setAgentId] = useState('');
  const [instr, setInstr] = useState('');
  const [comment, setComment] = useState('');
  const runActive = useTicketRuns(t.id).some(isActive);
  const cols = tab ? sortedColumns(tab) : [];

  const save = async () => {
    const r = await updateTicket(t, {
      title: f.title.trim(),
      body: f.body,
      tags: f.tags
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean),
      priority: f.priority,
      color: f.color || null,
    });
    if (r) setEditing(false);
  };

  return (
    <div>
      {editing ? (
        <div>
          <label className="field">
            <span className="label">Title</span>
            <input value={f.title} onChange={(e) => setF({ ...f, title: e.target.value })} maxLength={120} />
          </label>
          <label className="field">
            <span className="label">Task (markdown, becomes the agent's prompt)</span>
            <textarea value={f.body} onChange={(e) => setF({ ...f, body: e.target.value })} style={{ minHeight: 160 }} />
          </label>
          <div className="field-row" style={{ marginTop: 10 }}>
            <label className="field">
              <span className="label">Tags</span>
              <input value={f.tags} onChange={(e) => setF({ ...f, tags: e.target.value })} placeholder="react, bug" />
            </label>
            <label className="field">
              <span className="label">Priority</span>
              <select value={f.priority} onChange={(e) => setF({ ...f, priority: e.target.value as Priority })}>
                {PRIORITIES.map((p) => (
                  <option key={p} value={p}>
                    {PRIORITY_LABEL[p]}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span className="label">Paper colour</span>
              <div className="row">
                <input type="color" value={f.color || '#fffbe8'} onChange={(e) => setF({ ...f, color: e.target.value })} />
                {f.color && (
                  <button className="btn sm ghost" onClick={() => setF({ ...f, color: '' })}>
                    Reset
                  </button>
                )}
              </div>
            </label>
          </div>
          <div className="row end" style={{ marginTop: 12 }}>
            <button className="btn ghost" onClick={() => setEditing(false)}>
              Cancel
            </button>
            <button className="btn gold" onClick={() => void save()} disabled={!f.title.trim()}>
              Save
            </button>
          </div>
        </div>
      ) : (
        <>
          <dl className="kv">
            <dt>STAGE</dt>
            <dd>
              {canEdit ? (
                <select
                  value={t.column_id}
                  onChange={(e) => {
                    const pos = Math.max(0, ...Object.values(useBoard.getState().tickets).filter((x) => x.column_id === e.target.value).map((x) => x.position)) + 1024;
                    void moveTicket(t, e.target.value, pos);
                  }}
                  aria-label="Move to column"
                  style={{ width: 'auto' }}
                >
                  {cols.map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.name}
                    </option>
                  ))}
                </select>
              ) : (
                cols.find((c) => c.id === t.column_id)?.name
              )}
            </dd>
            <dt>PRIORITY</dt>
            <dd>{PRIORITY_LABEL[t.priority]}</dd>
            {t.tags.length > 0 && (
              <>
                <dt>TAGS</dt>
                <dd className="chips">
                  {t.tags.map((g) => (
                    <span key={g} className="chip">
                      {g}
                    </span>
                  ))}
                </dd>
              </>
            )}
            {(t.repo_id || tab?.repo_id) && (
              <>
                <dt>REPO</dt>
                <dd>{repos[t.repo_id ?? tab?.repo_id ?? '']?.name ?? '—'}</dd>
              </>
            )}
            {t.branch && (
              <>
                <dt>BRANCH</dt>
                <dd>
                  <code>{t.branch}</code>
                </dd>
              </>
            )}
            {t.pr_url && (
              <>
                <dt>PULL REQUEST</dt>
                <dd>
                  <a href={t.pr_url} target="_blank" rel="noreferrer noopener">
                    #{t.pr_number} <Icon name="external" size={12} />
                  </a>
                </dd>
              </>
            )}
            <dt>REVIEWS</dt>
            <dd>{t.review_rounds === 0 ? 'No changes requested yet' : `${t.review_rounds} round(s) of changes`}</dd>
            <dt>PINNED</dt>
            <dd>
              {creator ?? 'Someone'}, {timeAgo(t.created_at)}
            </dd>
          </dl>
          {t.review_feedback && (
            <div className="sheet" style={{ marginTop: 10, borderColor: '#c0392b', background: '#fff1ec' }}>
              <span className="stamp" style={{ marginTop: 0 }}>
                {t.review_feedback.startsWith('Run failed') ? 'RUN FAILED' : 'LAST REVIEW'}
              </span>
              <p className="feedback" style={{ WebkitLineClamp: 'unset' }}>
                {t.review_feedback}
              </p>
            </div>
          )}
          <div className="section-title">TASK</div>
          {t.body ? <QuotableBody text={t.body} quotes={quotes} onQuote={onQuote} canComment={canEdit} /> : <p className="muted">No description.</p>}
          {canEdit && (
            <div className="row" style={{ marginTop: 10 }}>
              <button className="btn sm" onClick={() => setEditing(true)}>
                <Icon name="edit" size={14} /> Edit
              </button>
              <button
                className="btn sm red"
                onClick={() => {
                  if (confirm(`Delete T-${t.number}? This cancels its run.`)) void deleteTicket(t);
                }}
              >
                <Icon name="trash" size={14} /> Delete
              </button>
            </div>
          )}
        </>
      )}

      {canRun && !runActive && (
        <>
          <div className="section-title">SEND A BOBA</div>
          <div className="row">
            <select className="grow" value={agentId} onChange={(e) => setAgentId(e.target.value)} aria-label="Agent">
              <option value="">Pick an agent…</option>
              {agents.map((a) => (
                <option key={a.id} value={a.id} disabled={a.status !== 'idle'}>
                  {a.name} · {a.role} · Lv{a.level}
                  {a.status !== 'idle' ? ' (busy)' : ''}
                </option>
              ))}
            </select>
          </div>
          <textarea style={{ marginTop: 8, minHeight: 60 }} placeholder="Extra instructions for this run (optional)" value={instr} onChange={(e) => setInstr(e.target.value)} />
          <div className="row end" style={{ marginTop: 8 }}>
            <button className="btn gold" disabled={!agentId} onClick={() => void assignAgent(agentId, t.id, 'tap', instr.trim() || null).then((r) => r && (setInstr(''), setAgentId('')))}>
              ▶ Start
            </button>
          </div>
        </>
      )}

      {canEdit && (
        <>
          <div className="section-title">COMMENT</div>
          <form
            className="row"
            onSubmit={async (e) => {
              e.preventDefault();
              if (!comment.trim()) return;
              const ev = await attempt(() => api.comment(t.id, comment.trim()));
              if (ev) {
                useBoard.getState().mergeHistory([ev]);
                setComment('');
                toast('Comment added', 'good');
              }
            }}
          >
            <input className="grow" value={comment} onChange={(e) => setComment(e.target.value)} placeholder="Say something to the team…" aria-label="Comment" />
            <button className="btn sm" disabled={!comment.trim()}>
              Post
            </button>
          </form>
        </>
      )}
    </div>
  );
}

export function TicketPanel({ ticketId, initialView, commentId, runId }: { ticketId: string; initialView?: View; commentId?: string; runId?: string }) {
  const t = useBoard((s) => s.tickets[ticketId]);
  const close = useUi((u) => u.open);
  const runs = useTicketRuns(ticketId);
  const agents = useBoard((s) => s.agents);
  const [view, setView] = useState<View>(initialView ?? (commentId ? 'discussion' : runs.length ? 'live' : 'details'));
  const [selected, setSelected] = useState<string | null>(runId ?? null);
  const [draft, setDraft] = useState<CommentAnchor | null>(null);
  const [focusSeq, setFocusSeq] = useState<number | undefined>(undefined);
  const comments = useBoard((s) => s.comments);
  const threadCount = useMemo(() => Object.values(comments).filter((c) => c.ticket_id === ticketId && !c.parent_id && !c.deleted && !c.resolved).length, [comments, ticketId]);
  const lineCounts = useMemo(() => {
    const out: Record<string, Record<number, number>> = {};
    for (const c of Object.values(comments)) if (c.ticket_id === ticketId && !c.deleted && c.anchor.on === 'run_log') ((out[c.anchor.run_id] ??= {})[c.anchor.seq] = (out[c.anchor.run_id]?.[c.anchor.seq] ?? 0) + 1);
    return out;
  }, [comments, ticketId]);
  const comment = (a: CommentAnchor) => {
    setDraft(a);
    setView('discussion');
  };
  const jump = (a: CommentAnchor) => {
    if (a.on === 'run_log' || a.on === 'run') {
      setSelected(a.run_id);
      setFocusSeq(a.on === 'run_log' ? a.seq : undefined);
      setView('live');
    } else setView('details');
  };

  // All rounds (REST), merged into the store so they update live.
  useEffect(() => {
    let alive = true;
    api
      .ticketRuns(ticketId)
      .then((rs) => alive && rs.forEach((r) => useBoard.getState().upsert({ entity: 'run', data: r })))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [ticketId]);

  const ordered = useMemo(() => [...runs].reverse(), [runs]); // oldest → newest
  const activeRun = runs.find(isActive);
  // Follow the newest run unless the user picked an older round.
  const run = (selected && runs.find((r) => r.id === selected)) || activeRun || runs[0];
  if (!t) return null;
  return (
    <Drawer
      title={`T-${t.number} · ${t.title}`}
      onClose={() => close(null)}
      head={
        activeRun ? (
          <img src={bodyUrl(agents[activeRun.agent_id]?.sprite)} alt="" width={30} height={30} style={{ animation: 'spin-hop 1.1s infinite' }} />
        ) : undefined
      }
    >
      <div className="panel-tabs" role="tablist">
        {(['live', 'discussion', 'details', 'history'] as View[]).map((v) => (
          <button key={v} role="tab" aria-selected={view === v} onClick={() => setView(v)}>
            {v === 'live' ? (activeRun ? '● LIVE' : `RUNS (${runs.length})`) : v === 'details' ? 'DETAILS' : v === 'discussion' ? `TALK${threadCount ? ` (${threadCount})` : ''}` : 'HISTORY'}
          </button>
        ))}
      </div>
      <div className="sheet">
        {view === 'details' && <Details t={t} onQuote={(q) => comment({ on: 'ticket_body', quote: q })} />}
        {view === 'discussion' && <Discussion ticketId={t.id} draft={draft} onDraftDone={() => setDraft(null)} focusId={commentId} onJump={jump} />}
        {view === 'history' && <HistoryList query={{ ticket_id: t.id }} compact />}
        {view === 'live' &&
          (run ? (
            <>
              {ordered.length > 1 && (
                <div className="round-picker" aria-label="Rounds">
                  {ordered.map((r, i) => (
                    <button key={r.id} aria-pressed={r.id === run.id} onClick={() => setSelected(r.id)} title={`${agents[r.agent_id]?.name ?? ''} · ${STATUS_LABEL[r.status]}`}>
                      #{i + 1} <Icon name={r.kind === 'review' ? 'search' : 'tools'} size={12} />{' '}
                      {r.verdict === 'approved' ? <Icon name="check" size={12} /> : r.verdict === 'changes_requested' ? <Icon name="refresh" size={12} /> : r.status === 'failed' ? <Icon name="close" size={12} /> : isActive(r) ? '●' : ''}
                    </button>
                  ))}
                </div>
              )}
              <RunView
                key={run.id}
                run={run}
                onSwitchRun={setSelected}
                lineCounts={lineCounts[run.id]}
                focusSeq={focusSeq}
                onCommentLine={(seq) => comment({ on: 'run_log', run_id: run.id, seq })}
                onCommentRun={() => comment({ on: 'run', run_id: run.id })}
              />
            </>
          ) : (
            <div className="empty-state">
              <p>No agent has worked on this ticket yet.</p>
              <p className="hint-text">Drag a boba from the tavern onto it, or use “Send a boba” in Details.</p>
              <button className="btn sm" onClick={() => setView('details')}>
                Open details
              </button>
            </div>
          ))}
      </div>
    </Drawer>
  );
}

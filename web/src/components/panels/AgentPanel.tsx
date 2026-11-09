import { useEffect, useMemo, useState, type CSSProperties } from 'react';
import { api } from '../../api/client';
import type { Run } from '../../api/types';
import { MODELS, isConcierge, xpForNextLevel, xpPct } from '../../lib/game';
import { duration, timeAgo, usd } from '../../lib/format';
import { deleteAgent } from '../../store/actions';
import { isActive, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Drawer } from '../ui/Overlay';
import { Sprite } from '../ui/Sprite';
import { HistoryList } from './HistoryList';
import { STATUS_LABEL } from './RunView';

export function AgentPanel({ agentId }: { agentId: string }) {
  const a = useBoard((s) => s.agents[agentId]);
  const runsMap = useBoard((s) => s.runs);
  const tickets = useBoard((s) => s.tickets);
  const repos = useBoard((s) => s.repos);
  const tabs = useBoard((s) => s.tabs);
  const skills = useBoard((s) => s.skills);
  const connectors = useBoard((s) => s.connectors);
  const ui = useUi();
  const canManage = useCan('manage_agents');
  const [showPrompt, setShowPrompt] = useState(false);

  // Fetch runs of the tickets this agent touched so "recent runs" is complete.
  useEffect(() => {
    const ids = new Set(Object.values(useBoard.getState().tickets).filter((t) => t.agent_id === agentId).map((t) => t.id));
    ids.forEach((id) =>
      api
        .ticketRuns(id)
        .then((rs) => rs.forEach((r) => useBoard.getState().upsert({ entity: 'run', data: r })))
        .catch(() => {}),
    );
  }, [agentId]);

  const runs = useMemo(
    () =>
      Object.values(runsMap)
        .filter((r) => r.agent_id === agentId)
        .sort((x, y) => (x.created_at < y.created_at ? 1 : -1)),
    [runsMap, agentId],
  );
  if (!a) return null;

  const current: Run | undefined = a.current_run_id ? runsMap[a.current_run_id] : runs.find(isActive);
  const ct = current?.ticket_id ? tickets[current.ticket_id] : undefined;
  const repo = ct ? repos[ct.repo_id ?? tabs[ct.tab_id]?.repo_id ?? ''] : undefined;
  const reviews = runs.filter((r) => r.kind === 'review' && r.verdict);
  const approvals = reviews.filter((r) => r.verdict === 'approved').length;
  const approvalRate = reviews.length ? Math.round((approvals / reviews.length) * 100) : null;
  const isReviewer = a.role.toLowerCase() === 'reviewer' || (reviews.length > 0 && runs.every((r) => r.kind === 'review'));
  // For authors: share of their PRs that passed review on the first round.
  const authored = Object.values(tickets).filter((t) => t.agent_id === a.id && t.pr_number != null);
  const firstPass = authored.length ? Math.round((authored.filter((t) => t.review_rounds === 0).length / authored.length) * 100) : null;
  const spent = runs.reduce((s, r) => s + r.cost_usd, 0);
  const model = MODELS.find((m) => m.id === a.model)?.label ?? a.model ?? 'Default';

  return (
    <Drawer title={a.name.toUpperCase()} onClose={() => ui.open(null)}>
      <div className="sheet">
        <div className="profile-hero" style={{ '--ac': a.color } as CSSProperties}>
          <div className="pedestal">
            <Sprite sprite={a.sprite} color={a.color} status={a.status} size={130} body />
          </div>
          <h2>{a.name}</h2>
          <div className="row" style={{ justifyContent: 'center' }}>
            <span className="role" style={{ background: a.color }}>
              {a.role}
            </span>
            <span className="lvl">LEVEL {a.level}</span>
          </div>
          {a.tagline && <p className="tagline">“{a.tagline}”</p>}
          <div className="xp-big">
            <div className="xp">
              <i style={{ width: `${xpPct(a.xp, a.level)}%` }} />
            </div>
            <small>
              {a.xp} / {xpForNextLevel(a.level)} XP to level {a.level + 1}
            </small>
          </div>
        </div>

        <div className="stat-grid" style={{ marginTop: 12 }}>
          <div className="stat">
            <b>{a.runs_completed}</b>
            <span>runs done</span>
          </div>
          {isReviewer ? (
            <>
              <div className="stat">
                <b>{reviews.length}</b>
                <span>reviews done</span>
              </div>
              <div className="stat">
                <b>{approvals}</b>
                <span>approvals given</span>
              </div>
              <div className="stat">
                <b>{approvalRate === null ? '—' : `${approvalRate}%`}</b>
                <span>approval rate</span>
              </div>
            </>
          ) : (
            <>
              <div className="stat">
                <b>{a.prs_approved}</b>
                <span>PRs approved</span>
              </div>
              <div className="stat">
                <b>{firstPass === null ? '—' : `${firstPass}%`}</b>
                <span>first-round pass</span>
              </div>
            </>
          )}
          <div className="stat">
            <b>{usd(spent)}</b>
            <span>recent spend</span>
          </div>
        </div>

        <div className="section-title">WHERE IT WORKS</div>
        {current && ct ? (
          <button className="run-row" onClick={() => ui.open({ kind: 'ticket', id: ct.id, view: 'live' })}>
            <Sprite sprite={a.sprite} color={a.color} status={a.status} size={30} />
            <span>
              <b>
                T-{ct.number} {ct.title}
              </b>
              <br />
              <span className="hint-text">
                {repo?.name ?? 'repo'} · <code>{current.branch ?? ct.branch}</code> · {current.activity ?? STATUS_LABEL[current.status]}
              </span>
            </span>
            <span className={`run-status ${current.status}`}>{Math.round(current.progress * 100)}%</span>
          </button>
        ) : (
          <p className="muted">Resting in the tavern. Drag it onto a ticket to put it to work.</p>
        )}

        <div className="section-title">PERSONA</div>
        <dl className="kv">
          <dt>MODEL</dt>
          <dd>{model}</dd>
          <dt>PERMISSIONS</dt>
          <dd>{a.permission_mode}</dd>
          {a.max_turns != null && (
            <>
              <dt>MAX TURNS</dt>
              <dd>{a.max_turns}</dd>
            </>
          )}
          <dt>SKILLS</dt>
          <dd className="chips">
            {a.skill_ids.length
              ? a.skill_ids.map((id) => (
                  <button key={id} className="chip" onClick={() => ui.open({ kind: 'skills', id })}>
                    {skills[id]?.name ?? id}
                  </button>
                ))
              : '—'}
          </dd>
          <dt>PORTALS</dt>
          <dd className="chips">
            {a.connector_ids.length ? a.connector_ids.map((id) => <span key={id} className="chip">🌀 {connectors[id]?.name ?? id}</span>) : '—'}
          </dd>
          <dt>BOARD TOOLS</dt>
          <dd>{a.board_tools ? 'On: can file and update tickets' : 'Off'}</dd>
          {a.allowed_tools.length > 0 && (
            <>
              <dt>TOOLS</dt>
              <dd className="chips">
                {a.allowed_tools.map((x) => (
                  <code key={x}>{x}</code>
                ))}
              </dd>
            </>
          )}
        </dl>
        <button className="btn sm ghost" style={{ marginTop: 8 }} onClick={() => setShowPrompt((v) => !v)}>
          {showPrompt ? 'Hide' : 'Show'} system prompt
        </button>
        {showPrompt && <div className="prompt-box" style={{ marginTop: 6 }}>{a.system_prompt || '(empty)'}</div>}

        {isConcierge(a) && <p className="hint-text" style={{ marginTop: 10 }}>The Concierge is built in: it reads the board and proposes actions from the ⌘K bar, and never acts without your confirmation.</p>}
        {canManage && !isConcierge(a) && (
          <div className="row" style={{ marginTop: 12 }}>
            <button className="btn gold sm" onClick={() => ui.show({ kind: 'agent-edit', id: a.id })}>
              ✏ Edit persona
            </button>
            <button
              className="btn red sm"
              disabled={a.status !== 'idle'}
              title={a.status !== 'idle' ? 'Cancel its run first' : undefined}
              onClick={async () => {
                if (confirm(`Send ${a.name} home? Its history stays.`) && (await deleteAgent(a.id))) ui.open(null);
              }}
            >
              👋 Dismiss
            </button>
          </div>
        )}

        <div className="section-title">RECENT RUNS</div>
        {runs.length === 0 && <p className="muted">{a.runs_completed ? 'No recent runs on the board right now.' : 'No runs yet.'}</p>}
        {runs.slice(0, 12).map((r) => {
          const t = r.ticket_id ? tickets[r.ticket_id] : undefined;
          return (
            <button key={r.id} className="run-row" onClick={() => t && ui.open({ kind: 'ticket', id: t.id, view: 'live' })}>
              <span>{r.kind === 'review' ? '🔍' : r.kind === 'job' ? '📜' : r.kind === 'command' ? '🎙' : '🛠'}</span>
              <span style={{ minWidth: 0 }}>
                {t ? `T-${t.number} ${t.title}` : r.kind === 'job' ? 'Quest run' : r.kind === 'command' ? 'Concierge command' : 'Deleted ticket'}
                <br />
                <span className="hint-text">
                  {timeAgo(r.created_at)} · {duration(r.started_at ?? r.created_at, r.ended_at)} · {usd(r.cost_usd)}
                </span>
              </span>
              <span className={`run-status ${r.status}`}>{r.verdict === 'approved' ? 'Approved' : r.verdict === 'changes_requested' ? 'Changes' : STATUS_LABEL[r.status]}</span>
            </button>
          );
        })}

        <div className="section-title">HISTORY</div>
        <HistoryList query={{ agent_id: a.id }} compact />
      </div>
    </Drawer>
  );
}

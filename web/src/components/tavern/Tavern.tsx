import { useDraggable } from '@dnd-kit/core';
import { memo, useEffect, useState, type CSSProperties } from 'react';
import type { Agent } from '../../api/types';
import { sfx } from '../../audio/audio';
import { xpForNextLevel, xpPct } from '../../lib/game';
import { fxBus, toast } from '../../store/bus';
import { usePrefs } from '../../store/prefs';
import { useAgentsList, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Sprite } from '../ui/Sprite';

function statusText(a: Agent, ticketNo: number | undefined, prNo: number | null | undefined): string {
  switch (a.status) {
    case 'working':
      return ticketNo ? `Working on T-${ticketNo}` : 'Working…';
    case 'reviewing':
      return prNo ? `Reviewing PR #${prNo}` : ticketNo ? `Reviewing T-${ticketNo}` : 'Reviewing…';
    case 'waiting':
      return ticketNo ? `Needs you on T-${ticketNo}` : 'Waiting for permission';
    case 'error':
      return 'Something went wrong';
    default:
      return a.tagline || 'Ready in the tavern';
  }
}

const AgentCard = memo(function AgentCard({ agent: a }: { agent: Agent }) {
  const canRun = useCan('run_agents');
  const picked = useUi((u) => u.picked === a.id);
  const pick = useUi((u) => u.pick);
  const open = useUi((u) => u.open);
  const run = useBoard((s) => (a.current_run_id ? s.runs[a.current_run_id] : undefined));
  const ticket = useBoard((s) => (run?.ticket_id ? s.tickets[run.ticket_id] : undefined));
  const busy = a.status !== 'idle';
  const [fx, setFx] = useState<'shake' | 'fresh' | null>(null);
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id: `a:${a.id}`,
    data: { type: 'agent', agentId: a.id, label: a.name },
    disabled: busy || !canRun,
  });

  useEffect(
    () =>
      fxBus.on((f) => {
        if ((f.fx === 'blocked' || f.fx === 'recruited') && f.agent_id === a.id) {
          setFx(f.fx === 'blocked' ? 'shake' : 'fresh');
          setTimeout(() => setFx(null), 800);
        }
      }),
    [a.id],
  );

  const onClick = () => {
    if (busy) {
      if (ticket) open({ kind: 'ticket', id: ticket.id, view: 'live' });
      return;
    }
    if (!canRun) {
      open({ kind: 'agent', id: a.id });
      return;
    }
    if (picked) pick(null);
    else {
      pick(a.id);
      sfx('pearl', 0.35);
      toast(`${a.name} is ready. Now click a ticket.`, 'info', a.sprite);
    }
  };

  return (
    <div
      ref={setNodeRef}
      {...attributes}
      {...listeners}
      role="button"
      tabIndex={0}
      aria-pressed={picked}
      aria-label={`${a.name}, ${a.role}, level ${a.level}. ${statusText(a, ticket?.number, ticket?.pr_number)}`}
      data-agent-card={a.id}
      className={`agent ${a.status} ${busy ? 'busy' : ''} ${picked ? 'picked' : ''} ${isDragging ? 'lifted' : ''} ${fx ?? ''}`}
      style={{ '--ac': a.color } as CSSProperties}
      onClick={onClick}
      onKeyDown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          onClick();
        }
        listeners?.onKeyDown?.(e);
      }}
    >
      <span className="agent-face" data-agent-face={a.id}>
        <Sprite sprite={a.sprite} color={a.color} status={a.status} size={58} />
      </span>
      <div className="agent-body" style={{ minWidth: 0 }}>
        <div className="agent-name">
          {a.name} <span className="lvl">Lv{a.level}</span>
        </div>
        <span className="role">{a.role}</span>
        <div className="xp" title={`${a.xp} / ${xpForNextLevel(a.level)} XP`}>
          <i style={{ width: `${xpPct(a.xp, a.level)}%` }} />
        </div>
        <div className="status-line">{statusText(a, ticket?.number, ticket?.pr_number)}</div>
      </div>
      <button
        className="icon-btn agent-info"
        aria-label={`Open ${a.name}'s profile`}
        title="Profile"
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          open({ kind: 'agent', id: a.id });
        }}
      >
        i
      </button>
    </div>
  );
});

export function Tavern() {
  const agents = useAgentsList();
  const canAgents = useCan('manage_agents');
  const canRun = useCan('run_agents');
  const show = useUi((u) => u.show);
  const collapsed = usePrefs((p) => p.tavernCollapsed);
  const setPrefs = usePrefs((p) => p.set);
  const working = agents.filter((a) => a.status !== 'idle').length;

  return (
    <aside className="tavern wood" aria-label="Tavern of agents">
      <div className="tavern-head">
        <div className="sign">
          THE TAVERN{' '}
          <span style={{ opacity: 0.7 }}>
            {working}/{agents.length}
          </span>
        </div>
        <button className="icon-btn" onClick={() => setPrefs({ tavernCollapsed: !collapsed })} aria-label={collapsed ? 'Expand tavern' : 'Collapse tavern'} title={collapsed ? 'Expand' : 'Collapse'}>
          {collapsed ? '»' : '«'}
        </button>
      </div>
      {canRun && <div className="tavern-hint">Drag a boba onto a ticket (or tap the boba, then the ticket)</div>}
      <div className="agents">
        {agents.map((a) => (
          <AgentCard key={a.id} agent={a} />
        ))}
        {canAgents && (
          <button className="recruit" onClick={() => show({ kind: 'agent-new' })}>
            <span style={{ fontSize: 16 }}>+</span>
            <span className="recruit-label">Recruit a boba</span>
          </button>
        )}
      </div>
    </aside>
  );
}

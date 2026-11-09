import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { memo, useEffect, useState, type CSSProperties } from 'react';
import type { Column, Run, Ticket } from '../../api/types';
import { KIND_COLORS, paperTilt } from '../../lib/game';
import { assignAgent } from '../../store/actions';
import { fxBus } from '../../store/bus';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Sprite } from '../ui/Sprite';
import { useCommentCount } from '../comments/Comments';
import { Icon } from '../ui/Icon';

const plain = (md: string) =>
  md
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/[#>*_`[\]()-]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();

export function stageColor(col: Column | undefined) {
  return col?.color || KIND_COLORS[col?.kind ?? 'custom'];
}

interface PaperProps {
  ticket: Ticket;
  overlay?: boolean;
  run?: Run;
  column?: Column;
  canDrop?: boolean;
  agentOver?: boolean;
  dragging?: boolean;
  landed?: boolean;
}

/** The "little paper" pinned on the board. Pure presentational (also used as the drag overlay). */
export const TicketPaper = memo(function TicketPaper({ ticket: t, overlay, run, column, canDrop, agentOver, dragging, landed }: PaperProps) {
  const col = useBoard((s) => column ?? s.tabs[t.tab_id]?.columns.find((c) => c.id === t.column_id));
  const worker = useBoard((s) => s.agents[run?.agent_id ?? (col?.kind === 'done' ? t.agent_id ?? '' : '')]);
  const reviewer = useBoard((s) => (col?.auto_reviewer_agent_id ? s.agents[col.auto_reviewer_agent_id] : undefined));
  const kind = col?.kind;
  // Server counter is authoritative; the store count covers comments that arrived before the ticket upsert.
  const loaded = useCommentCount(t.id);
  const comments = Math.max(t.comment_count, loaded);
  const active = !!run;
  const failed = t.review_feedback?.startsWith('Run failed');
  const style = {
    '--c': stageColor(col),
    '--r': `${paperTilt(t.id)}deg`,
    '--tint': t.color ?? undefined,
  } as CSSProperties;

  return (
    <article
      data-ticket={t.id}
      className={[
        'ticket',
        t.color ? 'tinted' : '',
        overlay ? 'overlay' : '',
        active ? 'running' : '',
        canDrop ? 'can-drop' : '',
        agentOver ? 'agent-over' : '',
        dragging ? 'dragging' : '',
        landed ? 'landed' : '',
      ].join(' ')}
      style={style}
    >
      <div className="t-top">
        <span>T-{t.number}</span>
        <span className={`prio ${t.priority}`}>{t.priority.toUpperCase()}</span>
        {t.review_rounds > 0 && <span title={`${t.review_rounds} review round(s)`}>R{t.review_rounds + (kind === 'review' || kind === 'done' ? 1 : 0)}</span>}
        <span className="grow" />
        {(t.source.kind === 'github_issue' || t.source.kind === 'github_pr') && (
          <a
            className="pr-badge gh"
            href={t.source.url}
            target="_blank"
            rel="noreferrer noopener"
            onClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => e.stopPropagation()}
            title={`Imported from GitHub ${t.source.kind === 'github_pr' ? 'PR' : 'issue'} #${t.source.number} by ${t.source.author}`}
          >
            <Icon name={t.source.kind === 'github_pr' ? 'pr' : 'issue'} size={12} /> {t.source.number}
          </a>
        )}
        {t.pr_number != null && (
          <a
            className="pr-badge"
            href={t.pr_url ?? undefined}
            target="_blank"
            rel="noreferrer noopener"
            onClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => e.stopPropagation()}
            title="Open the pull request"
          >
            #{t.pr_number}
          </a>
        )}
      </div>
      <h3>{t.title}</h3>
      {t.body && <p className="t-body">{plain(t.body)}</p>}

      {kind === 'done' && t.pr_number != null && <div className="stamp ok">APPROVED</div>}
      {!active && t.review_feedback && kind !== 'done' && (
        <>
          <div className={`stamp ${failed ? 'fail' : ''}`}>{failed ? 'RUN FAILED' : 'CHANGES REQUESTED'}</div>
          <div className="feedback">{failed ? t.review_feedback.replace(/^Run failed:\s*/, '') : t.review_feedback}</div>
        </>
      )}
      {kind === 'review' && !active && (
        <div className="waiting-note">
          {reviewer ? (reviewer.status === 'idle' ? `${reviewer.name} will pick this up…` : `Waiting for ${reviewer.name}…`) : 'Drag a reviewer here to review'}
        </div>
      )}
      {active && (
        <>
          <div className="bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(run.progress * 100)}>
            <i style={{ width: `${Math.max(4, Math.round(run.progress * 100))}%` }} />
          </div>
          <div className="activity">{run.status === 'waiting' ? 'Needs your permission' : run.activity ?? 'Working…'}</div>
        </>
      )}

      <div className="t-foot">
        <div className="chips">
          {t.tags.slice(0, 3).map((g) => (
            <span key={g} className="chip">
              {g}
            </span>
          ))}
        </div>
        {comments > 0 && (
          <span className={`c-badge ${t.open_thread_count ? 'open' : ''}`} title={`${comments} comment${comments > 1 ? 's' : ''}${t.open_thread_count ? `, ${t.open_thread_count} open thread${t.open_thread_count > 1 ? 's' : ''}` : ''}`}>
            <Icon name="chat" size={12} /> {comments}
          </span>
        )}
        {worker && (
          <span className="worker" data-ticket-worker={t.id}>
            <Sprite sprite={worker.sprite} color={worker.color} status={active ? worker.status : 'idle'} size={34} />
            <span>{worker.name}</span>
          </span>
        )}
      </div>
    </article>
  );
});

export const TicketCard = memo(function TicketCard({ ticket, run, column, canEdit, hidden }: { ticket: Ticket; run?: Run; column: Column; canEdit: boolean; hidden?: boolean }) {
  const draggingAgent = useUi((u) => u.draggingAgent);
  const picked = useUi((u) => u.picked);
  const open = useUi((u) => u.open);
  const pick = useUi((u) => u.pick);
  const spot = useUi((u) => u.spotlight === ticket.id);
  const [landed, setLanded] = useState(false);
  const { attributes, listeners, setNodeRef, transform, transition, isDragging, isOver, active } = useSortable({
    id: `t:${ticket.id}`,
    data: { type: 'ticket', ticketId: ticket.id, columnId: column.id, label: `T-${ticket.number} ${ticket.title}` },
    disabled: { draggable: !canEdit, droppable: false },
  });
  useEffect(
    () =>
      fxBus.on((f) => {
        if (f.fx === 'assigned' && f.ticket_id === ticket.id) {
          setLanded(true);
          setTimeout(() => setLanded(false), 600);
        }
      }),
    [ticket.id],
  );
  const agentMode = !!draggingAgent || !!picked;
  const canDrop = agentMode && !run && column.kind !== 'done';
  const agentOver = isOver && active?.data.current?.type === 'agent';

  const onClick = () => {
    if (picked) {
      pick(null);
      if (!run) void assignAgent(picked, ticket.id, 'tap');
      return;
    }
    open({ kind: 'ticket', id: ticket.id });
  };

  return (
    <div
      ref={setNodeRef}
      className={spot ? 'spotlight' : undefined}
      style={{ transform: CSS.Translate.toString(transform), transition, display: hidden ? 'none' : undefined }}
      {...attributes}
      {...listeners}
      role="button"
      tabIndex={0}
      aria-label={`T-${ticket.number} ${ticket.title}${run ? `, ${run.activity ?? 'running'}` : ''}`}
      onClick={onClick}
      onKeyDown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          onClick();
          return;
        }
        listeners?.onKeyDown?.(e);
      }}
    >
      <TicketPaper ticket={ticket} run={run} column={column} canDrop={canDrop} agentOver={agentOver} dragging={isDragging} landed={landed} />
    </div>
  );
});

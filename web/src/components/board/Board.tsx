import { useDroppable } from '@dnd-kit/core';
import { SortableContext, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { memo, useMemo, useState } from 'react';
import type { Column, Ticket } from '../../api/types';
import { bodyUrl } from '../../lib/game';
import { sortedColumns, useActiveRuns, useCan, useTicketsByColumn } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { stageColor, TicketCard } from './TicketCard';

const EMPTY: Record<Column['kind'], string> = {
  backlog: 'Ideas go here.',
  todo: 'Nothing to do. Pin a ticket!',
  doing: 'Drop a boba on a ticket to start.',
  review: 'PRs land here for review.',
  done: 'Approved work shows up here.',
  custom: 'Empty.',
};

function matches(t: Ticket, q: string) {
  if (!q) return true;
  const s = q.toLowerCase();
  return t.title.toLowerCase().includes(s) || `t-${t.number}` === s || t.tags.some((g) => g.toLowerCase().includes(s)) || t.body.toLowerCase().includes(s);
}

const ColumnView = memo(function ColumnView({ column, ids, canEdit, query }: { column: Column; ids: string[]; canEdit: boolean; query: string }) {
  const tickets = useBoard((s) => s.tickets);
  const runs = useActiveRuns();
  const reviewer = useBoard((s) => (column.auto_reviewer_agent_id ? s.agents[column.auto_reviewer_agent_id] : undefined));
  const show = useUi((u) => u.show);
  const open = useUi((u) => u.open);
  const { setNodeRef, isOver, active } = useDroppable({ id: `c:${column.id}`, data: { type: 'column', columnId: column.id, label: column.name } });
  const ticketOver = isOver && active?.data.current?.type === 'ticket';
  const visible = ids.filter((id) => tickets[id] && matches(tickets[id] as Ticket, query));

  return (
    <section id={`col-${column.id}`} className={`col ${ticketOver ? 'drop-over' : ''}`} aria-label={`${column.name} column`}>
      <div className="col-head">
        <span className="dot" style={{ background: stageColor(column) }} />
        <span className="name">{column.name}</span>
        {reviewer && (
          <span
            className={`guard ${reviewer.status !== 'idle' ? 'busy' : ''}`}
            title={`${reviewer.name} reviews every PR that lands here`}
            onClick={() => open({ kind: 'agent', id: reviewer.id })}
          >
            <img src={bodyUrl(reviewer.sprite)} alt="" />
          </span>
        )}
        <span>{visible.length}</span>
        {canEdit && (column.kind === 'todo' || column.kind === 'backlog' || column.kind === 'custom') && (
          <button className="col-add" onClick={() => show({ kind: 'ticket-new', tabId: column.tab_id, columnId: column.id })} aria-label={`Add a ticket to ${column.name}`} title="Add a ticket">
            +
          </button>
        )}
      </div>
      <SortableContext id={column.id} items={ids.map((id) => `t:${id}`)} strategy={verticalListSortingStrategy}>
        <div className="cards" ref={setNodeRef}>
          {ids.map((id) => {
            const t = tickets[id];
            return t ? <TicketCard key={id} ticket={t} run={runs[id]} column={column} canEdit={canEdit} hidden={!matches(t, query)} /> : null;
          })}
          {visible.length === 0 && <div className="col-empty">{query ? 'No match.' : EMPTY[column.kind]}</div>}
        </div>
      </SortableContext>
    </section>
  );
});

export function Board({ tabId, dragCols, draggingTicket, canEdit }: { tabId: string; dragCols: Record<string, string[]> | null; draggingTicket: string | null; canEdit: boolean }) {
  const tab = useBoard((s) => s.tabs[tabId]);
  const byCol = useTicketsByColumn(tabId);
  const canManage = useCan('manage_workspace');
  const show = useUi((u) => u.show);
  const [query, setQuery] = useState('');
  const columns = useMemo(() => (tab ? sortedColumns(tab) : []), [tab]);
  const repo = useBoard((s) => (tab?.repo_id ? s.repos[tab.repo_id] : undefined));
  if (!tab) return null;

  return (
    <section className="board-wrap wood" aria-label={`${tab.name} board`}>
      <div className="board-tools">
        <span className="sign" style={{ textAlign: 'left' }}>
          {tab.icon} {tab.name.toUpperCase()}
        </span>
        {repo && (
          <span className="chip" title={repo.path}>
            ⎇ {repo.name}
          </span>
        )}
        <span className="grow" />
        <input className="search" type="search" placeholder="Filter tickets…" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Filter tickets" />
        {canManage && (
          <button className="btn sm" onClick={() => show({ kind: 'tab-edit', id: tab.id })}>
            ⚙ Columns
          </button>
        )}
      </div>
      <div className="col-pills" role="tablist" aria-label="Jump to column">
        {columns.map((c) => (
          <button
            key={c.id}
            className="col-pill"
            onClick={() => document.getElementById(`col-${c.id}`)?.scrollIntoView({ behavior: 'smooth', inline: 'start', block: 'nearest' })}
          >
            <span className="dot" style={{ background: stageColor(c) }} /> {c.name} <b>{(dragCols?.[c.id] ?? byCol[c.id] ?? []).length}</b>
          </button>
        ))}
      </div>
      <div className="board">
        {columns.map((c) => (
          <ColumnView key={c.id} column={c} ids={dragCols?.[c.id] ?? (byCol[c.id] ?? []).map((t) => t.id)} canEdit={canEdit} query={draggingTicket ? '' : query} />
        ))}
      </div>
    </section>
  );
}

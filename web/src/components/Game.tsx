import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  MouseSensor,
  TouchSensor,
  closestCorners,
  pointerWithin,
  rectIntersection,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import { arrayMove, sortableKeyboardCoordinates } from '@dnd-kit/sortable';
import { AnimatePresence } from 'motion/react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { connection } from '../api/connection';
import type { Ticket } from '../api/types';
import { positionBetween } from '../lib/game';
import { assignAgent, moveTicket } from '../store/actions';
import { toast } from '../store/bus';
import { usePrefs, reducedMotion } from '../store/prefs';
import { isActive, useActiveRuns, useCan, useTabs } from '../store/selectors';
import { useBoard } from '../store/store';
import { useUi } from '../store/ui';
import { Board } from './board/Board';
import { TicketPaper } from './board/TicketCard';
import { FxLayer } from './fx/FxLayer';
import { Header } from './Header';
import { MusicController } from './MusicController';
import { AgentEditor } from './modals/AgentEditor';
import { SettingsModal } from './modals/SettingsModal';
import { ShortcutsModal } from './modals/ShortcutsModal';
import { TabEditor } from './modals/TabEditor';
import { TicketModal } from './modals/TicketModal';
import { AgentPanel } from './panels/AgentPanel';
import { HistoryDrawer } from './panels/HistoryDrawer';
import { SkillsDrawer } from './panels/SkillsDrawer';
import { TicketPanel } from './panels/TicketPanel';
import { Tavern } from './tavern/Tavern';
import { handleGithubReturn } from './integrations/GithubCard';
import { GithubSetup, githubSetupFromUrl } from './integrations/GithubSetup';
import { MembersModal, ProfileModal } from './team/Account';
import { RunnersPage } from './team/Runners';
import { CommandBar } from './command/CommandBar';
import { Integrations } from './integrations/Integrations';
import { PortalEditor, Portals } from './integrations/Portals';
import { QuestBoard, QuestPanel } from './quests/QuestBoard';
import { QuestEditor } from './quests/QuestEditor';
import { Sprite } from './ui/Sprite';
import { Loading } from './ui/Loading';

type ActiveDrag = { type: 'agent'; id: string } | { type: 'ticket'; id: string } | null;

const tid = (id: string) => `t:${id}`;
const strip = (s: string) => s.slice(2);

const collision: CollisionDetection = (args) => {
  const type = args.active.data.current?.type;
  if (type === 'agent') {
    const tickets = args.droppableContainers.filter((d) => d.data.current?.type === 'ticket');
    return pointerWithin({ ...args, droppableContainers: tickets });
  }
  const targets = args.droppableContainers.filter((d) => d.data.current?.type !== 'agent');
  const scoped = { ...args, droppableContainers: targets };
  const p = pointerWithin(scoped);
  if (p.length) {
    // Prefer a ticket under the pointer over its column.
    const t = p.find((c) => String(c.id).startsWith('t:'));
    return t ? [t] : p;
  }
  const r = rectIntersection(scoped);
  return r.length ? r : closestCorners(scoped);
};

export function Game({ onLoggedOut }: { onLoggedOut: () => Promise<void> }) {
  const ready = useBoard((s) => s.ready);
  const tabs = useTabs();
  const ui = useUi();
  const prefs = usePrefs();
  const canEdit = useCan('edit_tickets');
  const activeRuns = useActiveRuns();
  const [drag, setDrag] = useState<ActiveDrag>(null);
  const [dragCols, setDragCols] = useState<Record<string, string[]> | null>(null);

  // Pick an active tab, remember it per viewer.
  const activeTab = ui.activeTab && tabs.some((t) => t.id === ui.activeTab) ? ui.activeTab : null;
  useEffect(() => {
    if (!ready || activeTab || tabs.length === 0) return;
    const remembered = tabs.find((t) => t.id === prefs.lastTab);
    ui.setTab((remembered ?? tabs[0])?.id ?? null);
  }, [ready, activeTab, tabs, prefs.lastTab, ui]);
  useEffect(() => {
    if (activeTab) {
      connection.setViewing(activeTab);
      if (activeTab !== usePrefs.getState().lastTab) usePrefs.getState().set({ lastTab: activeTab });
    }
  }, [activeTab]);

  // Back from GitHub's app setup / install pages.
  useEffect(() => {
    if (!ready) return;
    const setup = githubSetupFromUrl();
    if (setup) {
      ui.setView('integrations');
      ui.show({ kind: 'github-setup', ...setup });
      return;
    }
    if (!handleGithubReturn()) return;
    ui.setView('integrations');
    toast('Welcome back from GitHub 🐙 The app is set up: add a repo below.', 'gold');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready]);

  // Deep link: #/t/<number> opens a ticket.
  useEffect(() => {
    if (!ready) return;
    const m = location.hash.match(/^#\/t\/(\d+)/);
    if (!m) return;
    const t = Object.values(useBoard.getState().tickets).find((x) => x.number === Number(m[1]));
    if (t) {
      ui.setTab(t.tab_id);
      ui.open({ kind: 'ticket', id: t.id });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready]);
  const panelTicket = ui.panel?.kind === 'ticket' ? useBoard.getState().tickets[ui.panel.id] : undefined;
  useEffect(() => {
    if (!ready) return;
    const next = panelTicket ? `#/t/${panelTicket.number}` : '';
    if (location.hash !== next) history.replaceState(null, '', `${location.pathname}${location.search}${next}`);
  }, [panelTicket, ready]);

  // Reduced motion class.
  useEffect(() => {
    document.documentElement.classList.toggle('reduce-motion', reducedMotion());
  }, [prefs.motion]);

  // Keyboard shortcuts.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (e.metaKey || e.ctrlKey || e.altKey || el.closest('input, textarea, select, [contenteditable]')) return;
      const st = useUi.getState();
      if (e.key === 'Escape') {
        if (st.picked) st.pick(null);
        return;
      }
      if (st.modal || st.command.open) return;
      const tabsNow = Object.values(useBoard.getState().tabs).sort((a, b) => a.position - b.position);
      if (/^[1-9]$/.test(e.key)) {
        const t = tabsNow[Number(e.key) - 1];
        if (t) st.setTab(t.id);
      } else if (e.key === 'n' && st.activeTab) st.show({ kind: 'ticket-new', tabId: st.activeTab });
      else if (e.key === 'a') st.show({ kind: 'agent-new' });
      else if (e.key === 'h') st.open(st.panel?.kind === 'history' ? null : { kind: 'history' });
      else if (e.key === 'k') st.open(st.panel?.kind === 'skills' ? null : { kind: 'skills' });
      else if (e.key === 'q') st.setView(st.view === 'quests' ? 'board' : 'quests');
      else if (e.key === 'i') st.setView(st.view === 'integrations' ? 'board' : 'integrations');
      else if (e.key === 'p') st.setView(st.view === 'portals' ? 'board' : 'portals');
      else if (e.key === 'm') {
        const p = usePrefs.getState();
        const ws = useBoard.getState().settings?.music_enabled_by_default ?? false;
        p.set({ music: !(p.music ?? ws) });
      } else if (e.key === '?') st.show({ kind: 'shortcuts' });
      else return;
      e.preventDefault();
    };
    window.addEventListener('keydown', h);
    return () => window.removeEventListener('keydown', h);
  }, []);

  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 6 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 180, tolerance: 8 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const columnsOf = useCallback(() => {
    const st = useBoard.getState();
    const tab = activeTab ? st.tabs[activeTab] : undefined;
    const out: Record<string, string[]> = {};
    if (!tab) return out;
    for (const c of tab.columns) out[c.id] = [];
    for (const t of Object.values(st.tickets).filter((x) => x.tab_id === tab.id).sort((a, b) => a.position - b.position))
      out[t.column_id]?.push(t.id);
    return out;
  }, [activeTab]);

  const onDragStart = (e: DragStartEvent) => {
    const type = e.active.data.current?.type as 'agent' | 'ticket';
    const id = strip(String(e.active.id));
    setDrag({ type, id });
    if (type === 'agent') ui.setDraggingAgent(id);
    else setDragCols(columnsOf());
  };

  const findCol = (cols: Record<string, string[]>, overId: string) => {
    if (overId.startsWith('c:')) return strip(overId);
    const t = strip(overId);
    return Object.keys(cols).find((k) => cols[k]?.includes(t));
  };

  const onDragOver = (e: DragOverEvent) => {
    if (drag?.type !== 'ticket' || !e.over || !dragCols) return;
    const from = findCol(dragCols, tid(drag.id));
    const to = findCol(dragCols, String(e.over.id));
    if (!from || !to || from === to) return;
    setDragCols((cols) => {
      if (!cols) return cols;
      const src = (cols[from] ?? []).filter((x) => x !== drag.id);
      const dst = [...(cols[to] ?? [])];
      const overIdx = String(e.over?.id).startsWith('t:') ? dst.indexOf(strip(String(e.over?.id))) : -1;
      dst.splice(overIdx >= 0 ? overIdx : dst.length, 0, drag.id);
      return { ...cols, [from]: src, [to]: dst };
    });
  };

  const endDrag = () => {
    setDrag(null);
    setDragCols(null);
    ui.setDraggingAgent(null);
  };

  const onDragEnd = (e: DragEndEvent) => {
    const d = drag;
    const cols = dragCols;
    endDrag();
    if (!d || !e.over) return;
    const st = useBoard.getState();
    if (d.type === 'agent') {
      const ticketId = e.over.data.current?.ticketId as string | undefined;
      if (!ticketId) return;
      if (isActive(activeRuns[ticketId])) {
        toast('Someone is already working on that ticket', 'bad');
        return;
      }
      void assignAgent(d.id, ticketId, 'drag');
      return;
    }
    if (!cols) return;
    const t = st.tickets[d.id];
    if (!t) return;
    const col = findCol(cols, tid(d.id));
    if (!col) return;
    let list = cols[col] ?? [];
    const overId = String(e.over.id);
    if (overId.startsWith('t:') && overId !== tid(d.id)) {
      const a = list.indexOf(d.id);
      const b = list.indexOf(strip(overId));
      if (a >= 0 && b >= 0) list = arrayMove(list, a, b);
    }
    const idx = list.indexOf(d.id);
    const pos = (id: string | undefined) => (id ? st.tickets[id]?.position : undefined);
    const before = pos(list[idx - 1]);
    const after = pos(list[idx + 1]);
    let p = positionBetween(before, after);
    if (col === t.column_id && list[idx - 1] === undefined && after === t.position) p = t.position;
    void moveTicket(t, col, p);
  };

  const overlay = useMemo(() => {
    if (!drag) return null;
    const st = useBoard.getState();
    if (drag.type === 'agent') {
      const a = st.agents[drag.id];
      return a ? (
        <div className="agent-ghost">
          <Sprite sprite={a.sprite} color={a.color} size={76} />
        </div>
      ) : null;
    }
    const t: Ticket | undefined = st.tickets[drag.id];
    return t ? <TicketPaper ticket={t} overlay /> : null;
  }, [drag]);

  if (!ready) return <Loading text="Connecting to the board…" />;

  const modal = ui.modal;
  return (
    <DndContext
      sensors={sensors}
      collisionDetection={collision}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onDragCancel={endDrag}
      accessibility={{
        announcements: {
          onDragStart: ({ active }) => `Picked up ${active.data.current?.label ?? 'item'}`,
          onDragOver: ({ over }) => (over ? `Over ${over.data.current?.label ?? 'a drop zone'}` : 'Not over a drop zone'),
          onDragEnd: ({ over }) => (over ? `Dropped on ${over.data.current?.label ?? 'target'}` : 'Dropped'),
          onDragCancel: () => 'Drag cancelled',
        },
      }}
    >
      <div className={`game ${ui.picked ? 'picking' : ''}`}>
        <Header activeTab={activeTab} onLoggedOut={onLoggedOut} />
        <div className={`game-main ${prefs.tavernCollapsed ? 'tavern-collapsed' : ''}`}>
          <Tavern />
          {ui.view === 'runners' ? (
            <RunnersPage />
          ) : ui.view === 'quests' ? (
            <QuestBoard />
          ) : ui.view === 'integrations' ? (
            <Integrations />
          ) : ui.view === 'portals' ? (
            <Portals />
          ) : activeTab ? (
            <Board tabId={activeTab} dragCols={dragCols} draggingTicket={drag?.type === 'ticket' ? drag.id : null} canEdit={canEdit} />
          ) : (
            <div className="board-wrap wood">
              <div className="sheet empty-state">
                <p>No tab yet. Create one to start pinning tickets.</p>
                <button className="btn gold" onClick={() => ui.show({ kind: 'tab-new' })}>
                  + New tab
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      <DragOverlay dropAnimation={drag?.type === 'agent' ? null : undefined}>{overlay}</DragOverlay>

      <AnimatePresence>
        {ui.panel?.kind === 'ticket' && <TicketPanel key={`t-${ui.panel.id}-${ui.panel.commentId ?? ''}-${ui.panel.runId ?? ''}`} ticketId={ui.panel.id} initialView={ui.panel.view} commentId={ui.panel.commentId} runId={ui.panel.runId} />}
        {ui.panel?.kind === 'agent' && <AgentPanel key={`a-${ui.panel.id}`} agentId={ui.panel.id} />}
        {ui.panel?.kind === 'history' && <HistoryDrawer key="history" />}
        {ui.panel?.kind === 'skills' && <SkillsDrawer key="skills" initialId={ui.panel.id} />}
        {ui.panel?.kind === 'quest' && <QuestPanel key={`q-${ui.panel.id}`} jobId={ui.panel.id} />}
      </AnimatePresence>

      {modal?.kind === 'ticket-new' && <TicketModal tabId={modal.tabId} columnId={modal.columnId} />}
      {modal?.kind === 'agent-new' && <AgentEditor />}
      {modal?.kind === 'agent-edit' && <AgentEditor agentId={modal.id} />}
      {modal?.kind === 'tab-new' && <TabEditor />}
      {modal?.kind === 'tab-edit' && <TabEditor tabId={modal.id} />}
      {modal?.kind === 'settings' && <SettingsModal section={modal.section} onLoggedOut={onLoggedOut} />}
      {modal?.kind === 'shortcuts' && <ShortcutsModal />}
      {modal?.kind === 'quest-new' && <QuestEditor />}
      {modal?.kind === 'quest-edit' && <QuestEditor jobId={modal.id} />}
      {modal?.kind === 'portal-new' && <PortalEditor presetKind={modal.preset} />}
      {modal?.kind === 'portal-edit' && <PortalEditor connectorId={modal.id} />}
      {modal?.kind === 'github-setup' && <GithubSetup org={modal.org} name={modal.name} />}
      {modal?.kind === 'profile' && <ProfileModal />}
      {modal?.kind === 'members' && <MembersModal />}
      <CommandBar />

      <FxLayer />
      <MusicController />
    </DndContext>
  );
}

import { useEffect, useRef, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { api, getTransport } from '../api/client';
import { connection } from '../api/connection';
import { PEARL, asset, avatarUrl } from '../lib/game';
import { compact } from '../lib/format';
import { attempt } from '../store/actions';
import { usePrefs } from '../store/prefs';
import { useCan, useOpenTicketCount, useTabs } from '../store/selectors';
import { useBoard } from '../store/store';
import { useUi } from '../store/ui';

const PLACES = [
  { view: 'quests', icon: '📜', label: 'Quests', key: 'Q' },
  { view: 'integrations', icon: '🔗', label: 'Repos', key: 'I' },
  { view: 'portals', icon: '🌀', label: 'Portals', key: 'P' },
] as const;

function TabButton({ id, active }: { id: string; active: boolean }) {
  const tab = useBoard((s) => s.tabs[id]);
  const count = useOpenTicketCount(id);
  const viewers = useBoard(useShallow((s) => s.presence.filter((p) => p.tab_id === id && p.user_id !== s.me?.id)));
  const setTab = useUi((u) => u.setTab);
  const show = useUi((u) => u.show);
  const canManage = useCan('manage_workspace');
  if (!tab) return null;
  return (
    <button
      role="tab"
      aria-selected={active}
      className={`tab ${active ? 'active' : ''}`}
      style={{ '--tab-c': tab.color } as React.CSSProperties}
      onClick={() => setTab(id)}
      onDoubleClick={() => canManage && show({ kind: 'tab-edit', id })}
      title={canManage ? 'Double-click to customise' : undefined}
    >
      <span className="ico" aria-hidden="true">
        {tab.icon}
      </span>
      {tab.name} <span className="count">{count}</span>
      {active && canManage && (
        <span
          className="tab-edit"
          role="button"
          tabIndex={0}
          aria-label={`Customise ${tab.name}`}
          onClick={(e) => {
            e.stopPropagation();
            show({ kind: 'tab-edit', id });
          }}
          onKeyDown={(e) => e.key === 'Enter' && show({ kind: 'tab-edit', id })}
        >
          ⚙
        </span>
      )}
      {viewers.length > 0 && (
        <span className="tab-viewers" aria-label={`${viewers.map((v) => v.name).join(', ')} viewing`}>
          {viewers.slice(0, 3).map((v) => (
            <img key={v.user_id} src={avatarUrl(v.avatar)} alt="" title={`${v.name} is here`} />
          ))}
        </span>
      )}
    </button>
  );
}

function Pearls() {
  const pearls = useBoard((s) => s.stats?.pearls ?? 0);
  const ref = useRef<HTMLDivElement>(null);
  const [shown, setShown] = useState(pearls);
  // Count up smoothly when pearls land.
  useEffect(() => {
    if (pearls === shown) return;
    const step = Math.max(1, Math.ceil(Math.abs(pearls - shown) / 12));
    const id = setTimeout(() => setShown((v) => (v < pearls ? Math.min(pearls, v + step) : Math.max(pearls, v - step))), 900 / 12);
    return () => clearTimeout(id);
  }, [pearls, shown]);
  return (
    <div className="pearls" id="pearl-counter" ref={ref} title="Team pearls: earned when a reviewer approves a PR" aria-label={`${pearls} pearls`}>
      <img src={PEARL} alt="" />
      <span>{compact(shown)}</span>
    </div>
  );
}

function UserMenu({ onLoggedOut }: { onLoggedOut: () => Promise<void> }) {
  const me = useBoard((s) => s.me);
  const [open, setOpen] = useState(false);
  const show = useUi((u) => u.show);
  const canUsers = useCan('manage_users');
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const h = (e: PointerEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener('pointerdown', h);
    return () => window.removeEventListener('pointerdown', h);
  }, [open]);
  if (!me) return null;
  const item = (label: string, fn: () => void) => (
    <button
      className="menu-item"
      role="menuitem"
      onClick={() => {
        setOpen(false);
        fn();
      }}
    >
      {label}
    </button>
  );
  return (
    <div style={{ position: 'relative' }} ref={ref}>
      <button className="presence-face me-btn" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((o) => !o)} title={`${me.name} (${me.role})`}>
        <img src={avatarUrl(me.avatar)} alt={me.name} />
      </button>
      {open && (
        <div className="menu wood" role="menu">
          <div className="sheet">
            <div style={{ padding: '6px 10px' }}>
              <b className="label">{me.name}</b>
              <div className="hint-text">
                {me.email} · {me.role}
              </div>
            </div>
            {item('⚙ Settings', () => show({ kind: 'settings' }))}
            {canUsers && item('✉ Invite a teammate', () => show({ kind: 'settings', section: 'team' }))}
            {item('⌨ Keyboard shortcuts', () => show({ kind: 'shortcuts' }))}
            {item('↩ Log out', async () => {
              await attempt(() => api.logout());
              connection.stop();
              useBoard.getState().reset();
              useBoard.getState().setMe(null);
              await onLoggedOut();
            })}
          </div>
        </div>
      )}
    </div>
  );
}

export function Header({ activeTab, onLoggedOut }: { activeTab: string | null; onLoggedOut: () => Promise<void> }) {
  const tabs = useTabs();
  const ws = useBoard((s) => s.settings);
  const conn = useBoard((s) => s.conn);
  const presence = useBoard((s) => s.presence);
  const meId = useBoard((s) => s.me?.id);
  const ui = useUi();
  const canManage = useCan('manage_workspace');
  const canEdit = useCan('edit_tickets');
  const canAgents = useCan('manage_agents');
  const musicPref = usePrefs((p) => p.music);
  const setPrefs = usePrefs((p) => p.set);
  const musicOn = musicPref ?? ws?.music_enabled_by_default ?? false;
  const others = presence.filter((p) => p.user_id !== meId);

  return (
    <header className="top wood">
      <div className="logo">
        <img src={asset('sprites/avatar/boba_avatar.webp')} alt="" />
        <h1>
          BOBA AGENT
          <small>{ws?.name}</small>
        </h1>
      </div>
      <nav className="tab-bar" aria-label="Boards">
        <div className="tabs" role="tablist">
          {tabs.map((t) => (
            <TabButton key={t.id} id={t.id} active={ui.view === 'board' && t.id === activeTab} />
          ))}
          {canManage && (
            <button className="tab" onClick={() => ui.show({ kind: 'tab-new' })} aria-label="New tab" title="New tab">
              +
            </button>
          )}
          <span className="tab-sep" aria-hidden="true" />
          {PLACES.map((p) => (
            <button
              key={p.view}
              role="tab"
              aria-selected={ui.view === p.view}
              className={`tab place ${ui.view === p.view ? 'active' : ''}`}
              onClick={() => ui.setView(ui.view === p.view ? 'board' : p.view)}
              title={`${p.label} (${p.key})`}
            >
              <span className="ico" aria-hidden="true">
                {p.icon}
              </span>
              {p.label}
            </button>
          ))}
        </div>
      </nav>
      <div className="top-spacer" />
      <div className="top-actions">
        {(ws?.demo_mode || getTransport().mock) && (
          <span className="demo-flag" title="No Claude Code key configured: runs are simulated">
            DEMO
          </span>
        )}
        {conn !== 'online' && <span className={`conn ${conn}`}>{conn === 'offline' ? 'Reconnecting…' : 'Connecting…'}</span>}
        {others.length > 0 && (
          <div className="presence" aria-label={`Online: ${others.map((o) => o.name).join(', ')}`}>
            {others.slice(0, 5).map((p) => (
              <span key={p.user_id} className="presence-face" title={`${p.name} is online${p.tab_id ? ` on ${useBoard.getState().tabs[p.tab_id]?.name ?? 'a tab'}` : ''}`}>
                <img src={avatarUrl(p.avatar)} alt={p.name} />
              </span>
            ))}
          </div>
        )}
        <Pearls />
        {canEdit && activeTab && (
          <button className="btn gold" onClick={() => ui.show({ kind: 'ticket-new', tabId: activeTab })} title="New ticket (N)">
            + Ticket
          </button>
        )}
        {canAgents && (
          <button className="btn" onClick={() => ui.show({ kind: 'agent-new' })} title="Recruit an agent (A)">
            + Agent
          </button>
        )}
        <button className="btn icon cmd-btn" onClick={() => ui.setCommand({ open: true })} title="Command bar (⌘K / Ctrl+K)" aria-label="Command bar">
          ⌘K
        </button>
        <button className="btn icon" onClick={() => ui.setCommand({ open: true, voice: true })} title="Talk to the Concierge (hold Space)" aria-label="Voice command">
          🎙
        </button>
        <button className="btn icon" onClick={() => ui.open(ui.panel?.kind === 'history' ? null : { kind: 'history' })} title="History (H)" aria-label="History">
          📜
        </button>
        <button className="btn icon" onClick={() => ui.open(ui.panel?.kind === 'skills' ? null : { kind: 'skills' })} title="Skills (K)" aria-label="Skills">
          📚
        </button>
        <button className={`btn icon ${musicOn ? 'on' : ''}`} onClick={() => setPrefs({ music: !musicOn })} title="Music (M)" aria-pressed={musicOn} aria-label="Music">
          ♪
        </button>
        <UserMenu onLoggedOut={onLoggedOut} />
      </div>
    </header>
  );
}

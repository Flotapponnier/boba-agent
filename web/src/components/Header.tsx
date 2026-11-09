import { useEffect, useRef, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { getTransport } from '../api/client';
import { PEARL, asset, portraitUrl } from '../lib/game';
import { compact } from '../lib/format';
import { usePrefs } from '../store/prefs';
import { useCan, useOpenTicketCount, useTabs } from '../store/selectors';
import { useBoard } from '../store/store';
import { AccountChip, Bell } from './team/Account';
import { Glyph, Icon } from './ui/Icon';
import { useUi } from '../store/ui';

const PLACES = [
  { view: 'quests', icon: 'quest', label: 'Quests', key: 'Q' },
  { view: 'integrations', icon: 'repo', label: 'Repos', key: 'I' },
  { view: 'portals', icon: 'portal', label: 'Portals', key: 'P' },
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
        <Glyph value={tab.icon} mono />
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
          <Icon name="gear" size={12} />
        </span>
      )}
      {viewers.length > 0 && (
        <span className="tab-viewers" aria-label={`${viewers.map((v) => v.name).join(', ')} viewing`}>
          {viewers.slice(0, 3).map((v) => (
            <img key={v.user_id} src={portraitUrl(v.avatar)} alt="" title={`${v.name} is here`} />
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
              <Icon name="plus" size={14} />
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
                <Icon name={p.icon} />
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
                <img src={portraitUrl(p.avatar)} alt={p.name} />
              </span>
            ))}
          </div>
        )}
        <Pearls />
        {canEdit && activeTab && (
          <button className="btn gold" onClick={() => ui.show({ kind: 'ticket-new', tabId: activeTab })} title="New ticket (N)">
            <Icon name="plus" size={14} /> Ticket
          </button>
        )}
        {canAgents && (
          <button className="btn" onClick={() => ui.show({ kind: 'agent-new' })} title="Recruit an agent (A)">
            <Icon name="plus" size={14} /> Agent
          </button>
        )}
        <button className="btn icon cmd-btn" onClick={() => ui.setCommand({ open: true })} title="Command bar (⌘K / Ctrl+K)" aria-label="Command bar">
          ⌘K
        </button>
        <button className="btn icon" onClick={() => ui.setCommand({ open: true, voice: true })} title="Talk to the Concierge (hold Space)" aria-label="Voice command">
          <Icon name="mic" size={20} />
        </button>
        <button className="btn icon" onClick={() => ui.open(ui.panel?.kind === 'history' ? null : { kind: 'history' })} title="History (H)" aria-label="History">
          <Icon name="clock" size={20} />
        </button>
        <button className="btn icon" onClick={() => ui.open(ui.panel?.kind === 'skills' ? null : { kind: 'skills' })} title="Skills (K)" aria-label="Skills">
          <Icon name="book" size={20} />
        </button>
        <button className={`btn icon ${musicOn ? 'on' : ''}`} onClick={() => setPrefs({ music: !musicOn })} title="Music (M)" aria-pressed={musicOn} aria-label="Music">
          <Icon name="music" size={20} />
        </button>
        <Bell />
        <AccountChip onLoggedOut={onLoggedOut} />
      </div>
    </header>
  );
}

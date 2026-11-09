import { useState } from 'react';
import { useAgentsList, useTabs } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Drawer } from '../ui/Overlay';
import { HistoryList, type Category } from './HistoryList';

export function HistoryDrawer() {
  const close = useUi((u) => u.open);
  const tabs = useTabs();
  const agents = useAgentsList();
  const stats = useBoard((s) => s.stats);
  const [cat, setCat] = useState<Category>('all');
  const [tab, setTab] = useState('');
  const [agent, setAgent] = useState('');

  return (
    <Drawer title="TIMELINE" onClose={() => close(null)}>
      <div className="sheet">
        {stats && (
          <div className="stat-grid" style={{ marginBottom: 12 }}>
            <div className="stat">
              <b>{stats.runs_completed}</b>
              <span>runs</span>
            </div>
            <div className="stat">
              <b>{stats.prs_approved}</b>
              <span>PRs approved</span>
            </div>
            <div className="stat">
              <b>{stats.pearls}</b>
              <span>pearls</span>
            </div>
            <div className="stat">
              <b>${stats.total_cost_usd.toFixed(2)}</b>
              <span>spent</span>
            </div>
          </div>
        )}
        <div className="filters">
          <select value={cat} onChange={(e) => setCat(e.target.value as Category)} aria-label="Event type">
            <option value="all">Everything</option>
            <option value="tickets">Tickets & comments</option>
            <option value="runs">Runs & PRs</option>
            <option value="reviews">Reviews</option>
            <option value="agents">Agents & levels</option>
            <option value="automation">Quests, sync & concierge</option>
            <option value="team">Team & workspace</option>
          </select>
          <select value={tab} onChange={(e) => setTab(e.target.value)} aria-label="Tab">
            <option value="">All tabs</option>
            {tabs.map((t) => (
              <option key={t.id} value={t.id}>
                {t.icon} {t.name}
              </option>
            ))}
          </select>
          <select value={agent} onChange={(e) => setAgent(e.target.value)} aria-label="Agent">
            <option value="">All agents</option>
            {agents.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </select>
        </div>
        <HistoryList query={{ tab_id: tab || null, agent_id: agent || null }} category={cat} />
      </div>
    </Drawer>
  );
}

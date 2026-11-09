import { useState, type CSSProperties } from 'react';
import type { ColumnKind, ColumnSpec } from '../../api/types';
import { KIND_COLORS, KIND_LABEL, SWATCHES } from '../../lib/game';
import { createTab, deleteTab, updateTab } from '../../store/actions';
import { sortedColumns, useAgentsList, useTabs } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';

const KINDS: ColumnKind[] = ['backlog', 'todo', 'doing', 'review', 'done', 'custom'];
const ICONS = ['🧋', '🔎', '🛠️', '🎨', '🧪', '📣', '📈', '🐛', '🚀', '📚', '🔒', '🌍'];

type Spec = ColumnSpec & { key: string };

const PRESETS: Record<string, [string, ColumnKind][]> = {
  Classic: [
    ['To do', 'todo'],
    ['Doing', 'doing'],
    ['Review', 'review'],
    ['Done', 'done'],
  ],
  'With backlog': [
    ['Backlog', 'backlog'],
    ['To do', 'todo'],
    ['Doing', 'doing'],
    ['PR · Review', 'review'],
    ['Done', 'done'],
  ],
  'Two reviews': [
    ['To do', 'todo'],
    ['Doing', 'doing'],
    ['Code review', 'review'],
    ['QA review', 'review'],
    ['Shipped', 'done'],
  ],
};

let k = 0;
const key = () => `k${++k}`;

export function TabEditor({ tabId }: { tabId?: string }) {
  const tab = useBoard((s) => (tabId ? s.tabs[tabId] : undefined));
  const repos = useBoard((s) => s.repos);
  const tabs = useTabs();
  const agents = useAgentsList();
  const show = useUi((u) => u.show);
  const reviewer = agents.find((a) => a.role.toLowerCase() === 'reviewer')?.id ?? null;
  const [f, setF] = useState(() => ({
    name: tab?.name ?? '',
    icon: tab?.icon ?? '🧋',
    color: tab?.color ?? '#f39c12',
    repo_id: tab?.repo_id ?? '',
  }));
  const [cols, setCols] = useState<Spec[]>(() =>
    tab
      ? sortedColumns(tab).map((c) => ({ key: key(), id: c.id, name: c.name, kind: c.kind, color: c.color, auto_reviewer_agent_id: c.auto_reviewer_agent_id, on_approve_column_id: c.on_approve_column_id, on_changes_column_id: c.on_changes_column_id }))
      : PRESETS.Classic!.map(([name, kind]) => ({ key: key(), id: null, name, kind, color: KIND_COLORS[kind], auto_reviewer_agent_id: kind === 'review' ? reviewer : null, on_approve_column_id: null, on_changes_column_id: null })),
  );
  const [busy, setBusy] = useState(false);
  const close = () => show(null);
  const patch = (i: number, p: Partial<Spec>) => setCols((cs) => cs.map((c, j) => (j === i ? { ...c, ...p } : c)));
  const move = (i: number, d: number) =>
    setCols((cs) => {
      const j = i + d;
      if (j < 0 || j >= cs.length) return cs;
      const n = [...cs];
      [n[i], n[j]] = [n[j] as Spec, n[i] as Spec];
      return n;
    });
  const tickets = useBoard((s) => s.tickets);
  const count = (id: string | null) => (id ? Object.values(tickets).filter((t) => t.column_id === id).length : 0);

  const save = async () => {
    setBusy(true);
    const columns = cols.map(({ key: _k, ...c }) => ({ ...c, name: c.name.trim() || KIND_LABEL[c.kind] }));
    const res = tab
      ? await updateTab(tab.id, { name: f.name.trim(), icon: f.icon, color: f.color, repo_id: f.repo_id || null, columns })
      : await createTab({ name: f.name.trim(), icon: f.icon, color: f.color, repo_id: f.repo_id || null, columns });
    setBusy(false);
    if (res) close();
  };

  const idx = tab ? tabs.findIndex((t) => t.id === tab.id) : -1;
  const existing = cols.filter((c) => c.id);

  return (
    <Modal title={tab ? `CUSTOMISE ${tab.name.toUpperCase()}` : 'NEW TAB'} onClose={close} wide>
      <div className="field-row">
        <label className="field">
          <span className="label">Name</span>
          <input autoFocus required maxLength={24} value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} placeholder="Dev, SEO, Design…" />
        </label>
        <label className="field">
          <span className="label">Default repo</span>
          <select value={f.repo_id} onChange={(e) => setF({ ...f, repo_id: e.target.value })}>
            <option value="">None</option>
            {Object.values(repos).map((r) => (
              <option key={r.id} value={r.id}>
                {r.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="field" style={{ marginTop: 10 }}>
        <span className="label">Icon & colour</span>
        <div className="swatches">
          {ICONS.map((i) => (
            <button key={i} type="button" className="swatch" style={{ background: f.icon === i ? '#fff3c4' : 'var(--paper-2)', fontSize: 15, display: 'grid', placeItems: 'center' }} aria-pressed={f.icon === i} onClick={() => setF({ ...f, icon: i })}>
              {i}
            </button>
          ))}
          <input maxLength={4} value={f.icon} onChange={(e) => setF({ ...f, icon: e.target.value })} style={{ width: 60 }} aria-label="Custom icon" />
        </div>
        <div className="swatches">
          {SWATCHES.map((c) => (
            <button key={c} type="button" className="swatch" style={{ background: c }} aria-pressed={f.color === c} aria-label={`Colour ${c}`} onClick={() => setF({ ...f, color: c })} />
          ))}
          <input type="color" value={f.color} onChange={(e) => setF({ ...f, color: e.target.value })} aria-label="Custom colour" />
        </div>
      </div>

      <div className="section-title">COLUMNS</div>
      {!tab && (
        <div className="row" style={{ marginBottom: 8 }}>
          <span className="hint-text">Presets:</span>
          {Object.entries(PRESETS).map(([name, p]) => (
            <button key={name} className="btn sm ghost" onClick={() => setCols(p.map(([n, kind]) => ({ key: key(), id: null, name: n, kind, color: KIND_COLORS[kind], auto_reviewer_agent_id: kind === 'review' ? reviewer : null, on_approve_column_id: null, on_changes_column_id: null })))}>
              {name}
            </button>
          ))}
        </div>
      )}
      {cols.map((c, i) => (
        <div key={c.key} className="col-row" style={{ '--c': c.color ?? KIND_COLORS[c.kind] } as CSSProperties}>
          <div className="move">
            <button type="button" onClick={() => move(i, -1)} disabled={i === 0} aria-label="Move left">
              ▲
            </button>
            <button type="button" onClick={() => move(i, 1)} disabled={i === cols.length - 1} aria-label="Move right">
              ▼
            </button>
          </div>
          <input value={c.name} onChange={(e) => patch(i, { name: e.target.value })} aria-label="Column name" maxLength={24} />
          <select className="kind" value={c.kind} onChange={(e) => {
            const kind = e.target.value as ColumnKind;
            patch(i, { kind, color: KIND_COLORS[kind], auto_reviewer_agent_id: kind === 'review' ? c.auto_reviewer_agent_id ?? reviewer : null });
          }} aria-label="Column kind">
            {KINDS.map((kd) => (
              <option key={kd} value={kd}>
                {KIND_LABEL[kd]}
              </option>
            ))}
          </select>
          <input type="color" value={c.color ?? KIND_COLORS[c.kind]} onChange={(e) => patch(i, { color: e.target.value })} aria-label="Pin colour" />
          <button
            type="button"
            className="icon-btn"
            style={{ background: 'var(--red)' }}
            aria-label={`Remove ${c.name}`}
            disabled={cols.length <= 1}
            title={count(c.id) ? `${count(c.id)} ticket(s) will move to the first To do column` : 'Remove'}
            onClick={() => setCols((cs) => cs.filter((_, j) => j !== i))}
          >
            ✕
          </button>
          {c.kind === 'review' && (
            <div className="review-opts">
              <label className="field">
                <span className="label">Auto reviewer</span>
                <select value={c.auto_reviewer_agent_id ?? ''} onChange={(e) => patch(i, { auto_reviewer_agent_id: e.target.value || null })}>
                  <option value="">Nobody (drag a reviewer manually)</option>
                  {agents.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name} ({a.role})
                    </option>
                  ))}
                </select>
              </label>
              <label className="field">
                <span className="label">When approved</span>
                <select value={c.on_approve_column_id ?? ''} onChange={(e) => patch(i, { on_approve_column_id: e.target.value || null })}>
                  <option value="">First Done column</option>
                  {existing.filter((x) => x.id !== c.id).map((x) => (
                    <option key={x.id} value={x.id ?? ''}>
                      {x.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="field">
                <span className="label">When changes requested</span>
                <select value={c.on_changes_column_id ?? ''} onChange={(e) => patch(i, { on_changes_column_id: e.target.value || null })}>
                  <option value="">First To do column</option>
                  {existing.filter((x) => x.id !== c.id).map((x) => (
                    <option key={x.id} value={x.id ?? ''}>
                      {x.name}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          )}
        </div>
      ))}
      <button className="btn sm" style={{ marginTop: 8 }} onClick={() => setCols((cs) => [...cs, { key: key(), id: null, name: 'New column', kind: 'custom', color: KIND_COLORS.custom, auto_reviewer_agent_id: null, on_approve_column_id: null, on_changes_column_id: null }])}>
        + Column
      </button>
      <p className="hint-text" style={{ marginTop: 6 }}>
        Kind drives the automation: dropping a boba moves the ticket to Doing, finished work goes to Review, the reviewer sends it to Done or back to To do.
      </p>

      <div className="row between" style={{ marginTop: 14 }}>
        <div className="row">
          {tab && (
            <>
              <button className="btn sm" disabled={idx <= 0} onClick={() => void updateTab(tab.id, { position: idx - 1 })} title="Move tab left">
                ◀
              </button>
              <button className="btn sm" disabled={idx >= tabs.length - 1} onClick={() => void updateTab(tab.id, { position: idx + 1 })} title="Move tab right">
                ▶
              </button>
              <button
                className="btn sm red"
                onClick={async () => {
                  if (confirm(`Delete the tab ${tab.name} and all its tickets?`) && (await deleteTab(tab.id))) close();
                }}
              >
                Delete tab
              </button>
            </>
          )}
        </div>
        <div className="row">
          <button className="btn ghost" onClick={close}>
            Cancel
          </button>
          <button className="btn gold" disabled={busy || !f.name.trim() || cols.length === 0} onClick={() => void save()}>
            {tab ? 'Save' : 'Create tab'}
          </button>
        </div>
      </div>
    </Modal>
  );
}

import { useState, type FormEvent } from 'react';
import type { Priority } from '../../api/types';
import { PRIORITIES, PRIORITY_LABEL } from '../../lib/game';
import { createTicket } from '../../store/actions';
import { sortedColumns } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';

export function TicketModal({ tabId, columnId }: { tabId: string; columnId?: string }) {
  const tab = useBoard((s) => s.tabs[tabId]);
  const repos = useBoard((s) => s.repos);
  const show = useUi((u) => u.show);
  const cols = tab ? sortedColumns(tab).filter((c) => c.kind !== 'doing' && c.kind !== 'review' && c.kind !== 'done') : [];
  const [f, setF] = useState({
    title: '',
    body: '',
    tags: '',
    priority: 'normal' as Priority,
    column: columnId ?? cols.find((c) => c.kind === 'todo')?.id ?? cols[0]?.id ?? '',
    repo: '',
  });
  const [busy, setBusy] = useState(false);
  const close = () => show(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    const t = await createTicket({
      tab_id: tabId,
      column_id: f.column || null,
      title: f.title.trim(),
      body: f.body.trim() || null,
      tags: f.tags
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean),
      priority: f.priority,
      color: null,
      repo_id: f.repo || null,
    });
    setBusy(false);
    if (t) close();
  }

  return (
    <Modal title="PIN A NEW TICKET" onClose={close}>
      <form onSubmit={submit}>
        <label className="field">
          <span className="label">Title</span>
          <input required autoFocus maxLength={120} value={f.title} onChange={(e) => setF({ ...f, title: e.target.value })} placeholder="Fix the fee calculation on forex pairs" />
        </label>
        <label className="field">
          <span className="label">Task for the agent (markdown)</span>
          <textarea value={f.body} onChange={(e) => setF({ ...f, body: e.target.value })} placeholder={'What should be done, how to check it is done.\n\n- Reproduce with a test first\n- Keep the public API'} style={{ minHeight: 140 }} />
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
          {cols.length > 1 && (
            <label className="field">
              <span className="label">Column</span>
              <select value={f.column} onChange={(e) => setF({ ...f, column: e.target.value })}>
                {cols.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </label>
          )}
          {Object.keys(repos).length > 0 && (
            <label className="field">
              <span className="label">Repo</span>
              <select value={f.repo} onChange={(e) => setF({ ...f, repo: e.target.value })}>
                <option value="">Tab default{tab?.repo_id ? ` (${repos[tab.repo_id]?.name})` : ''}</option>
                {Object.values(repos).map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.name}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>
        <div className="row end" style={{ marginTop: 14 }}>
          <button type="button" className="btn ghost" onClick={close}>
            Cancel
          </button>
          <button className="btn gold" disabled={busy || !f.title.trim()}>
            📌 Pin it
          </button>
        </div>
      </form>
    </Modal>
  );
}

import { memo, useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { Repo, RepoSync } from '../../api/types';
import { sfx } from '../../audio/audio';
import { timeAgo } from '../../lib/format';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { useCan, useTabs } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { UNAVAILABLE, useAvailable } from '../../store/features';

function Toggle({ checked, onChange, label, hint, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string; disabled?: boolean }) {
  return (
    <label className="toggle-row">
      <span className="switch">
        <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
        <span />
      </span>
      <span>
        {label}
        {hint && <small>{hint}</small>}
      </span>
    </label>
  );
}

const RepoCard = memo(function RepoCard({ repo }: { repo: Repo }) {
  const tabs = useTabs();
  const canManage = useCan('manage_automations');
  const canRun = useCan('run_agents');
  const imported = useBoard((s) => Object.values(s.tickets).filter((t) => (t.source.kind === 'github_issue' || t.source.kind === 'github_pr') && t.source.repo_id === repo.id).length);
  const [sync, setSync] = useState<RepoSync>(repo.sync);
  const [attribution, setAttribution] = useState(repo.ai_attribution);
  const [labelIn, setLabelIn] = useState('');
  const [busy, setBusy] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const avail = useAvailable('sync');
  const dirty =
    attribution !== repo.ai_attribution || JSON.stringify({ ...sync, last_synced_at: null, last_error: null }) !== JSON.stringify({ ...repo.sync, last_synced_at: null, last_error: null });
  useEffect(() => setSync((s) => ({ ...s, last_synced_at: repo.sync.last_synced_at, last_error: repo.sync.last_error })), [repo.sync.last_synced_at, repo.sync.last_error]);
  const set = <K extends keyof RepoSync>(k: K, v: RepoSync[K]) => setSync((s) => ({ ...s, [k]: v }));
  const ro = !canManage;

  const save = async () => {
    setBusy(true);
    const r = await attempt(() => api.updateRepo(repo.id, { sync, ai_attribution: attribution }));
    setBusy(false);
    if (r) {
      useBoard.getState().upsert({ entity: 'repo', data: r });
      toast(`${r.name} sync saved`, 'good');
    }
  };
  const syncNow = async () => {
    setSyncing(true);
    const rep = await attempt(() => api.syncRepo(repo.id));
    setSyncing(false);
    if (!rep) return;
    const n = rep.imported_issues + rep.imported_prs;
    if (rep.errors.length) toast(`Sync finished with errors: ${rep.errors[0]}`, 'bad');
    else {
      if (n) sfx('pearl');
      toast(`${repo.name}: ${rep.imported_issues} issue(s), ${rep.imported_prs} PR(s) imported · ${rep.updated} updated · ${rep.closed} closed`, n ? 'gold' : 'good');
    }
  };

  return (
    <article className={`repo-card sheet ${sync.enabled ? 'on' : ''}`}>
      <header className="row">
        <span className="repo-ico" aria-hidden="true">
          🐙
        </span>
        <div className="grow">
          <h3>{repo.name}</h3>
          <div className="hint-text">
            {repo.remote_url ?? repo.path} · {repo.default_branch}
          </div>
        </div>
        <span className={`run-status ${repo.sync.last_error ? 'failed' : repo.sync.enabled ? 'succeeded' : ''}`}>{repo.sync.last_error ? 'Error' : repo.sync.enabled ? 'Syncing' : 'Off'}</span>
      </header>
      <div className="row hint-text" style={{ margin: '6px 0' }}>
        <span>Last sync: {repo.sync.last_synced_at ? timeAgo(repo.sync.last_synced_at) : 'never'}</span>
        <span>· {imported} imported ticket{imported === 1 ? '' : 's'} on the board</span>
      </div>
      {repo.sync.last_error && <p className="form-error">⚠ {repo.sync.last_error}</p>}

      <fieldset disabled={ro} className="sync-grid">
        <Toggle checked={sync.enabled} onChange={(v) => set('enabled', v)} label="Sync with GitHub" hint={`Polls every ${sync.poll_minutes} min (and on webhooks if configured)`} />
        <label className="field">
          <span className="label">Imported tickets land in</span>
          <select value={sync.target_tab_id ?? ''} onChange={(e) => set('target_tab_id', e.target.value || null)}>
            <option value="">Pick a tab…</option>
            {tabs.map((t) => (
              <option key={t.id} value={t.id}>
                {t.icon} {t.name}
              </option>
            ))}
          </select>
        </label>
        <Toggle checked={sync.import_issues} onChange={(v) => set('import_issues', v)} label="Import open issues" hint="Into the tab's first To do column" />
        <div className="field">
          <span className="label">Only issues labelled (empty = all)</span>
          <div className="chips">
            {sync.issue_labels.map((l) => (
              <span key={l} className="chip x-able">
                {l}
                <button type="button" aria-label={`Remove ${l}`} onClick={() => set('issue_labels', sync.issue_labels.filter((x) => x !== l))}>
                  ×
                </button>
              </span>
            ))}
            <input
              value={labelIn}
              onChange={(e) => setLabelIn(e.target.value)}
              placeholder="+ label"
              style={{ width: 110, minHeight: 28, fontSize: 16 }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  const v = labelIn.trim();
                  if (v && !sync.issue_labels.includes(v)) set('issue_labels', [...sync.issue_labels, v]);
                  setLabelIn('');
                }
              }}
              aria-label="Add a label filter"
            />
          </div>
        </div>
        <Toggle checked={sync.import_prs} onChange={(v) => set('import_prs', v)} label="Import open PRs" hint="Into the Review column, so its auto reviewer reviews them (outside contributors too)" />
        <Toggle checked={sync.skip_draft_prs} onChange={(v) => set('skip_draft_prs', v)} label="Skip draft PRs" disabled={!sync.import_prs} />
        <Toggle checked={sync.comment_back} onChange={(v) => set('comment_back', v)} label="Comment back on GitHub" hint="Run summaries and verdicts as comments" />
        <Toggle checked={sync.post_reviews} onChange={(v) => set('post_reviews', v)} label="Post real PR reviews" hint="Approve / request changes as the bot account" />
        <Toggle checked={sync.close_issues_on_done} onChange={(v) => set('close_issues_on_done', v)} label="Close issues when Done" />
        <Toggle
          checked={attribution}
          onChange={setAttribution}
          label="Allow AI attribution in commits (Co-Authored-By: Claude)"
          hint="Off: agents' commits and PRs on this repo carry no Claude attribution trailer"
        />
        <label className="field">
          <span className="label">Poll every (minutes)</span>
          <input type="number" min={1} max={1440} value={sync.poll_minutes} onChange={(e) => set('poll_minutes', Math.max(1, Number(e.target.value) || 10))} style={{ width: 110 }} />
        </label>
      </fieldset>

      <div className="row end" style={{ marginTop: 10 }}>
        {canManage && (
          <button className="btn sm" disabled={!avail || !dirty || busy} title={avail ? undefined : UNAVAILABLE} onClick={() => void save()}>
            Save
          </button>
        )}
        {canRun && (
          <button className="btn gold sm" disabled={!avail || syncing || dirty} title={!avail ? UNAVAILABLE : dirty ? 'Save first' : undefined} onClick={() => void syncNow()}>
            {syncing ? '⟳ Syncing…' : '⟳ Sync now'}
          </button>
        )}
      </div>
    </article>
  );
});

export function Integrations() {
  const repos = useBoard((s) => s.repos);
  const show = useUi((u) => u.show);
  const canWs = useCan('manage_workspace');
  const list = Object.values(repos).sort((a, b) => a.name.localeCompare(b.name));
  return (
    <section className="page wood" aria-label="Integrations">
      <div className="page-head">
        <div>
          <div className="sign" style={{ textAlign: 'left' }}>
            🔗 INTEGRATIONS
          </div>
          <p className="page-sub">Connect repositories: GitHub issues become tickets, outside PRs go straight to your reviewer, and results flow back to GitHub.</p>
        </div>
        {canWs && (
          <button className="btn gold" onClick={() => show({ kind: 'settings', section: 'repos' })}>
            + Connect a repo
          </button>
        )}
      </div>
      <div className="repo-grid">
        {list.map((r) => (
          <RepoCard key={r.id} repo={r} />
        ))}
        {list.length === 0 && (
          <div className="sheet empty-state">
            <p>No repository yet.</p>
          </div>
        )}
      </div>
    </section>
  );
}

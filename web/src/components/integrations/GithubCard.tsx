import { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { GithubAppStatus, GithubRepo, Repo } from '../../api/types';
import { sfx } from '../../audio/audio';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { UNAVAILABLE, useAvailable, useFeatures } from '../../store/features';
import { useCan, useTabs } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { Modal } from '../ui/Overlay';
import { useUi } from '../../store/ui';
import { Brand, Icon } from '../ui/Icon';

/** GitHub sends people back to /settings/integrations after creating / installing the app. */
export function handleGithubReturn(): boolean {
  if (location.pathname.replace(/\/$/, '') !== '/settings/integrations') return false;
  history.replaceState(null, '', '/' + location.hash);
  return true;
}

function startSetup(org: string, name: string) {
  // Game-styled confirm page; it fetches the manifest (fresh single-use state) when it opens.
  useUi.getState().show({ kind: 'github-setup', org: org.trim() || undefined, name: name.trim() || undefined });
}

function RepoPicker({ onClose }: { onClose: () => void }) {
  const tabs = useTabs();
  const [repos, setRepos] = useState<GithubRepo[] | null>(null);
  const [q, setQ] = useState('');
  const [adding, setAdding] = useState<string | null>(null);
  const [added, setAdded] = useState<Record<string, Repo>>({});
  const [syncTab, setSyncTab] = useState(tabs[0]?.id ?? '');
  const [withSync, setWithSync] = useState(true);

  useEffect(() => {
    api
      .githubRepos()
      .then(setRepos)
      .catch((e) => {
        toast(e instanceof Error ? e.message : 'Could not list GitHub repos', 'bad');
        setRepos([]);
      });
  }, []);

  const list = useMemo(() => (repos ?? []).filter((r) => r.full_name.toLowerCase().includes(q.toLowerCase())), [repos, q]);

  const add = async (r: GithubRepo) => {
    setAdding(r.full_name);
    const repo = await attempt(() => api.createRepo({ name: r.full_name, path: '', remote_url: r.html_url, default_branch: r.default_branch, runner_id: null, sync: null }));
    if (repo) {
      useBoard.getState().upsert({ entity: 'repo', data: repo });
      setAdded((a) => ({ ...a, [r.full_name]: repo }));
      sfx('pearl', 0.6);
      if (withSync && syncTab) {
        const patched = await attempt(() => api.updateRepo(repo.id, { sync: { ...repo.sync, enabled: true, target_tab_id: syncTab } }));
        if (patched) {
          useBoard.getState().upsert({ entity: 'repo', data: patched });
          const rep = await attempt(() => api.syncRepo(repo.id));
          if (rep) toast(`${r.full_name} added: ${rep.imported_issues} issue(s), ${rep.imported_prs} PR(s) imported`, 'gold');
        }
      } else toast(`${r.full_name} added to the board`, 'good');
    }
    setAdding(null);
  };

  return (
    <Modal title="ADD A REPO FROM GITHUB" onClose={onClose} wide>
      <div className="row" style={{ marginBottom: 10 }}>
        <input className="grow" type="search" placeholder="Search repos…" value={q} onChange={(e) => setQ(e.target.value)} autoFocus aria-label="Search repos" />
      </div>
      <label className="row" style={{ marginBottom: 10 }}>
        <input type="checkbox" checked={withSync} onChange={(e) => setWithSync(e.target.checked)} />
        <span>Import its issues and PRs into</span>
        <select value={syncTab} onChange={(e) => setSyncTab(e.target.value)} disabled={!withSync} style={{ width: 'auto' }} aria-label="Target tab">
          {tabs.map((t) => (
            <option key={t.id} value={t.id}>
              {t.name}
            </option>
          ))}
        </select>
      </label>
      <div className="gh-repos">
        {repos === null && <p className="muted">Asking GitHub…</p>}
        {repos !== null && list.length === 0 && <p className="muted">No repo found. Install the app on more repos first.</p>}
        {list.map((r) => {
          const on = r.repo_id || added[r.full_name];
          return (
            <div key={r.full_name} className={`gh-repo ${on ? 'on' : ''}`}>
              <span className="grow" style={{ minWidth: 0 }}>
                <a href={r.html_url} target="_blank" rel="noreferrer noopener">
                  <b>{r.full_name}</b>
                </a>{' '}
                {r.private && <span className="badge">private</span>}
                <br />
                <span className="hint-text">{r.default_branch}</span>
              </span>
              {on ? (
                <span className="run-status succeeded">On the board</span>
              ) : adding === r.full_name ? (
                <span className="cloning">
                  <span className="dots">
                    <i />
                    <i />
                    <i />
                  </span>{' '}
                  Cloning…
                </span>
              ) : (
                <button className="btn sm gold" disabled={!!adding} onClick={() => void add(r)}>
                  + Add
                </button>
              )}
            </div>
          );
        })}
      </div>
      <p className="hint-text" style={{ marginTop: 10 }}>
        The server clones the repo with the app's token. Big repos take a moment.
      </p>
    </Modal>
  );
}

export function GithubCard() {
  const avail = useAvailable('github');
  const canAuto = useCan('manage_automations');
  const canWs = useCan('manage_workspace');
  const [st, setSt] = useState<GithubAppStatus | null>(null);
  const [org, setOrg] = useState('');
  const [name, setName] = useState('Boba Agent');
  const [picker, setPicker] = useState(false);

  const load = useCallback(() => {
    if (useFeatures.getState().missing.github) return;
    api
      .githubApp()
      .then(setSt)
      .catch(() => setSt(null));
  }, []);
  useEffect(() => {
    load();
    window.addEventListener('boba:github-changed', load);
    window.addEventListener('focus', load); // back from the GitHub tab
    return () => {
      window.removeEventListener('boba:github-changed', load);
      window.removeEventListener('focus', load);
    };
  }, [load]);

  if (!avail) return <div className="sheet soon">Connecting GitHub is {UNAVAILABLE.toLowerCase()}.</div>;
  if (!st) return <div className="sheet gh-card muted">Checking GitHub…</div>;

  return (
    <article className={`sheet gh-card ${st.configured ? 'on' : ''}`}>
      <div className="row">
        <span className="gh-octo" aria-hidden="true">
          <Brand name="github" size={28} mono />
        </span>
        <div className="grow">
          <h3>{st.configured ? 'GitHub is connected' : 'Connect GitHub'}</h3>
          <p className="hint-text">
            {st.configured
              ? 'Bobas clone, push and open PRs with the Boba Agent GitHub App. Issues and PRs flow into the board.'
              : 'Create a GitHub App for this server in two clicks. It gives the bobas their own identity to clone, push, open PRs and sync issues, with only the repos you pick.'}
          </p>
        </div>
      </div>
      {!st.configured ? (
        canAuto ? (
          <div className="gh-setup">
            <div className="field-row">
              <label className="field">
                <span className="label">Organisation (empty = your account)</span>
                <input value={org} onChange={(e) => setOrg(e.target.value)} placeholder="boba-labs" />
              </label>
              <label className="field">
                <span className="label">App name</span>
                <input value={name} onChange={(e) => setName(e.target.value)} maxLength={34} />
              </label>
            </div>
            <div className="row" style={{ marginTop: 10 }}>
              <button className="btn gold" onClick={() => startSetup(org, name)}>
                <Brand name="github" size={14} mono /> Create the Boba Agent GitHub App
              </button>
              <span className="hint-text">You'll confirm on GitHub, pick the repos, and come back here.</span>
            </div>
          </div>
        ) : (
          <p className="hint-text">Ask an admin to connect GitHub.</p>
        )
      ) : (
        <>
          <div className="row" style={{ margin: '8px 0' }}>
            {st.html_url && (
              <a className="chip" href={st.html_url} target="_blank" rel="noreferrer noopener">
                <Icon name="robot" size={12} /> {st.slug}
              </a>
            )}
            <span className="grow" />
            {st.install_url && canAuto && (
              <a className="btn sm" href={st.install_url} target="_blank" rel="noreferrer noopener">
                <Icon name="plus" size={12} /> Install on more repos
              </a>
            )}
            {canWs && (
              <button className="btn sm gold" onClick={() => setPicker(true)}>
                + Add repo from GitHub
              </button>
            )}
          </div>
          {st.installations.length === 0 && <p className="form-error">The app isn't installed anywhere yet. Use “Install on more repos”.</p>}
          {st.installations.map((i) => (
            <div key={i.id} className="gh-install">
              <b>@{i.account}</b> <span className="hint-text">· {i.repos.length} repo{i.repos.length === 1 ? '' : 's'}</span>
              <div className="chips" style={{ marginTop: 4 }}>
                {i.repos.slice(0, 12).map((r) => (
                  <span key={r} className="chip">
                    {r}
                  </span>
                ))}
                {i.repos.length > 12 && <span className="hint-text">+{i.repos.length - 12} more</span>}
              </div>
            </div>
          ))}
        </>
      )}
      {picker && (
        <RepoPicker
          onClose={() => {
            setPicker(false);
            load();
          }}
        />
      )}
    </article>
  );
}

import { useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { RunnerAuth, RunnerInfo, RunnerScope, RunnerToken } from '../../api/types';
import { sfx } from '../../audio/audio';
import { portraitUrl } from '../../lib/game';
import { timeAgo } from '../../lib/format';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { Modal } from '../ui/Overlay';
import { UNAVAILABLE, useAvailable, useFeatures } from '../../store/features';
import { Icon } from '../ui/Icon';

export const INSTALL_CMD = 'curl -fsSL https://raw.githubusercontent.com/Flotapponnier/boba-agent/main/install.sh | sh';
export const RUNNER_DOCS = 'https://github.com/Flotapponnier/boba-agent/blob/main/docs/RUNNER.md';

function Copyable({ text, label }: { text: string; label: string }) {
  return (
    <div className="copyable">
      <pre className="cmd-snippet">{text}</pre>
      <button className="btn sm" onClick={() => void navigator.clipboard?.writeText(text).then(() => toast(`${label} copied`, 'good'))}>
        <Icon name="copy" size={14} /> Copy
      </button>
    </div>
  );
}

/** The three steps from docs/RUNNER.md, shown before and after the token. */
function InstallSteps({ personal }: { personal: boolean }) {
  return (
    <ol className="install-steps">
      <li>
        <b>Install boba-agent</b> (macOS / Linux, checksum-verified):
        <Copyable text={INSTALL_CMD} label="Install command" />
      </li>
      <li>
        <b>{personal ? 'Sign in to Claude Code' : 'Give it organisation credentials'}</b>
        {personal ? (
          <>
            : <code>claude auth login</code>, then check <code>claude auth status</code>. For pull requests: <code>gh auth login</code>.
          </>
        ) : (
          <>
            : <code>ANTHROPIC_API_KEY</code> (or Bedrock / Vertex), plus <code>gh auth login</code> or <code>GH_TOKEN</code>.
          </>
        )}
      </li>
      <li>
        <b>Connect</b> with the command below (the token is shown once).
      </li>
    </ol>
  );
}

const AUTH: Record<RunnerAuth, { label: string; tone: string }> = {
  api_key: { label: 'API key', tone: 'ok' },
  bedrock: { label: 'Bedrock', tone: 'ok' },
  vertex: { label: 'Vertex', tone: 'ok' },
  subscription: { label: 'Claude subscription', tone: 'personal' },
  demo: { label: 'Demo (simulated)', tone: 'demo' },
  unknown: { label: 'Not reported yet', tone: 'demo' },
};

function RunnerCard({ r }: { r: RunnerInfo }) {
  const owner = useBoard((s) => (r.owner_user_id ? s.users[r.owner_user_id] : undefined));
  const me = useBoard((s) => s.me);
  const canWs = useCan('manage_workspace');
  const canRemove = r.id !== 'local' && (r.owner_user_id === me?.id || canWs);
  const pct = r.max_concurrent_runs ? Math.round((r.active_runs / r.max_concurrent_runs) * 100) : 0;
  return (
    <article className={`runner sheet ${r.online ? 'online' : ''}`}>
      <div className="row">
        <span className={`dot-online ${r.online ? 'on' : ''}`} title={r.online ? 'Online' : 'Offline'} />
        <b className="grow runner-name">{r.name}</b>
        <span className={`scope ${r.scope}`}>{r.scope === 'shared' ? 'Shared' : 'Personal'}</span>
      </div>
      <dl className="kv" style={{ marginTop: 6 }}>
        {owner && (
          <>
            <dt>OWNER</dt>
            <dd className="row" style={{ gap: 6 }}>
              <img src={portraitUrl(owner.avatar)} alt="" width={22} height={22} style={{ borderRadius: 4 }} /> {owner.name}
            </dd>
          </>
        )}
        <dt>CLAUDE</dt>
        <dd>
          <span className={`auth ${AUTH[r.auth].tone}`}>{AUTH[r.auth].label}</span> {r.claude_version && <span className="hint-text">v{r.claude_version}</span>}
        </dd>
        <dt>HOST</dt>
        <dd>{r.host ?? '—'}</dd>
        <dt>LOAD</dt>
        <dd>
          <div className="xp" style={{ width: 140, display: 'inline-block', verticalAlign: 'middle' }}>
            <i style={{ width: `${pct}%` }} />
          </div>{' '}
          {r.active_runs}/{r.max_concurrent_runs} runs
        </dd>
        <dt>SEEN</dt>
        <dd>{r.online ? 'now' : r.last_seen_at ? timeAgo(r.last_seen_at) : 'never'}</dd>
      </dl>
      {canRemove && (
        <div className="row end" style={{ marginTop: 8 }}>
          <button
            className="btn sm red"
            onClick={async () => {
              if (!confirm(`Disconnect ${r.name}? Its token stops working.`)) return;
              const ok = await attempt(() => api.deleteRunner(r.id).then(() => true));
              if (ok) {
                useBoard.getState().remove('runner', r.id);
                toast(`${r.name} disconnected`, 'good');
              }
            }}
          >
            Disconnect
          </button>
        </div>
      )}
    </article>
  );
}

function ConnectModal({ scope, onClose }: { scope: RunnerScope; onClose: () => void }) {
  const me = useBoard((s) => s.me);
  const [name, setName] = useState(scope === 'personal' ? `${me?.name ?? 'My'}'s machine` : 'vps-runner');
  const [slots, setSlots] = useState(scope === 'personal' ? 2 : 4);
  const [token, setToken] = useState<RunnerToken | null>(null);
  const [busy, setBusy] = useState(false);
  const create = async () => {
    setBusy(true);
    const t = await attempt(() => api.createRunner({ name: name.trim(), scope, max_concurrent_runs: slots }));
    setBusy(false);
    if (!t && useFeatures.getState().missing.runners) return onClose();
    if (t) {
      useBoard.getState().upsert({ entity: 'runner', data: t.runner });
      setToken(t);
      sfx('done', 0.6);
    }
  };
  return (
    <Modal title={scope === 'personal' ? 'CONNECT MY MACHINE' : 'ADD A SHARED RUNNER'} onClose={onClose}>
      {!token ? (
        <>
          {scope === 'personal' ? (
            <p className="hint-text" style={{ marginBottom: 10 }}>
              Your machine runs <b>only the work you start</b> (drag, @mention, follow-ups) with <b>your own</b> Claude Code login. Nobody else's runs ever use it, so your Claude Pro/Max
              subscription stays personal, as Anthropic's terms require.
            </p>
          ) : (
            <p className="hint-text" style={{ marginBottom: 10 }}>
              A shared runner takes anyone's runs, quests and auto reviews. It must use an organisation <b>API key</b>, <b>Bedrock</b> or <b>Vertex</b>. A shared runner logged in with a personal subscription is refused.
            </p>
          )}
          <InstallSteps personal={scope === 'personal'} />
          <label className="field">
            <span className="label">Name</span>
            <input autoFocus value={name} onChange={(e) => setName(e.target.value)} maxLength={40} />
          </label>
          <label className="field">
            <span className="label">Runs at the same time</span>
            <input type="number" min={1} max={16} value={slots} onChange={(e) => setSlots(Math.max(1, Number(e.target.value) || 1))} style={{ width: 100 }} />
          </label>
          <div className="row end" style={{ marginTop: 12 }}>
            <button className="btn ghost" onClick={onClose}>
              Cancel
            </button>
            <button className="btn gold" disabled={busy || !name.trim()} onClick={() => void create()}>
              Create token
            </button>
          </div>
        </>
      ) : (
        <>
          <p>
            Run this on the machine{scope === 'personal' ? ' where you are logged in to Claude Code' : ''}. <b>The token is shown only once.</b>
          </p>
          <p className="hint-text">1. Install (if not done yet):</p>
          <Copyable text={INSTALL_CMD} label="Install command" />
          <p className="hint-text">2. Connect this machine:</p>
          <Copyable text={token.command} label="Connect command" />
          <p className="hint-text">
            It connects out over WebSocket, so it works behind a NAT or a laptop firewall. Full guide:{' '}
            <a href={RUNNER_DOCS} target="_blank" rel="noreferrer noopener">
              docs/RUNNER.md <Icon name="external" size={12} />
            </a>
          </p>
          <div className="row end" style={{ marginTop: 12 }}>
            <button className="btn gold" onClick={onClose}>
              Done
            </button>
          </div>
        </>
      )}
    </Modal>
  );
}

export function RunnersPage() {
  const runners = useBoard((s) => s.runners);
  const me = useBoard((s) => s.me);
  const canWs = useCan('manage_workspace');
  const canRun = useCan('run_agents');
  const [connect, setConnect] = useState<RunnerScope | null>(null);
  const avail = useAvailable('runners');
  const list = useMemo(() => Object.values(runners).sort((a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name)), [runners]);
  const mine = list.filter((r) => r.scope === 'personal' && r.owner_user_id === me?.id);
  const shared = list.filter((r) => r.scope === 'shared');
  const others = list.filter((r) => r.scope === 'personal' && r.owner_user_id !== me?.id);
  return (
    <section className="page wood" aria-label="Runners">
      <div className="page-head">
        <div>
          <div className="sign" style={{ textAlign: 'left' }}>
            <Icon name="terminal" size={24} /> CONNECT YOUR CLAUDE CODE
          </div>
          <p className="page-sub">Runners are the machines where bobas actually run Claude Code. Your runs go to your own machine when it's online, otherwise to a shared runner.</p>
        </div>
        <div className="row">
          {canRun && (
            <button className="btn gold" disabled={!avail} title={avail ? undefined : UNAVAILABLE} onClick={() => setConnect('personal')}>
              <Icon name="plug" size={14} /> Connect my machine
            </button>
          )}
          {canWs && (
            <button className="btn" disabled={!avail} title={avail ? undefined : UNAVAILABLE} onClick={() => setConnect('shared')}>
              + Shared runner
            </button>
          )}
        </div>
      </div>
      <div className="runner-explain sheet">
        <div style={{ gridColumn: '1 / -1' }} className="row between">
          <span className="hint-text">
            New here? Install with <code>curl … install.sh | sh</code>, sign in to Claude Code, then Connect my machine.
          </span>
          <a className="btn sm" href={RUNNER_DOCS} target="_blank" rel="noreferrer noopener">
            <Icon name="book" size={14} /> Runner guide
          </a>
        </div>
        <div>
          <b>
            <Icon name="user" size={14} /> Personal
          </b>
          <p>Your laptop, your Claude login. Runs only what you start. Your subscription is never shared.</p>
        </div>
        <div>
          <b>
            <Icon name="server" size={14} /> Shared
          </b>
          <p>A server with an organisation API key (or Bedrock / Vertex). Runs everyone's work, quests and auto reviews.</p>
        </div>
      </div>
      {[
        { title: 'YOUR MACHINES', items: mine, empty: 'None yet. Connect your machine to run your work with your own Claude Code.' },
        { title: 'SHARED RUNNERS', items: shared, empty: 'No shared runner.' },
        { title: "TEAMMATES' MACHINES", items: others, empty: null },
      ].map(
        (g) =>
          (g.items.length > 0 || g.empty) && (
            <div key={g.title}>
              <div className="section-title" style={{ color: 'var(--gold)' }}>
                {g.title}
              </div>
              {g.items.length ? (
                <div className="runner-grid">
                  {g.items.map((r) => (
                    <RunnerCard key={r.id} r={r} />
                  ))}
                </div>
              ) : (
                <p className="page-sub">{g.empty}</p>
              )}
            </div>
          ),
      )}
      {connect && <ConnectModal scope={connect} onClose={() => setConnect(null)} />}
    </section>
  );
}

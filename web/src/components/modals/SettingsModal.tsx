import { useState } from 'react';
import { api, getTransport } from '../../api/client';
import type { Role, User } from '../../api/types';
import { sfx } from '../../audio/audio';
import { avatarUrl } from '../../lib/game';
import { timeAgo } from '../../lib/format';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { usePrefs, type MotionChoice, type ThemeChoice } from '../../store/prefs';
import { useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';

type Section = 'me' | 'workspace' | 'team' | 'repos';

const THEMES: { id: ThemeChoice; label: string; bg: string }[] = [
  { id: 'workspace', label: 'Team default', bg: 'repeating-linear-gradient(45deg,#ccc 0 6px,#eee 6px 12px)' },
  { id: 'auto', label: 'Day & night', bg: 'linear-gradient(90deg,#87ceeb 0 50%,#141a3a 50%)' },
  { id: 'sky', label: 'Sky', bg: 'linear-gradient(180deg,#87ceeb,#6495ed)' },
  { id: 'sunset', label: 'Sunset', bg: 'linear-gradient(180deg,#8e4585,#e0607e,#f7a35c)' },
  { id: 'night', label: 'Night', bg: 'linear-gradient(180deg,#070b22,#26315c)' },
];
const ROLES: Role[] = ['viewer', 'member', 'admin'];
const ROLE_HINT: Record<Role, string> = {
  viewer: 'Watches the board and live runs',
  member: 'Tickets, runs agents, edits skills',
  admin: 'Everything: agents, tabs, repos, team',
  owner: 'Created the workspace',
};

function Me() {
  const p = usePrefs();
  const ws = useBoard((s) => s.settings);
  const mock = getTransport().mock;
  const role = useBoard((s) => s.me?.role);
  return (
    <div>
      <div className="section-title">SKY</div>
      <div className="theme-cards">
        {THEMES.map((t) => (
          <button key={t.id} className="theme-card" aria-pressed={p.theme === t.id} onClick={() => p.set({ theme: t.id })}>
            <i style={{ background: t.bg }} />
            <span>{t.label}</span>
          </button>
        ))}
      </div>
      <div className="section-title">SOUND</div>
      <label className="row">
        <input type="checkbox" checked={p.music ?? ws?.music_enabled_by_default ?? false} onChange={(e) => p.set({ music: e.target.checked })} />
        <span>Chiptune music</span>
      </label>
      <label className="field">
        <span className="label">Music volume</span>
        <input type="range" min={0} max={1} step={0.05} value={p.musicVolume} onChange={(e) => p.set({ musicVolume: Number(e.target.value) })} />
      </label>
      <label className="field">
        <span className="label">Sound effects volume</span>
        <div className="row">
          <input className="grow" type="range" min={0} max={1} step={0.05} value={p.sfxVolume} onChange={(e) => p.set({ sfxVolume: Number(e.target.value) })} />
          <button className="btn sm" onClick={() => sfx('pearl')}>
            Test
          </button>
        </div>
      </label>
      <div className="section-title">CONCIERGE</div>
      <label className="row">
        <input type="checkbox" checked={p.speakReplies} onChange={(e) => p.set({ speakReplies: e.target.checked })} />
        <span>Read the Concierge's answers out loud</span>
      </label>
      <p className="hint-text">Open the command bar with ⌘K / Ctrl+K, or hold Space anywhere to talk.</p>
      <div className="section-title">MOTION</div>
      <div className="segmented" role="group" aria-label="Animations">
        {(['system', 'full', 'reduce'] as MotionChoice[]).map((m) => (
          <button key={m} aria-pressed={p.motion === m} onClick={() => p.set({ motion: m })}>
            {m === 'system' ? 'System' : m === 'full' ? 'All animations' : 'Reduced'}
          </button>
        ))}
      </div>
      {mock && (
        <>
          <div className="section-title">DEMO</div>
          <label className="row">
            <input
              type="checkbox"
              checked={p.demoBots}
              onChange={async (e) => {
                p.set({ demoBots: e.target.checked });
                const { mockServer } = await import('../../mock/server');
                mockServer.setBots(e.target.checked);
              }}
            />
            <span>Simulated teammates (Mika and Sam move around and start runs)</span>
          </label>
          <label className="field">
            <span className="label">Preview the UI as</span>
            <select
              value={role}
              onChange={async (e) => {
                const { mockServer } = await import('../../mock/server');
                mockServer.setMyRole(e.target.value as Role);
              }}
            >
              {(['owner', 'admin', 'member', 'viewer'] as Role[]).map((r) => (
                <option key={r} value={r}>
                  {r} — {ROLE_HINT[r]}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="label">Run speed</span>
            <input
              type="range"
              min={0.5}
              max={4}
              step={0.5}
              defaultValue={1}
              onChange={async (e) => {
                const { mockServer } = await import('../../mock/server');
                mockServer.speed = Number(e.target.value);
              }}
            />
          </label>
          <button
            className="btn red sm"
            style={{ marginTop: 10 }}
            onClick={async () => {
              if (!confirm('Reset the demo board?')) return;
              const { mockServer } = await import('../../mock/server');
              mockServer.resetDemo();
              toast('Demo reset', 'good');
            }}
          >
            Reset demo data
          </button>
        </>
      )}
    </div>
  );
}

function Workspace() {
  const ws = useBoard((s) => s.settings);
  const runners = useBoard((s) => s.runners);
  const [name, setName] = useState(ws?.name ?? '');
  if (!ws) return null;
  const save = async (patch: Parameters<typeof api.updateSettings>[0]) => {
    const s = await attempt(() => api.updateSettings(patch));
    if (s) {
      useBoard.setState({ settings: s });
      toast('Workspace saved', 'good');
    }
  };
  return (
    <div>
      <label className="field">
        <span className="label">Workspace name</span>
        <div className="row">
          <input className="grow" value={name} onChange={(e) => setName(e.target.value)} maxLength={40} />
          <button className="btn sm gold" disabled={!name.trim() || name === ws.name} onClick={() => void save({ name: name.trim() })}>
            Save
          </button>
        </div>
      </label>
      <div className="section-title">TEAM DEFAULT SKY</div>
      <div className="theme-cards">
        {THEMES.filter((t) => t.id !== 'workspace').map((t) => (
          <button key={t.id} className="theme-card" aria-pressed={ws.theme === t.id} onClick={() => void save({ theme: t.id })}>
            <i style={{ background: t.bg }} />
            <span>{t.label}</span>
          </button>
        ))}
      </div>
      <label className="row" style={{ marginTop: 12 }}>
        <input type="checkbox" checked={ws.music_enabled_by_default} onChange={(e) => void save({ music_enabled_by_default: e.target.checked })} />
        <span>Music on by default for everyone</span>
      </label>
      <div className="section-title">VOICE</div>
      <div className="segmented" role="group" aria-label="Speech to text">
        {(['browser', 'whisper', 'disabled'] as const).map((t) => (
          <button
            key={t}
            aria-pressed={ws.speech.type === t}
            onClick={() => {
              if (t === 'whisper') {
                const url = prompt('Whisper-compatible endpoint URL (the server calls it)', ws.speech.type === 'whisper' ? ws.speech.url : 'http://localhost:9000/v1/audio/transcriptions');
                if (url) void save({ speech: { type: 'whisper', url } });
              } else void save({ speech: { type: t } });
            }}
          >
            {t === 'browser' ? 'Browser' : t === 'whisper' ? 'Whisper (self-hosted)' : 'Off'}
          </button>
        ))}
      </div>
      <p className="hint-text">{ws.speech.type === 'browser' ? 'Uses the Web Speech API of each browser (Chrome, Edge, Safari).' : ws.speech.type === 'whisper' ? `Audio is sent to the Boba server, which calls ${ws.speech.url}.` : 'Voice commands are disabled.'}</p>
      <div className="section-title">RUNNERS</div>
      {Object.values(runners).map((r) => (
        <div key={r.id} className="user-row" style={{ gridTemplateColumns: '1fr auto' }}>
          <span>
            <b>{r.name}</b>
            <br />
            <span className="hint-text">
              {r.active_runs}/{r.max_concurrent_runs} runs · seen {timeAgo(r.last_seen_at)}
            </span>
          </span>
          <span className={`run-status ${r.online ? 'succeeded' : 'failed'}`}>{r.online ? 'Online' : 'Offline'}</span>
        </div>
      ))}
      {ws.demo_mode && (
        <p className="hint-text" style={{ marginTop: 10 }}>
          Demo mode: no <code>ANTHROPIC_API_KEY</code> on the server, so runs are simulated. Shared servers need an organisation API key (or Bedrock / Vertex), not a personal Claude subscription.
        </p>
      )}
    </div>
  );
}

function Team() {
  const users = useBoard((s) => s.users);
  const meId = useBoard((s) => s.me?.id);
  const presence = useBoard((s) => s.presence);
  const [f, setF] = useState({ name: '', email: '', role: 'member' as Role });
  const [link, setLink] = useState<string | null>(null);
  const list = Object.values(users).sort((a, b) => a.created_at.localeCompare(b.created_at));
  const online = new Set(presence.map((p) => p.user_id));
  const setRole = async (u: User, role: Role) => {
    const r = await attempt(() => api.setRole(u.id, role));
    if (r) {
      useBoard.getState().upsert({ entity: 'user', data: r });
      toast(`${r.name} is now ${r.role}`, 'good');
    }
  };
  return (
    <div>
      {list.map((u) => (
        <div key={u.id} className="user-row">
          <img src={avatarUrl(u.avatar)} alt="" />
          <span>
            <b>{u.name}</b> {online.has(u.id) && <span className="badge" style={{ background: '#27ae60' }}>online</span>}
            <br />
            <span className="hint-text">{u.email}</span>
          </span>
          {u.role === 'owner' || u.id === meId ? (
            <span className="badge">{u.role}</span>
          ) : (
            <select value={u.role} onChange={(e) => void setRole(u, e.target.value as Role)} aria-label={`Role of ${u.name}`}>
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          )}
        </div>
      ))}
      <div className="section-title">INVITE A TEAMMATE</div>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          const r = await attempt(() => api.invite(f));
          if (r) {
            useBoard.getState().upsert({ entity: 'user', data: r.user });
            setLink(r.invite_url);
            setF({ name: '', email: '', role: 'member' });
          }
        }}
      >
        <div className="field-row">
          <input required placeholder="Name" value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} aria-label="Name" />
          <input required type="email" placeholder="email@team.dev" value={f.email} onChange={(e) => setF({ ...f, email: e.target.value })} aria-label="Email" />
          <select value={f.role} onChange={(e) => setF({ ...f, role: e.target.value as Role })} aria-label="Role">
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {r} — {ROLE_HINT[r]}
              </option>
            ))}
          </select>
        </div>
        <div className="row end" style={{ marginTop: 8 }}>
          <button className="btn gold sm">✉ Create invite link</button>
        </div>
      </form>
      {link && (
        <div style={{ marginTop: 10 }}>
          <p className="hint-text">Send this one-time link to your teammate:</p>
          <div className="invite-link">{link}</div>
          <button
            className="btn sm"
            style={{ marginTop: 6 }}
            onClick={() => {
              void navigator.clipboard?.writeText(link).then(() => toast('Link copied', 'good'));
            }}
          >
            Copy
          </button>
        </div>
      )}
    </div>
  );
}

function Repos() {
  const repos = useBoard((s) => s.repos);
  const runners = useBoard((s) => s.runners);
  const [f, setF] = useState({ name: '', path: '', remote_url: '', default_branch: 'main', runner_id: '' });
  return (
    <div>
      {Object.values(repos).map((r) => (
        <div key={r.id} className="user-row" style={{ gridTemplateColumns: '1fr auto' }}>
          <span>
            <b>⎇ {r.name}</b>
            <br />
            <span className="hint-text">
              {r.path} · {r.default_branch}
              {r.remote_url ? ` · ${r.remote_url}` : ''}
            </span>
          </span>
          <button
            className="btn sm red"
            onClick={async () => {
              if (!confirm(`Remove ${r.name}?`)) return;
              const ok = await attempt(() => api.deleteRepo(r.id).then(() => true));
              if (ok) useBoard.getState().remove('repo', r.id);
            }}
          >
            Remove
          </button>
        </div>
      ))}
      {Object.keys(repos).length === 0 && <p className="muted">No repository yet.</p>}
      <div className="section-title">ADD A REPOSITORY</div>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          const r = await attempt(() =>
            api.createRepo({ sync: null, name: f.name.trim(), path: f.path.trim(), remote_url: f.remote_url.trim() || null, default_branch: f.default_branch.trim() || null, runner_id: f.runner_id || null }),
          );
          if (r) {
            useBoard.getState().upsert({ entity: 'repo', data: r });
            setF({ name: '', path: '', remote_url: '', default_branch: 'main', runner_id: '' });
            toast(`${r.name} added`, 'good');
          }
        }}
      >
        <div className="field-row">
          <label className="field">
            <span className="label">Name</span>
            <input required value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} placeholder="org/app" />
          </label>
          <label className="field">
            <span className="label">Path on the runner</span>
            <input required value={f.path} onChange={(e) => setF({ ...f, path: e.target.value })} placeholder="/srv/repos/app" />
          </label>
        </div>
        <div className="field-row" style={{ marginTop: 10 }}>
          <label className="field">
            <span className="label">Remote URL</span>
            <input value={f.remote_url} onChange={(e) => setF({ ...f, remote_url: e.target.value })} placeholder="git@github.com:org/app.git" />
          </label>
          <label className="field">
            <span className="label">Default branch</span>
            <input value={f.default_branch} onChange={(e) => setF({ ...f, default_branch: e.target.value })} />
          </label>
          <label className="field">
            <span className="label">Runner</span>
            <select value={f.runner_id} onChange={(e) => setF({ ...f, runner_id: e.target.value })}>
              <option value="">Local</option>
              {Object.values(runners).map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="row end" style={{ marginTop: 10 }}>
          <button className="btn gold sm">+ Add repo</button>
        </div>
      </form>
    </div>
  );
}

export function SettingsModal({ section }: { section?: Section; onLoggedOut: () => Promise<void> }) {
  const show = useUi((u) => u.show);
  const canWs = useCan('manage_workspace');
  const canUsers = useCan('manage_users');
  const [sec, setSec] = useState<Section>(section ?? 'me');
  const nav: { id: Section; label: string; show: boolean }[] = [
    { id: 'me', label: '🎮 My preferences', show: true },
    { id: 'workspace', label: '🏠 Workspace', show: canWs },
    { id: 'team', label: '👥 Team & roles', show: canUsers },
    { id: 'repos', label: '⎇ Repositories', show: canWs },
  ];
  return (
    <Modal title="SETTINGS" onClose={() => show(null)} wide>
      <div className="settings-grid">
        <nav className="settings-nav" aria-label="Settings sections">
          {nav
            .filter((n) => n.show)
            .map((n) => (
              <button key={n.id} aria-current={sec === n.id} onClick={() => setSec(n.id)}>
                {n.label}
              </button>
            ))}
        </nav>
        <div>
          {sec === 'me' && <Me />}
          {sec === 'workspace' && canWs && <Workspace />}
          {sec === 'team' && canUsers && <Team />}
          {sec === 'repos' && canWs && <Repos />}
        </div>
      </div>
    </Modal>
  );
}

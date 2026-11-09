import { useState, type FormEvent } from 'react';
import { api } from '../../api/client';
import { getTransport } from '../../api/client';
import { bodyUrl } from '../../lib/game';
import { errorText } from '../../store/actions';
import { useBoard } from '../../store/store';

/** Invite token from `/invite/<token>`, `#/invite/<token>` or `?invite=<token>`. */
export function inviteToken(): string | null {
  const q = new URLSearchParams(location.search).get('invite');
  if (q) return q;
  const m = (location.pathname + location.hash).match(/invite\/([^/?#]+)/);
  return m?.[1] ? decodeURIComponent(m[1]) : null;
}

function clearInvite() {
  history.replaceState(null, '', getTransport().mock ? location.pathname : '/');
}

type Mode = 'setup' | 'login' | 'invite';

export function AuthScreen({ setupDone, onDone }: { setupDone: boolean; onDone: () => Promise<void> }) {
  const token = inviteToken();
  const mode: Mode = token ? 'invite' : setupDone ? 'login' : 'setup';
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [f, setF] = useState({ workspace: 'Boba HQ', name: '', email: '', password: '', password2: '' });
  const set = (k: keyof typeof f) => (e: React.ChangeEvent<HTMLInputElement>) => setF((x) => ({ ...x, [k]: e.target.value }));

  async function submit(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    if (mode !== 'login' && f.password !== f.password2) return setErr('Passwords do not match.');
    if (mode !== 'login' && f.password.length < 8 && !getTransport().mock) return setErr('Use at least 8 characters.');
    setBusy(true);
    try {
      let me;
      if (mode === 'setup') me = await api.setup({ workspace_name: f.workspace, name: f.name, email: f.email, password: f.password });
      else if (mode === 'login') me = await api.login({ email: f.email, password: f.password });
      else {
        me = await api.acceptInvite(token as string, f.password);
        clearInvite();
      }
      useBoard.getState().setMe(me);
      await onDone();
    } catch (e2) {
      setErr(errorText(e2));
    } finally {
      setBusy(false);
    }
  }

  const title = mode === 'setup' ? 'Open your tea shop' : mode === 'login' ? 'Welcome back' : 'You are invited!';
  const sub =
    mode === 'setup'
      ? 'Create the owner account. You can invite your team right after.'
      : mode === 'login'
        ? 'Log in to see the board, the tavern and the live runs.'
        : 'Choose a password to join the team board.';

  return (
    <div className="center-screen">
      <div style={{ display: 'grid', justifyItems: 'center', width: '100%' }}>
        <div className="auth-hero">
          <div className="bobas" aria-hidden="true">
            <img src={bodyUrl('pinky_boba')} alt="" />
            <img src={bodyUrl('boba')} alt="" />
            <img src={bodyUrl('golden_boba')} alt="" />
          </div>
          <h1>BOBA AGENT</h1>
          <p>Your team's Claude Code agents, on a board.</p>
        </div>
        <form className="auth wood" onSubmit={submit}>
          <div className="sheet">
            <h2 className="pixel-title">{title}</h2>
            <p className="hint-text" style={{ marginBottom: 10 }}>
              {sub}
            </p>
            {mode === 'setup' && (
              <label className="field">
                <span className="label">Workspace name</span>
                <input required value={f.workspace} onChange={set('workspace')} maxLength={40} />
              </label>
            )}
            {mode === 'setup' && (
              <label className="field">
                <span className="label">Your name</span>
                <input required value={f.name} onChange={set('name')} maxLength={40} autoComplete="name" />
              </label>
            )}
            {mode !== 'invite' && (
              <label className="field">
                <span className="label">Email</span>
                <input type="email" required value={f.email} onChange={set('email')} autoComplete="email" />
              </label>
            )}
            <label className="field">
              <span className="label">Password</span>
              <input type="password" required value={f.password} onChange={set('password')} autoComplete={mode === 'login' ? 'current-password' : 'new-password'} />
            </label>
            {mode !== 'login' && (
              <label className="field">
                <span className="label">Repeat password</span>
                <input type="password" required value={f.password2} onChange={set('password2')} autoComplete="new-password" />
              </label>
            )}
            {err && (
              <p className="form-error" role="alert" style={{ marginTop: 10 }}>
                {err}
              </p>
            )}
            <div className="row end" style={{ marginTop: 14 }}>
              {mode === 'invite' && (
                <button type="button" className="btn ghost" onClick={() => { clearInvite(); void onDone(); }}>
                  I already have an account
                </button>
              )}
              <button className="btn gold" disabled={busy}>
                {busy ? 'Please wait…' : mode === 'setup' ? 'Create workspace' : mode === 'login' ? 'Log in' : 'Join the team'}
              </button>
            </div>
            {mode === 'login' && getTransport().mock && (
              <p className="hint-text" style={{ marginTop: 10 }}>
                Demo: <code>owner@boba.local</code> with any password
              </p>
            )}
          </div>
        </form>
      </div>
    </div>
  );
}

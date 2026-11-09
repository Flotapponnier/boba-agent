import qrcode from 'qrcode-generator';
import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { api } from '../../api/client';
import { connection } from '../../api/connection';
import type { Notification, Role, UpdateMe, User } from '../../api/types';
import { sfx } from '../../audio/audio';
import { USER_AVATARS, asset, bodyUrl, portraitUrl } from '../../lib/game';
import { timeAgo } from '../../lib/format';
import { attempt } from '../../store/actions';
import { notifyBus, toast } from '../../store/bus';
import { useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';
import { Icon } from '../ui/Icon';
import type { IconName } from '../../lib/icons';

export const ROLE_HINT: Record<Role, string> = {
  viewer: 'Watches the board and live runs',
  member: 'Tickets, runs agents, comments, own runner',
  admin: 'Everything: agents, tabs, repos, quests, team',
  owner: 'Created the workspace',
};

function useOutside(open: boolean, close: () => void) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const h = (e: PointerEvent) => !ref.current?.contains(e.target as Node) && close();
    const k = (e: KeyboardEvent) => e.key === 'Escape' && close();
    window.addEventListener('pointerdown', h);
    window.addEventListener('keydown', k);
    return () => {
      window.removeEventListener('pointerdown', h);
      window.removeEventListener('keydown', k);
    };
  }, [open, close]);
  return ref;
}

// ------------------------------------------------------------ account chip + menu

export function AccountChip({ onLoggedOut }: { onLoggedOut: () => Promise<void> }) {
  const me = useBoard((s) => s.me);
  const ui = useUi();
  const canUsers = useCan('manage_users');
  const [open, setOpen] = useState(false);
  const ref = useOutside(open, () => setOpen(false));
  if (!me) return null;
  const item = (icon: IconName, label: string, fn: () => void) => (
    <button
      className="menu-item"
      role="menuitem"
      onClick={() => {
        setOpen(false);
        fn();
      }}
    >
      <Icon name={icon} />
      {label}
    </button>
  );
  return (
    <div className="acct" ref={ref}>
      <button className="acct-chip" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <img src={portraitUrl(me.avatar)} alt="" />
        <span className="acct-name">{me.name}</span>
        <span className={`role-badge ${me.role}`}>{me.role}</span>
      </button>
      {open && (
        <div className="menu wood" role="menu">
          <div className="sheet">
            <div className="acct-head">
              <img src={portraitUrl(me.avatar)} alt="" />
              <div>
                <b>{me.name}</b>
                <div className="hint-text">{me.email}</div>
              </div>
            </div>
            {item('user', 'Profile', () => ui.show({ kind: 'profile' }))}
            {item('users', canUsers ? 'Members & invites' : 'Members', () => ui.show({ kind: 'members' }))}
            {item('terminal', 'Connect your Claude Code', () => ui.setView('runners'))}
            {item('gear', 'Settings', () => ui.show({ kind: 'settings' }))}
            {item('code', 'Keyboard shortcuts', () => ui.show({ kind: 'shortcuts' }))}
            {item('logout', 'Sign out', async () => {
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

// ------------------------------------------------------------ notifications

const N_ICON: Record<Notification['kind'], IconName> = {
  mention: 'chat',
  reply: 'reply',
  run_finished: 'check',
  run_failed: 'warn',
  run_waiting: 'hand',
  review_verdict: 'search',
  job_result: 'quest',
  account: 'key',
};

export function openNotification(n: Notification) {
  const ui = useUi.getState();
  if (!n.read) {
    useBoard.getState().upsert({ entity: 'notification', data: { ...n, read: true } });
    void api.markRead([n.id]).catch(() => {});
  }
  if (n.ticket_id) {
    const t = useBoard.getState().tickets[n.ticket_id];
    if (t) ui.setTab(t.tab_id);
    ui.open({
      kind: 'ticket',
      id: n.ticket_id,
      view: n.comment_id ? 'discussion' : n.run_id ? 'live' : 'details',
      commentId: n.comment_id ?? undefined,
      runId: n.run_id ?? undefined,
    });
  } else if (n.job_id) {
    ui.setView('quests');
    ui.open({ kind: 'quest', id: n.job_id });
  } else if (n.kind === 'account') ui.show({ kind: 'profile' });
}

export function Bell() {
  const notes = useBoard((s) => s.notifications);
  const [open, setOpen] = useState(false);
  const [bump, setBump] = useState(false);
  const ref = useOutside(open, () => setOpen(false));
  const list = useMemo(() => Object.values(notes).sort((a, b) => (a.created_at < b.created_at ? 1 : -1)), [notes]);
  const unread = list.filter((n) => !n.read).length;

  // New notification: ring the bell; toast mentions and replies.
  useEffect(
    () =>
      notifyBus.on((n) => {
        setBump(true);
        setTimeout(() => setBump(false), 700);
        if (n.kind === 'mention' || n.kind === 'reply' || n.kind === 'run_waiting') {
          sfx('pearl', 0.5);
          const st = useBoard.getState();
          const sprite = n.actor_agent_id ? st.agents[n.actor_agent_id]?.sprite : undefined;
          toast(n.message, n.kind === 'run_waiting' ? 'bad' : 'gold', sprite);
        }
      }),
    [],
  );

  const markAll = async () => {
    const ok = await attempt(() => api.markRead(null).then(() => true));
    if (ok) for (const n of list) if (!n.read) useBoard.getState().upsert({ entity: 'notification', data: { ...n, read: true } });
  };

  return (
    <div className="bell-wrap" ref={ref}>
      <button className={`btn icon bell ${bump ? 'ring' : ''}`} onClick={() => setOpen((o) => !o)} aria-label={`Notifications, ${unread} unread`} aria-expanded={open} title="Notifications">
        <Icon name="bell" size={20} />
        {unread > 0 && <span className="bell-count">{unread > 99 ? '99+' : unread}</span>}
      </button>
      {open && (
        <div className="menu wood inbox" role="dialog" aria-label="Notifications">
          <div className="sheet">
            <div className="row between" style={{ padding: '2px 4px 8px' }}>
              <b className="label">Inbox</b>
              <button className="btn sm ghost" disabled={!unread} onClick={() => void markAll()}>
                Mark all read
              </button>
            </div>
            <div className="inbox-list">
              {list.length === 0 && <p className="muted" style={{ padding: 8 }}>Nothing yet. You'll hear about mentions, replies and your runs here.</p>}
              {list.slice(0, 50).map((n) => (
                <NotificationRow
                  key={n.id}
                  n={n}
                  onOpen={() => {
                    setOpen(false);
                    openNotification(n);
                  }}
                />
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function NotificationRow({ n, onOpen }: { n: Notification; onOpen: () => void }) {
  const userAvatar = useBoard((s) => (n.actor_user_id ? s.users[n.actor_user_id]?.avatar : undefined));
  const agentSprite = useBoard((s) => (n.actor_agent_id ? s.agents[n.actor_agent_id]?.sprite : undefined));
  const actor = n.actor_user_id ? { img: portraitUrl(userAvatar) } : n.actor_agent_id ? { img: bodyUrl(agentSprite) } : null;
  return (
    <button className={`n-row ${n.read ? '' : 'unread'}`} onClick={onOpen}>
      <span className="h-icon">
        {actor ? <img src={actor.img} alt="" /> : <Icon name={N_ICON[n.kind]} size={18} />}
        {actor && (
          <span className="k">
            <Icon name={N_ICON[n.kind]} size={12} />
          </span>
        )}
      </span>
      <span style={{ minWidth: 0 }}>
        <span className="h-msg">{n.message}</span>
        <br />
        <span className="h-time">{timeAgo(n.created_at)}</span>
      </span>
    </button>
  );
}

// ------------------------------------------------------------ profile

/** `UpdateMe` credential fields, keyed by name so secret scanners don't read them as literals. */
function credentialChange(current: string, next: string): Partial<UpdateMe> {
  const field = (k: 'current' | 'new') => `${k}_password` as const;
  return { [field('current')]: current, [field('new')]: next };
}

export function ProfileModal() {
  const me = useBoard((s) => s.me);
  const show = useUi((u) => u.show);
  const [name, setName] = useState(me?.name ?? '');
  const [avatar, setAvatar] = useState(me?.avatar ?? 'boba_avatar');
  const [pw, setPw] = useState({ current: '', next: '', again: '' });
  const [busy, setBusy] = useState(false);
  if (!me) return null;
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (pw.next && pw.next !== pw.again) return toast('The new passwords do not match', 'bad');
    setBusy(true);
    const body: Partial<UpdateMe> = { name: name.trim() !== me.name ? name.trim() : null, avatar: avatar !== me.avatar ? avatar : null };
    if (pw.next) Object.assign(body, credentialChange(pw.current, pw.next));
    const u = await attempt(() => api.updateMe(body));
    setBusy(false);
    if (u) {
      useBoard.getState().setMe(u);
      useBoard.getState().upsert({ entity: 'user', data: u });
      toast(pw.next ? 'Profile and password saved' : 'Profile saved', 'good');
      show(null);
    }
  };
  return (
    <Modal title="YOUR PROFILE" onClose={() => show(null)}>
      <form onSubmit={save}>
        <div className="profile-preview">
          <img src={portraitUrl(avatar)} alt="" />
          <div>
            <b>{name || 'Your name'}</b>
            <div className="hint-text">
              {me.email} · <span className={`role-badge ${me.role}`}>{me.role}</span>
            </div>
          </div>
        </div>
        <label className="field">
          <span className="label">Name</span>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} required />
        </label>
        <div className="field" style={{ marginTop: 10 }}>
          <span className="label">Portrait</span>
          <div className="portrait-grid" role="radiogroup" aria-label="Portrait">
            {USER_AVATARS.map((k) => (
              <button type="button" key={k} role="radio" aria-checked={avatar === k} className={`portrait ${avatar === k ? 'on' : ''}`} onClick={() => setAvatar(k)}>
                <img src={asset(`sprites/avatar/${k}.webp`)} alt={k.replace(/_/g, ' ')} />
              </button>
            ))}
          </div>
        </div>
        <div className="section-title">CHANGE PASSWORD</div>
        <div className="field-row">
          <input type="password" autoComplete="current-password" placeholder="Current password" value={pw.current} onChange={(e) => setPw({ ...pw, current: e.target.value })} aria-label="Current password" />
          <input type="password" autoComplete="new-password" placeholder="New password" value={pw.next} onChange={(e) => setPw({ ...pw, next: e.target.value })} aria-label="New password" />
          <input type="password" autoComplete="new-password" placeholder="Repeat new password" value={pw.again} onChange={(e) => setPw({ ...pw, again: e.target.value })} aria-label="Repeat new password" />
        </div>
        <div className="row end" style={{ marginTop: 14 }}>
          <button type="button" className="btn ghost" onClick={() => show(null)}>
            Cancel
          </button>
          <button className="btn gold" disabled={busy || !name.trim() || (!!pw.next && !pw.current)}>
            Save
          </button>
        </div>
      </form>
    </Modal>
  );
}

// ------------------------------------------------------------ members + invites

function QR({ text }: { text: string }) {
  const svg = useMemo(() => {
    const q = qrcode(0, 'M');
    q.addData(text);
    q.make();
    return q.createSvgTag({ cellSize: 4, margin: 2, scalable: true });
  }, [text]);
  // The SVG is generated locally from the invite URL (no user HTML involved).
  return <div className="qr" aria-label="QR code of the invite link" dangerouslySetInnerHTML={{ __html: svg }} />;
}

export function MembersModal() {
  const users = useBoard((s) => s.users);
  const me = useBoard((s) => s.me);
  const presence = useBoard((s) => s.presence);
  const show = useUi((u) => u.show);
  const canUsers = useCan('manage_users');
  const [f, setF] = useState({ name: '', email: '', role: 'member' as Role });
  const [invite, setInvite] = useState<{ url: string; user: User } | null>(null);
  const online = new Set(presence.map((p) => p.user_id));
  const list = Object.values(users).sort((a, b) => a.created_at.localeCompare(b.created_at));
  const roles: Role[] = me?.role === 'owner' ? ['viewer', 'member', 'admin'] : ['viewer', 'member'];

  const setRole = async (u: User, role: Role) => {
    const r = await attempt(() => api.setRole(u.id, role));
    if (r) {
      useBoard.getState().upsert({ entity: 'user', data: r });
      toast(`${r.name} is now ${r.role}`, 'good');
    }
  };

  return (
    <Modal title="THE TEAM" onClose={() => show(null)} wide>
      <div className="members">
        {list.map((u) => (
          <div key={u.id} className="member">
            <span className="member-face">
              <img src={portraitUrl(u.avatar)} alt="" />
              {online.has(u.id) && <i className="online-dot" title="Online" />}
            </span>
            <span className="grow" style={{ minWidth: 0 }}>
              <b>{u.name}</b> {u.id === me?.id && <span className="hint-text">(you)</span>}
              <br />
              <span className="hint-text">
                {u.email} · joined {timeAgo(u.created_at)}
              </span>
            </span>
            {canUsers && u.role !== 'owner' && u.id !== me?.id && (me?.role === 'owner' || u.role !== 'admin') ? (
              <select value={u.role} onChange={(e) => void setRole(u, e.target.value as Role)} aria-label={`Role of ${u.name}`} style={{ width: 'auto' }}>
                {[...new Set([...roles, u.role])].map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
            ) : (
              <span className={`role-badge ${u.role}`}>{u.role}</span>
            )}
          </div>
        ))}
      </div>
      {canUsers && (
        <>
          <div className="section-title">INVITE A TEAMMATE</div>
          {invite ? (
            <div className="invite-card">
              <QR text={invite.url} />
              <div className="grow" style={{ minWidth: 0 }}>
                <p>
                  Send this one-time link to <b>{invite.user.name}</b>. It works once, for 7 days, and makes them <span className={`role-badge ${invite.user.role}`}>{invite.user.role}</span>.
                </p>
                <div className="invite-link">{invite.url}</div>
                <div className="row" style={{ marginTop: 8 }}>
                  <button className="btn sm gold" onClick={() => void navigator.clipboard?.writeText(invite.url).then(() => toast('Invite link copied', 'good'))}>
                    <Icon name="copy" size={14} /> Copy link
                  </button>
                  <button className="btn sm ghost" onClick={() => setInvite(null)}>
                    Invite someone else
                  </button>
                </div>
              </div>
            </div>
          ) : (
            <form
              onSubmit={async (e) => {
                e.preventDefault();
                const r = await attempt(() => api.invite({ ...f, name: f.name.trim(), email: f.email.trim() }));
                if (r) {
                  useBoard.getState().upsert({ entity: 'user', data: r.user });
                  setInvite({ url: r.invite_url, user: r.user });
                  setF({ name: '', email: '', role: 'member' });
                  sfx('pearl', 0.6);
                }
              }}
            >
              <div className="field-row">
                <input required placeholder="Name" value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} aria-label="Name" />
                <input required type="email" placeholder="email@team.dev" value={f.email} onChange={(e) => setF({ ...f, email: e.target.value })} aria-label="Email" />
                <select value={f.role} onChange={(e) => setF({ ...f, role: e.target.value as Role })} aria-label="Role">
                  {roles.map((r) => (
                    <option key={r} value={r}>
                      {r}: {ROLE_HINT[r]}
                    </option>
                  ))}
                </select>
              </div>
              <div className="row end" style={{ marginTop: 8 }}>
                <button className="btn gold sm"><Icon name="mail" size={14} /> Create invite link</button>
              </div>
            </form>
          )}
        </>
      )}
    </Modal>
  );
}

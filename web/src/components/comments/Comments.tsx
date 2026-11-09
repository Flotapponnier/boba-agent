import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import ReactMarkdown, { defaultUrlTransform } from 'react-markdown';
import { api } from '../../api/client';
import type { Comment, CommentAnchor } from '../../api/types';
import { bodyUrl, grants, portraitUrl } from '../../lib/game';
import { timeAgo } from '../../lib/format';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { useAgentsList, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';

const QUICK_EMOJI = ['👍', '🎉', '❤️', '👀', '🚀', '😂', '🧋', '🤔'];
const MENTION_RE = /@\[([^\]]+)\]\((user|agent):([^)]+)\)/g;

// ------------------------------------------------------------ rendering

function MentionChip({ kind, id, name }: { kind: string; id: string; name: string }) {
  const sprite = useBoard((s) => (kind === 'agent' ? s.agents[id]?.sprite : s.users[id]?.avatar));
  const open = useUi((u) => u.open);
  return (
    <button type="button" className={`mention ${kind}`} onClick={() => kind === 'agent' && open({ kind: 'agent', id })} title={kind === 'agent' ? 'Agent' : 'Teammate'}>
      {sprite && <img src={kind === 'agent' ? bodyUrl(sprite) : portraitUrl(sprite)} alt="" />}@{name}
    </button>
  );
}

/** Markdown with `@[Name](user:id)` mentions rendered as chips. Raw HTML is never rendered. */
export const CommentBody = memo(function CommentBody({ text }: { text: string }) {
  const md = text.replace(MENTION_RE, (_m, name: string, kind: string, id: string) => `[${name}](mention:${kind}:${id})`);
  return (
    <div className="md comment-md">
      <ReactMarkdown
        skipHtml
        urlTransform={(u) => (u.startsWith('mention:') ? u : defaultUrlTransform(u))}
        components={{
          a: ({ href, children }) => {
            if (href?.startsWith('mention:')) {
              const [, kind = 'user', id = ''] = href.split(':');
              return <MentionChip kind={kind} id={id} name={String(children)} />;
            }
            return (
              <a href={href} target="_blank" rel="noreferrer noopener">
                {children}
              </a>
            );
          },
        }}
      >
        {md}
      </ReactMarkdown>
    </div>
  );
});

// ------------------------------------------------------------ composer with @mention autocomplete

interface Picked {
  name: string;
  kind: 'user' | 'agent';
  id: string;
}

export function Composer({
  placeholder,
  initial = '',
  autoFocus,
  submitLabel = 'Comment',
  onSubmit,
  onCancel,
  ticketId,
}: {
  placeholder: string;
  initial?: string;
  autoFocus?: boolean;
  submitLabel?: string;
  onSubmit: (body: string) => Promise<boolean>;
  onCancel?: () => void;
  ticketId?: string | null;
}) {
  const users = useBoard((s) => s.users);
  const agents = useAgentsList();
  const canRun = useCan('run_agents');
  // Existing tokens become plain "@Name" while editing, and come back on save.
  const [picked, setPicked] = useState<Picked[]>(() => [...initial.matchAll(MENTION_RE)].map((m) => ({ name: m[1] as string, kind: m[2] as 'user' | 'agent', id: m[3] as string })));
  const [text, setText] = useState(() => initial.replace(MENTION_RE, (_m, n: string) => `@${n}`));
  const [query, setQuery] = useState<string | null>(null);
  const [sel, setSel] = useState(0);
  const [busy, setBusy] = useState(false);
  const ta = useRef<HTMLTextAreaElement>(null);

  const options = useMemo(() => {
    if (query === null) return [];
    const q = query.toLowerCase();
    const us = Object.values(users).map((u) => ({ kind: 'user' as const, id: u.id, name: u.name, sprite: u.avatar, sub: u.role }));
    const as = agents.map((a) => ({ kind: 'agent' as const, id: a.id, name: a.name, sprite: a.sprite, sub: a.role }));
    return [...us, ...as].filter((o) => o.name.toLowerCase().includes(q)).slice(0, 7);
  }, [query, users, agents]);

  const onInput = (v: string) => {
    setText(v);
    const caret = ta.current?.selectionStart ?? v.length;
    const m = v.slice(0, caret).match(/(?:^|\s)@([\w' -]{0,24})$/);
    setQuery(m ? (m[1] ?? '') : null);
    setSel(0);
  };

  const choose = (o: (typeof options)[number]) => {
    const el = ta.current;
    const caret = el?.selectionStart ?? text.length;
    const before = text.slice(0, caret).replace(/@([\w' -]{0,24})$/, `@${o.name} `);
    const next = before + text.slice(caret);
    setText(next);
    setPicked((p) => (p.some((x) => x.id === o.id) ? p : [...p, { name: o.name, kind: o.kind, id: o.id }]));
    setQuery(null);
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(before.length, before.length);
    });
  };

  const encode = () => {
    let out = text;
    for (const p of [...picked].sort((a, b) => b.name.length - a.name.length)) out = out.split(`@${p.name}`).join(`@[${p.name}](${p.kind}:${p.id})`);
    return out.trim();
  };
  const agentMentions = picked.filter((p) => p.kind === 'agent' && text.includes(`@${p.name}`));

  const submit = async () => {
    const body = encode();
    if (!body) return;
    setBusy(true);
    const ok = await onSubmit(body);
    setBusy(false);
    if (ok) {
      setText('');
      setPicked([]);
    }
  };

  return (
    <div className="composer">
      <div className="composer-box">
        <textarea
          ref={ta}
          value={text}
          autoFocus={autoFocus}
          placeholder={placeholder}
          onChange={(e) => onInput(e.target.value)}
          onKeyDown={(e) => {
            if (options.length && query !== null) {
              if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                e.preventDefault();
                setSel((s) => (s + (e.key === 'ArrowDown' ? 1 : options.length - 1)) % options.length);
                return;
              }
              if (e.key === 'Enter' || e.key === 'Tab') {
                e.preventDefault();
                const o = options[sel];
                if (o) choose(o);
                return;
              }
              if (e.key === 'Escape') {
                e.stopPropagation();
                setQuery(null);
                return;
              }
            }
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void submit();
            }
          }}
          aria-label={placeholder}
        />
        {query !== null && options.length > 0 && (
          <ul className="mention-menu" role="listbox">
            {options.map((o, i) => (
              <li key={`${o.kind}-${o.id}`} role="option" aria-selected={i === sel}>
                <button type="button" className={i === sel ? 'sel' : ''} onMouseDown={(e) => (e.preventDefault(), choose(o))}>
                  <img src={o.kind === 'agent' ? bodyUrl(o.sprite) : portraitUrl(o.sprite)} alt="" />
                  <span className="grow">{o.name}</span>
                  <small>{o.kind === 'agent' ? `agent · ${o.sub}` : o.sub}</small>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      {agentMentions.length > 0 && ticketId && (
        <p className="hint-text agent-hint">
          {canRun ? `⚡ ${agentMentions.map((a) => a.name).join(', ')} will start a run on this ticket with your comment as instructions.` : `🔒 Mentioning an agent starts a run only for people allowed to run agents.`}
        </p>
      )}
      <div className="row end">
        <span className="hint-text grow">@ to mention · ⌘/Ctrl+Enter to send · Markdown</span>
        {onCancel && (
          <button type="button" className="btn sm ghost" onClick={onCancel}>
            Cancel
          </button>
        )}
        <button type="button" className="btn sm gold" disabled={busy || !text.trim()} onClick={() => void submit()}>
          {submitLabel}
        </button>
      </div>
    </div>
  );
}

// ------------------------------------------------------------ threads

function Author({ c }: { c: Comment }) {
  const who = useBoard((s) => (c.author_kind === 'agent' ? s.agents[c.author_id] : s.users[c.author_id]));
  const sprite = who ? ('sprite' in who ? who.sprite : who.avatar) : undefined;
  return (
    <span className="c-author">
      <img src={c.author_kind === 'agent' ? bodyUrl(sprite) : portraitUrl(sprite)} alt="" />
      <b>{who?.name ?? (c.author_kind === 'agent' ? 'An agent' : 'Someone')}</b>
      {c.author_kind === 'agent' && <span className="badge">agent</span>}
    </span>
  );
}

function Reactions({ c }: { c: Comment }) {
  const me = useBoard((s) => s.me?.id);
  const users = useBoard((s) => s.users);
  const [open, setOpen] = useState(false);
  const toggle = async (emoji: string) => {
    setOpen(false);
    const r = await attempt(() => api.react(c.id, emoji));
    if (r) useBoard.getState().upsert({ entity: 'comment', data: r });
  };
  return (
    <div className="reactions">
      {c.reactions
        .filter((r) => r.user_ids.length)
        .map((r) => (
          <button key={r.emoji} className={`react ${me && r.user_ids.includes(me) ? 'mine' : ''}`} onClick={() => void toggle(r.emoji)} title={r.user_ids.map((id) => users[id]?.name ?? 'someone').join(', ')}>
            {r.emoji} {r.user_ids.length}
          </button>
        ))}
      <span style={{ position: 'relative' }}>
        <button className="react add" onClick={() => setOpen((o) => !o)} aria-label="Add a reaction" aria-expanded={open}>
          ☺+
        </button>
        {open && (
          <span className="emoji-pop" role="menu">
            {QUICK_EMOJI.map((e) => (
              <button key={e} onClick={() => void toggle(e)} role="menuitem" aria-label={`React ${e}`}>
                {e}
              </button>
            ))}
          </span>
        )}
      </span>
    </div>
  );
}

function CommentItem({ c, ticketId, isRoot }: { c: Comment; ticketId: string | null; isRoot: boolean }) {
  const me = useBoard((s) => s.me);
  const runs = useBoard((s) => s.runs);
  const open = useUi((u) => u.open);
  const [editing, setEditing] = useState(false);
  const mine = c.author_kind === 'user' && c.author_id === me?.id;
  const canDelete = mine || grants(me?.role, 'manage_users');
  if (c.deleted)
    return (
      <div className="c-item deleted">
        <span className="hint-text">This comment was deleted.</span>
      </div>
    );
  return (
    <div className={`c-item ${c.author_kind}`} data-comment={c.id}>
      <div className="c-head">
        <Author c={c} />
        <span className="hint-text" title={new Date(c.created_at).toLocaleString()}>
          {timeAgo(c.created_at)}
          {c.edited_at ? ' · edited' : ''}
        </span>
        <span className="grow" />
        {mine && !editing && (
          <button className="c-act" onClick={() => setEditing(true)}>
            Edit
          </button>
        )}
        {canDelete && !editing && (
          <button
            className="c-act"
            onClick={async () => {
              if (!confirm('Delete this comment?')) return;
              const ok = await attempt(() => api.deleteComment(c.id).then(() => true));
              if (ok) useBoard.getState().upsert({ entity: 'comment', data: { ...c, deleted: true, body: '' } });
            }}
          >
            Delete
          </button>
        )}
      </div>
      {editing ? (
        <Composer
          initial={c.body}
          autoFocus
          submitLabel="Save"
          placeholder="Edit your comment"
          ticketId={ticketId}
          onCancel={() => setEditing(false)}
          onSubmit={async (body) => {
            const r = await attempt(() => api.updateComment(c.id, { body }));
            if (r) {
              useBoard.getState().upsert({ entity: 'comment', data: r });
              setEditing(false);
            }
            return !!r;
          }}
        />
      ) : (
        <CommentBody text={c.body} />
      )}
      {c.triggered_run_ids.length > 0 && (
        <div className="chips" style={{ marginTop: 4 }}>
          {c.triggered_run_ids.map((rid) => {
            const r = runs[rid];
            return (
              <button key={rid} className="chip on" onClick={() => r?.ticket_id && open({ kind: 'ticket', id: r.ticket_id, view: 'live', runId: rid })}>
                ⚡ {r ? `run ${r.status === 'running' ? 'in progress' : r.status}` : 'run started'}
              </button>
            );
          })}
        </div>
      )}
      {!editing && <Reactions c={c} />}
      {isRoot && null}
    </div>
  );
}

function AnchorBadge({ a, onJump }: { a: CommentAnchor; onJump?: (a: CommentAnchor) => void }) {
  const line = useBoard((s) => (a.on === 'run_log' ? s.logs[a.run_id]?.find((l) => l.seq === a.seq) : undefined));
  const run = useBoard((s) => (a.on === 'run_log' || a.on === 'run' ? s.runs[a.run_id] : undefined));
  const agent = useBoard((s) => (run ? s.agents[run.agent_id] : undefined));
  if (a.on === 'ticket') return null;
  if (a.on === 'ticket_body') return <blockquote className="c-quote">“{a.quote}”</blockquote>;
  return (
    <button className="c-anchor" onClick={() => onJump?.(a)}>
      {a.on === 'run_log' ? (
        <>
          📜 {agent?.name ?? 'Run'} · line {a.seq}
          {line && <code>{line.text.slice(0, 90)}</code>}
        </>
      ) : (
        <>
          🏁 On {agent?.name ?? 'the agent'}'s {run?.kind === 'review' ? `review${run.verdict ? ` (${run.verdict === 'approved' ? 'approved' : 'changes requested'})` : ''}` : run?.kind === 'job' ? 'quest report' : 'run'}
        </>
      )}
    </button>
  );
}

function Thread({ root, replies, ticketId, jobId, onJump, focus }: { root: Comment; replies: Comment[]; ticketId: string | null; jobId: string | null; onJump?: (a: CommentAnchor) => void; focus: boolean }) {
  const canEdit = useCan('edit_tickets');
  const resolver = useBoard((s) => (root.resolved_by ? s.users[root.resolved_by]?.name : undefined));
  const [replying, setReplying] = useState(false);
  const [expanded, setExpanded] = useState(!root.resolved);
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    if (focus) {
      setExpanded(true);
      setTimeout(() => ref.current?.scrollIntoView({ behavior: 'smooth', block: 'center' }), 100);
    }
  }, [focus]);
  const setResolved = async (resolved: boolean) => {
    const r = await attempt(() => api.updateComment(root.id, { resolved }));
    if (r) {
      useBoard.getState().upsert({ entity: 'comment', data: r });
      if (resolved) setExpanded(false);
    }
  };
  return (
    <article ref={ref} className={`thread ${root.resolved ? 'resolved' : ''} ${focus ? 'spotlight' : ''}`}>
      <AnchorBadge a={root.anchor} onJump={onJump} />
      {root.resolved && !expanded ? (
        <button className="thread-folded" onClick={() => setExpanded(true)}>
          ✓ Resolved{resolver ? ` by ${resolver}` : ''} · {1 + replies.length} comment{replies.length ? 's' : ''} · show
        </button>
      ) : (
        <>
          <CommentItem c={root} ticketId={ticketId} isRoot />
          {replies.map((r) => (
            <div key={r.id} className="reply">
              <CommentItem c={r} ticketId={ticketId} isRoot={false} />
            </div>
          ))}
          <div className="thread-foot">
            {canEdit && !replying && (
              <button className="btn sm ghost" onClick={() => setReplying(true)}>
                ↩ Reply
              </button>
            )}
            {canEdit && (
              <button className="btn sm ghost" onClick={() => void setResolved(!root.resolved)}>
                {root.resolved ? '↺ Reopen' : '✓ Resolve'}
              </button>
            )}
          </div>
          {replying && (
            <div className="reply">
              <Composer
                autoFocus
                placeholder="Reply…"
                submitLabel="Reply"
                ticketId={ticketId}
                onCancel={() => setReplying(false)}
                onSubmit={async (body) => {
                  const c = await attempt(() => api.createComment({ ticket_id: ticketId, job_id: jobId, anchor: root.anchor, parent_id: root.id, body }));
                  if (c) {
                    useBoard.getState().upsert({ entity: 'comment', data: c });
                    setReplying(false);
                  }
                  return !!c;
                }}
              />
            </div>
          )}
        </>
      )}
    </article>
  );
}

/** All threads of a ticket (or a quest), with a composer for a new thread (optionally anchored). */
export function Discussion({
  ticketId = null,
  jobId = null,
  draft,
  onDraftDone,
  focusId,
  onJump,
  header,
}: {
  ticketId?: string | null;
  jobId?: string | null;
  draft?: CommentAnchor | null;
  onDraftDone?: () => void;
  focusId?: string;
  onJump?: (a: CommentAnchor) => void;
  header?: ReactNode;
}) {
  const all = useBoard((s) => s.comments);
  const canEdit = useCan('edit_tickets');
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    if (!ticketId) {
      setLoaded(true);
      return;
    }
    let alive = true;
    api
      .comments(ticketId)
      .then((cs) => {
        if (!alive) return;
        useBoard.getState().mergeComments(cs);
        setLoaded(true);
      })
      .catch(() => alive && setLoaded(true));
    return () => {
      alive = false;
    };
  }, [ticketId]);

  const threads = useMemo(() => {
    const mine = Object.values(all).filter((c) => (ticketId ? c.ticket_id === ticketId : c.job_id === jobId));
    const roots = mine.filter((c) => !c.parent_id).sort((a, b) => Number(a.resolved) - Number(b.resolved) || a.created_at.localeCompare(b.created_at));
    return roots.map((r) => ({ root: r, replies: mine.filter((c) => c.parent_id === r.id).sort((a, b) => a.created_at.localeCompare(b.created_at)) }));
  }, [all, ticketId, jobId]);
  const focusRoot = focusId ? (all[focusId]?.parent_id ?? focusId) : undefined;
  const open = threads.filter((t) => !t.root.resolved).length;

  return (
    <div className="discussion">
      {header}
      {canEdit && (
        <div className={`new-thread ${draft && draft.on !== 'ticket' ? 'anchored' : ''}`}>
          {draft && draft.on !== 'ticket' && (
            <div className="row">
              <span className="grow">
                <AnchorBadge a={draft} />
              </span>
              <button className="btn sm ghost" onClick={onDraftDone}>
                ✕
              </button>
            </div>
          )}
          <Composer
            key={JSON.stringify(draft ?? {})}
            autoFocus={!!draft && draft.on !== 'ticket'}
            placeholder={draft && draft.on !== 'ticket' ? 'Comment on this…' : 'Start a thread… (@ to mention a teammate or an agent)'}
            ticketId={ticketId}
            onSubmit={async (body) => {
              const c = await attempt(() => api.createComment({ ticket_id: ticketId, job_id: jobId, anchor: draft ?? { on: 'ticket' }, parent_id: null, body }));
              if (c) {
                useBoard.getState().upsert({ entity: 'comment', data: c });
                if (c.triggered_run_ids.length) toast('Agent mentioned: a run is starting ⚡', 'gold');
                onDraftDone?.();
              }
              return !!c;
            }}
          />
        </div>
      )}
      <div className="section-title">
        {open} open thread{open === 1 ? '' : 's'}
        {threads.length - open ? ` · ${threads.length - open} resolved` : ''}
      </div>
      {threads.map((t) => (
        <Thread key={t.root.id} root={t.root} replies={t.replies} ticketId={ticketId} jobId={jobId} onJump={onJump} focus={t.root.id === focusRoot} />
      ))}
      {loaded && threads.length === 0 && <p className="muted">No discussion yet. Select text in the task or hover a transcript line to comment on it.</p>}
    </div>
  );
}

let lastComments: unknown = null;
let lastCounts: Record<string, number> = {};
function countsOf(cs: Record<string, Comment>): Record<string, number> {
  if (cs === lastComments) return lastCounts;
  const out: Record<string, number> = {};
  for (const c of Object.values(cs)) if (c.ticket_id && !c.deleted) out[c.ticket_id] = (out[c.ticket_id] ?? 0) + 1;
  lastComments = cs;
  lastCounts = out;
  return out;
}

/** Comment count of a ticket (non-deleted), for the badge on its paper. */
export function useCommentCount(ticketId: string): number {
  return useBoard((s) => countsOf(s.comments)[ticketId] ?? 0);
}

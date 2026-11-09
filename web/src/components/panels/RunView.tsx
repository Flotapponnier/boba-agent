import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { api } from '../../api/client';
import { connection } from '../../api/connection';
import type { Run, RunLogLine } from '../../api/types';
import { compact, duration, usd } from '../../lib/format';
import { answerPermission, cancelRun, messageRun, updateAgent } from '../../store/actions';
import { commandPattern } from '../../lib/game';
import { isActive, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { Markdown } from '../ui/Markdown';
import { Sprite } from '../ui/Sprite';
import { runnerLabel } from '../../lib/runner';
import { useUi } from '../../store/ui';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj => (v && typeof v === 'object' ? (v as Obj) : {});
const str = (v: unknown) => (typeof v === 'string' ? v : v == null ? '' : JSON.stringify(v, null, 2));

export const STATUS_LABEL: Record<Run['status'], string> = {
  queued: 'Queued',
  running: 'Running',
  waiting: 'Needs you',
  succeeded: 'Done',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
}

function ToolInput({ name, input }: { name: string; input: Obj }) {
  if (name === 'Edit' || name === 'MultiEdit') {
    const edits = name === 'MultiEdit' && Array.isArray(input.edits) ? (input.edits as Obj[]) : [input];
    return (
      <>
        <span className="lbl">{str(input.file_path)}</span>
        {edits.map((e, i) => (
          <pre key={i}>
            {str(e.old_string)
              .split('\n')
              .map((l, j) => (
                <span key={`d${j}`} className="diff-del">
                  - {l}
                </span>
              ))}
            {str(e.new_string)
              .split('\n')
              .map((l, j) => (
                <span key={`a${j}`} className="diff-add">
                  + {l}
                </span>
              ))}
          </pre>
        ))}
      </>
    );
  }
  if (name === 'Bash')
    return (
      <>
        {!!input.description && <span className="lbl">{str(input.description)}</span>}
        <pre>$ {str(input.command)}</pre>
      </>
    );
  if (name === 'Write')
    return (
      <>
        <span className="lbl">{str(input.file_path)}</span>
        <pre>{str(input.content)}</pre>
      </>
    );
  if (name === 'TodoWrite' && Array.isArray(input.todos))
    return (
      <ul className="todos">
        {(input.todos as Obj[]).map((t, i) => (
          <li key={i} className={str(t.status)}>
            {t.status === 'completed' ? '☑' : t.status === 'in_progress' ? '▶' : '☐'} {str(t.content)}
          </li>
        ))}
      </ul>
    );
  return <pre>{JSON.stringify(input, null, 2)}</pre>;
}

const ToolCard = memo(function ToolCard({ use, result, defaultOpen }: { use: RunLogLine; result?: RunLogLine; defaultOpen: boolean }) {
  const d = obj(use.data);
  const name = str(d.name) || 'Tool';
  const r = obj(result?.data);
  const isErr = r.is_error === true;
  const summary = use.text.startsWith(name) ? use.text.slice(name.length).trim() : use.text;
  return (
    <details className={`tool ${isErr ? 'err' : ''} ${result ? '' : 'pending'}`} open={defaultOpen || name === 'TodoWrite'}>
      <summary>
        <span className="tool-name">{name}</span>
        <span className="tool-sum">{summary}</span>
        <span className="tool-state">{result ? (isErr ? '✗' : '✓') : '…'}</span>
      </summary>
      <div className="tool-body">
        <ToolInput name={name} input={obj(d.input)} />
        {result && (
          <>
            <span className="lbl">{isErr ? 'ERROR' : 'OUTPUT'}</span>
            <pre>{str(r.output ?? r.content) || result.text}</pre>
          </>
        )}
      </div>
    </details>
  );
});

function PermissionCard({ line, resolved, run }: { line: RunLogLine; resolved?: { allow: boolean; user_id?: string }; run: Run }) {
  const p = obj(obj(line.data).permission_request);
  const canRun = useCan('run_agents');
  const canAgents = useCan('manage_agents');
  const agent = useBoard((s) => s.agents[run.agent_id]);
  const input = obj(p.input);
  const pattern = str(p.tool) === 'Bash' ? commandPattern(str(input.command)) : null;
  const who = useBoard((s) => (resolved?.user_id ? s.users[resolved.user_id]?.name : undefined));
  const [busy, setBusy] = useState(false);
  const answer = async (allow: boolean) => {
    setBusy(true);
    await answerPermission(run.id, str(p.request_id), allow);
    setBusy(false);
  };
  return (
    <div className={`perm ${resolved ? 'resolved' : ''}`}>
      <h4>✋ PERMISSION: {str(p.tool) || 'tool'}</h4>
      <pre>{JSON.stringify(p.input ?? {}, null, 2)}</pre>
      {resolved ? (
        <span>{resolved.allow ? `✓ Allowed${who ? ` by ${who}` : ''}` : `✗ Denied${who ? ` by ${who}` : ''}`}</span>
      ) : canRun && isActive(run) ? (
        <div className="row">
          <button className="btn green sm" disabled={busy} onClick={() => void answer(true)}>
            Allow
          </button>
          <button className="btn red sm" disabled={busy} onClick={() => void answer(false)}>
            Deny
          </button>
          {pattern && canAgents && agent && !agent.allowed_tools.includes(pattern) && (
            <button
              className="btn gold sm"
              disabled={busy}
              title={`Adds ${pattern} to ${agent.name}'s allowed tools, then allows this call`}
              onClick={async () => {
                setBusy(true);
                await updateAgent(agent.id, { allowed_tools: [...agent.allowed_tools, pattern] });
                await answerPermission(run.id, str(p.request_id), true);
                setBusy(false);
              }}
            >
              Always allow <code>{pattern.slice(5, -3)}</code> for {agent.name}
            </button>
          )}
        </div>
      ) : (
        <span className="muted">Waiting for someone with the Run agents permission…</span>
      )}
    </div>
  );
}

function Verdict({ text }: { text: string }) {
  const m = text.match(/```boba-verdict\s*([\s\S]*?)```/);
  if (!m) return <Markdown text={text} className="l-text" />;
  let v: Obj = {};
  try {
    v = obj(JSON.parse(m[1] ?? '{}'));
  } catch {
    /* malformed verdict, show raw */
  }
  const verdict = str(v.verdict);
  return (
    <div className="l-text">
      <Markdown text={text.replace(m[0], '').trim()} />
      <div style={{ marginTop: 6 }}>
        <span className={`verdict ${verdict}`}>{verdict === 'approved' ? '✓ APPROVED' : '✗ CHANGES REQUESTED'}</span>
        {!!v.feedback && <p style={{ marginTop: 4 }}>{str(v.feedback)}</p>}
      </div>
    </div>
  );
}

function UserLine({ line }: { line: RunLogLine }) {
  const name = useBoard((s) => s.users[str(obj(line.data).user_id)]?.name);
  return (
    <div className="l-user">
      {name && <small>{name}</small>}
      {line.text}
    </div>
  );
}

/** A subscription runner doesn't charge per token: show the list-price equivalent instead of a cost. */
function CostLabel({ run }: { run: Run }) {
  const sub = useBoard((s) => (run.runner_id ? s.runners[run.runner_id]?.auth === 'subscription' : false));
  return sub ? (
    <span title="This run used a Claude subscription: nothing is charged per token">
      💰 <b>≈ {usd(run.cost_usd)}</b> list-price equivalent (included in subscription)
    </span>
  ) : (
    <span>
      💰 <b>{usd(run.cost_usd)}</b>
    </span>
  );
}

function RunnerBadge({ run }: { run: Run }) {
  const runners = useBoard((s) => s.runners);
  const users = useBoard((s) => s.users);
  const setView = useUi((u) => u.setView);
  const open = useUi((u) => u.open);
  const me = useBoard((s) => s.me?.id);
  const l = runnerLabel(run, runners, users);
  if (!l) return null;
  return (
    <div className={`runner-badge ${l.real ? 'real' : 'sim'}`} title={l.hint}>
      <span>{l.real ? '⚡' : '🎭'} {l.text}</span>
      {!l.real && run.started_by === me && (
        <button
          className="link-btn"
          onClick={() => {
            open(null);
            setView('runners');
          }}
        >
          Connect your own Claude Code to run it for real →
        </button>
      )}
    </div>
  );
}

/** Live transcript of one run: text, collapsed thinking, tool cards, permission prompts. */
export function RunView({
  run,
  onSwitchRun,
  lineCounts,
  focusSeq,
  onCommentLine,
  onCommentRun,
}: {
  run: Run;
  onSwitchRun?: (id: string) => void;
  lineCounts?: Record<number, number>;
  focusSeq?: number;
  onCommentLine?: (seq: number) => void;
  onCommentRun?: () => void;
}) {
  const canComment = useCan('edit_tickets');
  const lines = useBoard((s) => s.logs[run.id]);
  const agent = useBoard((s) => s.agents[run.agent_id]);
  const canRun = useCan('run_agents');
  const active = isActive(run);
  const now = useNow(active);
  const box = useRef<HTMLDivElement>(null);
  const [stick, setStick] = useState(true);
  const [msg, setMsg] = useState('');
  const [sending, setSending] = useState(false);

  // History first (REST), then live (WS subscription replays the last 200 lines).
  useEffect(() => {
    let alive = true;
    api
      .runLogs(run.id, undefined, 2000)
      .then((ls) => alive && useBoard.getState().mergeLogs(run.id, ls))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [run.id]);
  useEffect(() => (active ? connection.subscribe(run.id) : undefined), [run.id, active]);

  const items = useMemo(() => {
    const ls = lines ?? [];
    const results = new Map<string, RunLogLine>();
    const resolved = new Map<string, { allow: boolean; user_id?: string }>();
    for (const l of ls) {
      if (l.kind === 'tool_result') results.set(str(obj(l.data).tool_use_id), l);
      const pr = obj(obj(l.data).permission_resolved);
      if (pr.request_id) resolved.set(str(pr.request_id), { allow: pr.allow === true, user_id: pr.user_id ? str(pr.user_id) : undefined });
    }
    return { ls, results, resolved };
  }, [lines]);

  useLayoutEffect(() => {
    const el = box.current;
    if (el && stick) el.scrollTop = el.scrollHeight;
  }, [items, stick]);

  // Jump to a commented line.
  useEffect(() => {
    if (focusSeq === undefined) return;
    const t = setTimeout(() => {
      const el = box.current?.querySelector(`[data-seq="${focusSeq}"]`);
      if (el) {
        setStick(false);
        el.scrollIntoView({ behavior: 'smooth', block: 'center' });
      }
    }, 300);
    return () => clearTimeout(t);
  }, [focusSeq, lines?.length]);

  const onScroll = () => {
    const el = box.current;
    if (el) setStick(el.scrollTop + el.clientHeight >= el.scrollHeight - 24);
  };

  const send = async () => {
    const text = msg.trim();
    if (!text) return;
    setSending(true);
    const r = await messageRun(run.id, text);
    setSending(false);
    if (r) {
      setMsg('');
      setStick(true);
      if (r.id !== run.id) onSwitchRun?.(r.id);
    }
  };

  const renderLine = (l: RunLogLine, i: number): ReactNode => {
    switch (l.kind) {
      case 'text':
        return <Verdict key={l.seq} text={l.text} />;
      case 'thinking':
        return (
          <details key={l.seq} className="l-thinking">
            <summary>Thinking…</summary>
            {l.text}
          </details>
        );
      case 'tool_use':
        return <ToolCard key={l.seq} use={l} result={items.results.get(str(obj(l.data).id))} defaultOpen={active && i === lastToolIdx} />;
      case 'tool_result': {
        const id = str(obj(l.data).tool_use_id);
        // Orphan results (no matching tool_use) still show.
        return items.ls.some((u) => u.kind === 'tool_use' && str(obj(u.data).id) === id) ? null : (
          <pre key={l.seq} className="l-system">
            {l.text}
          </pre>
        );
      }
      case 'system': {
        const d = obj(l.data);
        if (d.permission_request) return <PermissionCard key={l.seq} line={l} run={run} resolved={items.resolved.get(str(obj(d.permission_request).request_id))} />;
        if (d.permission_resolved) return null;
        return (
          <div key={l.seq} className="l-system">
            {l.text}
          </div>
        );
      }
      case 'error':
        return (
          <div key={l.seq} className="l-error">
            {l.text}
          </div>
        );
      case 'user':
        return <UserLine key={l.seq} line={l} />;
      default:
        return null;
    }
  };

  const lastToolIdx = (() => {
    for (let i = items.ls.length - 1; i >= 0; i--) if (items.ls[i]?.kind === 'tool_use') return i;
    return -1;
  })();

  return (
    <div>
      <div className="run-head">
        <div className="row">
          {agent && <Sprite sprite={agent.sprite} color={agent.color} status={active ? agent.status : 'idle'} size={40} />}
          <div className="grow">
            <b className="label">
              {agent?.name ?? 'Agent'} · {run.kind === 'review' ? 'Review' : run.kind === 'job' ? 'Quest' : run.kind === 'command' ? 'Concierge' : 'Work'}
            </b>
            <div className="hint-text">{run.activity ?? (active ? 'Starting…' : run.summary ?? '')}</div>
          </div>
          <span className={`run-status ${run.status}`}>{STATUS_LABEL[run.status]}</span>
        </div>
        <RunnerBadge run={run} />
        {active && (
          <div className="bar" style={{ '--c': agent?.color ?? '#f39c12', marginTop: 0 } as React.CSSProperties}>
            <i style={{ width: `${Math.max(3, Math.round(run.progress * 100))}%` }} />
          </div>
        )}
        <div className="run-meta">
          <span>
            ⏱ <b>{duration(run.started_at ?? run.created_at, run.ended_at, now)}</b>
          </span>
          <CostLabel run={run} />
          <span>
            ⇣ <b>{compact(run.input_tokens)}</b> ⇡ <b>{compact(run.output_tokens)}</b> tokens
          </span>
          {run.branch && (
            <span>
              ⎇ <b>{run.branch}</b>
            </span>
          )}
          {run.verdict && <span className={`verdict ${run.verdict}`}>{run.verdict === 'approved' ? 'APPROVED' : 'CHANGES REQUESTED'}</span>}
        </div>
        {run.error && <div className="form-error">⚠ {run.error}</div>}
        <div className="row">
          {active && canRun && (
            <button className="btn red sm" onClick={() => void cancelRun(run.id)}>
              ⏹ Call {agent?.name ?? 'the agent'} back
            </button>
          )}
          {canComment && onCommentRun && (
            <button className="btn sm ghost" onClick={onCommentRun} title="Start a thread about this run (verdict, report…)">
              💬 Discuss this {run.kind === 'review' ? 'review' : run.kind === 'job' ? 'report' : 'run'}
            </button>
          )}
        </div>
      </div>

      <div className="transcript" ref={box} onScroll={onScroll} aria-live={active ? 'polite' : 'off'} aria-label="Transcript">
        {items.ls.length === 0 && <div className="l-system">{active ? 'Waiting for the first words…' : 'No transcript for this run.'}</div>}
        {items.ls.map((l, i) => {
          const node = renderLine(l, i);
          if (!node) return null;
          const n = lineCounts?.[l.seq];
          return (
            <div key={l.seq} className={`tl ${focusSeq === l.seq ? 'focus' : ''}`} data-seq={l.seq}>
              {node}
              {n ? (
                <button className="tl-n" onClick={() => onCommentLine?.(l.seq)} title={`${n} comment(s) on this line`}>
                  💬 {n}
                </button>
              ) : null}
              {canComment && onCommentLine && (
                <button className="tl-c" onClick={() => onCommentLine(l.seq)} aria-label="Comment on this line" title="Comment on this line">
                  💬
                </button>
              )}
            </div>
          );
        })}
        {!stick && (
          <button className="jump-live" onClick={() => setStick(true)}>
            ↓ Jump to latest
          </button>
        )}
      </div>

      {canRun && (
        <form
          className="follow"
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          <label className="grow">
            <span className="sr-only">Message to the agent</span>
            <textarea
              value={msg}
              onChange={(e) => setMsg(e.target.value)}
              placeholder={active ? `Tell ${agent?.name ?? 'the agent'} something…` : 'Send a follow-up (starts a new round)…'}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
            />
          </label>
          <button className="btn gold" disabled={sending || !msg.trim()}>
            Send
          </button>
        </form>
      )}
    </div>
  );
}

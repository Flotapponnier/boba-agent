import { AnimatePresence, motion } from 'motion/react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../../api/client';
import type { ActionResult, CommandAction, CommandPlan, Permission, PlannedAction, Priority } from '../../api/types';
import { sfx } from '../../audio/audio';
import { PRIORITIES, PRIORITY_LABEL, bodyUrl } from '../../lib/game';
import { describe, localTz } from '../../lib/schedule';
import { speak, startVoice, voiceSupport, type VoiceSession } from '../../lib/voice';
import { errorText } from '../../store/actions';
import { fxBus, planBus, toast } from '../../store/bus';
import { usePrefs } from '../../store/prefs';
import { useAgentsList, useConcierge, useTabs } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi, type Modal, type View } from '../../store/ui';
import { UNAVAILABLE, useAvailable, useFeatures } from '../../store/features';

type Phase =
  | { k: 'input' }
  | { k: 'listening' }
  | { k: 'thinking'; runId: string }
  | { k: 'plan'; plan: CommandPlan }
  | { k: 'executing'; plan: CommandPlan };

const PERM_LABEL: Record<Permission, string> = {
  view: 'View',
  edit_tickets: 'Edit tickets',
  run_agents: 'Run agents',
  manage_agents: 'Manage agents',
  manage_skills: 'Manage skills',
  manage_workspace: 'Manage workspace',
  manage_users: 'Manage users',
  manage_automations: 'Manage automations',
};

const CONCIERGE = 'golden_boba';

interface Quick {
  id: string;
  label: string;
  hint: string;
  run: () => void;
}

function useQuickActions(): Quick[] {
  const tabs = useTabs();
  const agents = useAgentsList();
  const tickets = useBoard((s) => s.tickets);
  const ui = useUi.getState;
  return useMemo(() => {
    const go = (v: View) => () => ui().setView(v);
    const modal = (m: Modal) => () => ui().show(m);
    const q: Quick[] = [
      ...tabs.map((t) => ({ id: `tab-${t.id}`, label: `${t.icon} ${t.name}`, hint: 'Go to tab', run: () => ui().setTab(t.id) })),
      { id: 'quests', label: '📜 Quest board', hint: 'Go to', run: go('quests') },
      { id: 'integrations', label: '🔗 Integrations', hint: 'Go to', run: go('integrations') },
      { id: 'portals', label: '🌀 Portals', hint: 'Go to', run: go('portals') },
      { id: 'new-ticket', label: '📌 New ticket', hint: 'Create', run: () => ui().activeTab && ui().show({ kind: 'ticket-new', tabId: ui().activeTab as string }) },
      { id: 'recruit', label: '🎉 Recruit a boba', hint: 'Create', run: modal({ kind: 'agent-new' }) },
      { id: 'quest', label: '📜 Post a quest', hint: 'Create', run: modal({ kind: 'quest-new' }) },
      { id: 'history', label: '🕰 Timeline', hint: 'Open', run: () => ui().open({ kind: 'history' }) },
      { id: 'skills', label: '📚 Skill library', hint: 'Open', run: () => ui().open({ kind: 'skills' }) },
      { id: 'settings', label: '⚙ Settings', hint: 'Open', run: modal({ kind: 'settings' }) },
      ...agents.map((a) => ({ id: `agent-${a.id}`, label: `${a.name} (${a.role})`, hint: 'Agent profile', run: () => ui().open({ kind: 'agent', id: a.id }) })),
      ...Object.values(tickets)
        .sort((a, b) => b.number - a.number)
        .slice(0, 60)
        .map((t) => ({ id: `t-${t.id}`, label: `T-${t.number} ${t.title}`, hint: 'Ticket', run: () => (ui().setTab(t.tab_id), ui().open({ kind: 'ticket', id: t.id })) })),
    ];
    return q;
  }, [tabs, agents, tickets, ui]);
}

function Waveform({ levels }: { levels: number[] }) {
  return (
    <div className="wave" aria-hidden="true">
      {levels.map((l, i) => (
        <i key={i} style={{ transform: `scaleY(${0.08 + l * 0.92})` }} />
      ))}
    </div>
  );
}

/** Editable details for a planned action (only the fields worth tweaking). */
function ActionEditor({ a, onChange }: { a: CommandAction; onChange: (a: CommandAction) => void }) {
  const tabs = useTabs();
  const agents = useAgentsList();
  switch (a.action) {
    case 'create_ticket':
      return (
        <div className="field-row">
          <input value={a.ticket.title} onChange={(e) => onChange({ ...a, ticket: { ...a.ticket, title: e.target.value } })} aria-label="Title" />
          <select value={a.ticket.tab_id} onChange={(e) => onChange({ ...a, ticket: { ...a.ticket, tab_id: e.target.value, column_id: null } })} aria-label="Tab">
            {tabs.map((t) => (
              <option key={t.id} value={t.id}>
                {t.icon} {t.name}
              </option>
            ))}
          </select>
          <select value={a.ticket.priority ?? 'normal'} onChange={(e) => onChange({ ...a, ticket: { ...a.ticket, priority: e.target.value as Priority } })} aria-label="Priority">
            {PRIORITIES.map((p) => (
              <option key={p} value={p}>
                {PRIORITY_LABEL[p]}
              </option>
            ))}
          </select>
        </div>
      );
    case 'assign_agent':
      return (
        <select value={a.assign.agent_id} onChange={(e) => onChange({ ...a, assign: { ...a.assign, agent_id: e.target.value } })} aria-label="Agent">
          {agents.map((x) => (
            <option key={x.id} value={x.id} disabled={x.status !== 'idle'}>
              {x.name} {x.status !== 'idle' ? '(busy)' : ''}
            </option>
          ))}
        </select>
      );
    case 'create_job':
      return (
        <div>
          <div className="field-row">
            <input value={a.job.name} onChange={(e) => onChange({ ...a, job: { ...a.job, name: e.target.value } })} aria-label="Quest name" />
            <select value={a.job.agent_id} onChange={(e) => onChange({ ...a, job: { ...a.job, agent_id: e.target.value } })} aria-label="Agent">
              {agents.map((x) => (
                <option key={x.id} value={x.id}>
                  {x.name}
                </option>
              ))}
            </select>
          </div>
          <p className="hint-text">
            ⏳ {describe(a.job.schedule)} · {a.job.output === 'tickets' ? '📋 files tickets' : a.job.output === 'pull_request' ? '🔧 opens a PR' : '📜 writes a report'} (fine-tune it on the Quest board after)
          </p>
        </div>
      );
    case 'create_agent':
      return (
        <div className="field-row">
          <input value={a.agent.name} onChange={(e) => onChange({ ...a, agent: { ...a.agent, name: e.target.value } })} aria-label="Name" />
          <input value={a.agent.role} onChange={(e) => onChange({ ...a, agent: { ...a.agent, role: e.target.value } })} aria-label="Role" />
        </div>
      );
    case 'create_tab':
      return <input value={a.tab.name} onChange={(e) => onChange({ ...a, tab: { ...a.tab, name: e.target.value } })} aria-label="Tab name" />;
    default:
      return null;
  }
}

function PlanCard({ p, checked, onCheck, onEdit }: { p: PlannedAction; checked: boolean; onCheck: (v: boolean) => void; onEdit: (a: CommandAction) => void }) {
  const [editing, setEditing] = useState(false);
  const role = useBoard((s) => s.me?.role);
  const editable = ['create_ticket', 'assign_agent', 'create_job', 'create_agent', 'create_tab'].includes(p.action.action);
  return (
    <motion.div layout className={`plan-card ${p.allowed ? '' : 'denied'} ${checked ? 'on' : ''}`} initial={{ opacity: 0, y: 12, rotate: -1 }} animate={{ opacity: 1, y: 0, rotate: 0 }}>
      <label className="row">
        <input type="checkbox" checked={checked && p.allowed} disabled={!p.allowed} onChange={(e) => onCheck(e.target.checked)} aria-label={p.label} />
        <span className="grow">{p.label}</span>
        {p.allowed && editable && (
          <button type="button" className="btn sm ghost" onClick={() => setEditing((v) => !v)} aria-expanded={editing}>
            {editing ? 'Done' : '✎'}
          </button>
        )}
      </label>
      {!p.allowed && (
        <p className="hint-text">
          🔒 Needs the “{PERM_LABEL[p.required]}” permission (you are {role}).
        </p>
      )}
      {editing && p.allowed && (
        <div style={{ marginTop: 6 }}>
          <ActionEditor a={p.action} onChange={onEdit} />
        </div>
      )}
    </motion.div>
  );
}

/** Focus what the concierge just created/changed so the user sees it happen. */
function focusResults(results: ActionResult[]) {
  const ui = useUi.getState();
  const st = useBoard.getState();
  for (const r of results) {
    if (!r.ok || !r.entity) continue;
    st.upsert(r.entity);
    const e = r.entity;
    if (e.entity === 'ticket') {
      ui.setTab(e.data.tab_id);
      ui.setSpotlight(e.data.id);
      setTimeout(() => {
        document.querySelector(`[data-ticket="${e.data.id}"]`)?.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'center' });
        fxBus.emit({ fx: 'assigned', agent_id: '', ticket_id: e.data.id, via: 'drag' });
      }, 150);
    } else if (e.entity === 'agent') {
      fxBus.emit({ fx: 'recruited', agent_id: e.data.id });
      setTimeout(() => document.querySelector(`[data-agent-card="${e.data.id}"]`)?.scrollIntoView({ behavior: 'smooth', block: 'center' }), 100);
    } else if (e.entity === 'job') {
      ui.setView('quests');
      ui.setSpotlight(e.data.id);
    } else if (e.entity === 'tab') {
      ui.setTab(e.data.id);
    } else if (e.entity === 'run' && e.data.ticket_id) {
      const t = st.tickets[e.data.ticket_id];
      if (t) {
        ui.setTab(t.tab_id);
        ui.open({ kind: 'ticket', id: t.id, view: 'live' });
      }
    }
  }
  setTimeout(() => useUi.getState().setSpotlight(null), 2600);
}

export function CommandBar() {
  const cmd = useUi((u) => u.command);
  const setCommand = useUi((u) => u.setCommand);
  const speech = useBoard((s) => s.settings?.speech);
  const quick = useQuickActions();
  const conciergeOk = useAvailable('command');
  const concierge = useConcierge();
  const speaker = concierge?.sprite ?? CONCIERGE;
  const speakerName = concierge?.name ?? 'The Concierge';
  const [phase, setPhase] = useState<Phase>({ k: 'input' });
  const [text, setText] = useState('');
  const [sel, setSel] = useState(0);
  const [levels, setLevels] = useState<number[]>(() => Array(32).fill(0.1));
  const [voiceErr, setVoiceErr] = useState<string | null>(null);
  const [checked, setChecked] = useState<Record<number, boolean>>({});
  const [answer, setAnswer] = useState('');
  const lastText = useRef('');
  const voice = useRef<VoiceSession | null>(null);
  const viaVoice = useRef(false);
  const stopWhenReady = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  const thinkingRun = useBoard((s) => (phase.k === 'thinking' ? s.runs[phase.runId] : undefined));
  const phaseRef = useRef(phase);
  phaseRef.current = phase;

  const close = useCallback(() => {
    (document.activeElement as HTMLElement | null)?.blur?.();
    voice.current?.cancel();
    voice.current = null;
    setCommand({ open: false });
    setPhase({ k: 'input' });
    setText('');
    setVoiceErr(null);
    setAnswer('');
  }, [setCommand]);

  const send = useCallback(async (t: string, isVoice: boolean) => {
    const s = t.trim();
    if (!s) {
      setPhase({ k: 'input' });
      return;
    }
    lastText.current = s;
    const ui = useUi.getState();
    const panel = ui.panel;
    setPhase({ k: 'thinking', runId: '' });
    try {
      const run = await api.command({
        text: s,
        tab_id: ui.view === 'board' ? ui.activeTab : null,
        ticket_id: panel?.kind === 'ticket' ? panel.id : null,
        agent_id: panel?.kind === 'agent' ? panel.id : null,
        voice: isVoice,
        // So "at 9" means 9 o'clock for the person speaking.
        timezone: localTz(),
      });
      useBoard.getState().upsert({ entity: 'run', data: run });
      setPhase((p) => (p.k === 'thinking' ? { k: 'thinking', runId: run.id } : p));
    } catch (e) {
      toast(errorText(e), 'bad');
      setPhase({ k: 'input' });
    }
  }, []);

  // Plans arrive on the WebSocket, addressed to us only.
  useEffect(
    () =>
      planBus.on((plan) => {
        const p = phaseRef.current;
        if (p.k !== 'thinking' || (p.runId && p.runId !== plan.run_id)) return;
        setChecked(Object.fromEntries(plan.actions.map((a, i) => [i, a.allowed])));
        sfx('pearl', 0.5);
        if (usePrefs.getState().speakReplies) speak(plan.question ?? plan.reply);
        setPhase({ k: 'plan', plan });
      }),
    [],
  );

  const beginVoice = useCallback(async () => {
    if (useFeatures.getState().missing.command) {
      setCommand({ open: true });
      setVoiceErr(`The Concierge (voice and plain-English commands) is ${UNAVAILABLE.toLowerCase()}. You can still jump anywhere from here.`);
      return;
    }
    const sup = voiceSupport(speech);
    setCommand({ open: true });
    if (!sup.ok) {
      setVoiceErr(sup.why ?? 'Voice is unavailable.');
      setPhase({ k: 'input' });
      return;
    }
    setVoiceErr(null);
    setText('');
    setPhase({ k: 'listening' });
    sfx('pearl', 0.4);
    stopWhenReady.current = false;
    try {
      voice.current = await startVoice(speech, {
        onInterim: setText,
        onLevels: setLevels,
        onError: (m) => setVoiceErr(m),
      });
      // Space was released before the mic was ready.
      if (stopWhenReady.current) void endVoiceRef.current();
    } catch {
      setPhase({ k: 'input' });
    }
  }, [speech, setCommand]);

  const endVoice = useCallback(async () => {
    const v = voice.current;
    voice.current = null;
    if (!v) {
      if (phaseRef.current.k === 'listening') stopWhenReady.current = true;
      return;
    }
    const t = await v.stop();
    viaVoice.current = true;
    if (t) {
      setText(t);
      void send(t, true);
    } else {
      setPhase({ k: 'input' });
      setVoiceErr((e) => e ?? "I didn't catch that. Try again or type.");
    }
  }, [send]);
  const endVoiceRef = useRef(endVoice);
  endVoiceRef.current = endVoice;

  // Global shortcuts: ⌘K / Ctrl+K, hold Space to talk.
  useEffect(() => {
    let spaceDown = false;
    const typing = (el: EventTarget | null) => !!(el as HTMLElement | null)?.closest?.('input, textarea, select, [contenteditable], button, [role=button]');
    const down = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && useUi.getState().command.open) {
        e.preventDefault();
        close();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        const open = useUi.getState().command.open;
        if (open) close();
        else setCommand({ open: true });
        return;
      }
      if (e.code === 'Space' && !e.repeat && !typing(e.target) && !useUi.getState().modal && phaseRef.current.k !== 'listening') {
        e.preventDefault();
        spaceDown = true;
        void beginVoice();
      }
    };
    const up = (e: KeyboardEvent) => {
      if (e.code === 'Space' && spaceDown) {
        spaceDown = false;
        e.preventDefault();
        void endVoice();
      }
    };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    return () => {
      window.removeEventListener('keydown', down);
      window.removeEventListener('keyup', up);
    };
  }, [beginVoice, endVoice, close, setCommand]);

  // Mic button in the header asks for voice through the UI store.
  useEffect(() => {
    if (cmd.open && cmd.voice && phase.k === 'input') {
      setCommand({ open: true });
      void beginVoice();
    }
    if (cmd.open && cmd.text) {
      setText(cmd.text);
      setCommand({ open: true });
    }
  }, [cmd, phase.k, beginVoice, setCommand]);

  useEffect(() => {
    if (cmd.open && phase.k === 'input') setTimeout(() => input.current?.focus(), 30);
  }, [cmd.open, phase.k]);

  const matches = useMemo(() => {
    const q = text.trim().toLowerCase();
    if (!q) return quick.slice(0, 7);
    return quick.filter((x) => x.label.toLowerCase().includes(q)).slice(0, 6);
  }, [quick, text]);
  const items = text.trim() && conciergeOk ? [{ id: 'ask', label: `Ask the Concierge: “${text.trim()}”`, hint: 'Enter', run: () => void send(text, viaVoice.current) }, ...matches] : matches;

  const execute = async (plan: CommandPlan, only?: number[]) => {
    const idx = only ?? plan.actions.map((_, i) => i).filter((i) => checked[i] && plan.actions[i]?.allowed);
    const actions = idx.map((i) => plan.actions[i]?.action).filter((a): a is CommandAction => !!a);
    if (!actions.length) return;
    setPhase({ k: 'executing', plan });
    try {
      const res = await api.executeCommand(plan.run_id, actions);
      const ok = res.filter((r) => r.ok).length;
      res.filter((r) => !r.ok).forEach((r) => toast(r.error ?? 'An action failed', 'bad'));
      if (ok) {
        sfx('done', 0.6);
        toast(`Done! ${ok} action${ok > 1 ? 's' : ''} carried out ✨`, 'gold', speaker);
      }
      close();
      focusResults(res);
    } catch (e) {
      toast(errorText(e), 'bad');
      setPhase({ k: 'plan', plan });
    }
  };

  const editAction = (i: number, a: CommandAction) =>
    setPhase((p) => (p.k === 'plan' ? { k: 'plan', plan: { ...p.plan, actions: p.plan.actions.map((x, j) => (j === i ? { ...x, action: a } : x)) } } : p));

  return (
    <AnimatePresence>
      {cmd.open && (
        <motion.div className="overlay cmd-overlay" onMouseDown={(e) => e.target === e.currentTarget && close()} initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}>
          <motion.div
            className="cmd wood"
            role="dialog"
            aria-modal="true"
            aria-label="Command bar"
            initial={{ y: -30, scale: 0.96, opacity: 0 }}
            animate={{ y: 0, scale: 1, opacity: 1 }}
            exit={{ y: -20, opacity: 0 }}
            transition={{ type: 'spring', stiffness: 420, damping: 32 }}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation();
                close();
              }
            }}
          >
            <div className="cmd-head">
              <span className={`concierge ${phase.k}`}>
                <img src={bodyUrl(speaker)} alt={speakerName} title={speakerName} />
                {phase.k === 'listening' && <span className="rings" />}
              </span>
              {phase.k === 'input' || phase.k === 'listening' ? (
                <input
                  ref={input}
                  className="cmd-input"
                  value={text}
                  readOnly={phase.k === 'listening'}
                  onChange={(e) => {
                    viaVoice.current = false;
                    setText(e.target.value);
                    setSel(0);
                  }}
                  placeholder={phase.k === 'listening' ? 'Listening…' : 'Ask the Concierge, or jump anywhere…'}
                  aria-label="Command"
                  onKeyDown={(e) => {
                    if (e.key === 'ArrowDown') {
                      e.preventDefault();
                      setSel((s) => Math.min(items.length - 1, s + 1));
                    } else if (e.key === 'ArrowUp') {
                      e.preventDefault();
                      setSel((s) => Math.max(0, s - 1));
                    } else if (e.key === 'Enter') {
                      e.preventDefault();
                      const it = items[sel];
                      if (it) {
                        it.run();
                        if (it.id !== 'ask') close();
                      }
                    }
                  }}
                />
              ) : (
                <div className="cmd-said">“{lastText.current}”</div>
              )}
              <button
                className={`btn icon mic ${phase.k === 'listening' ? 'on' : ''}`}
                aria-label={phase.k === 'listening' ? 'Stop and send' : 'Talk (hold Space)'}
                title="Hold Space anywhere to talk"
                onClick={() => (phase.k === 'listening' ? void endVoice() : void beginVoice())}
              >
                🎙
              </button>
            </div>

            {phase.k === 'listening' && (
              <div className="cmd-body listening">
                <Waveform levels={levels} />
                <p className="hint-text">Speak, then release Space (or tap 🎙) to send.</p>
              </div>
            )}
            {voiceErr && phase.k === 'input' && <p className="form-error cmd-body">{voiceErr}</p>}

            {phase.k === 'input' && (
              <ul className="cmd-list" role="listbox" aria-label="Suggestions">
                {items.map((it, i) => (
                  <li key={it.id} role="option" aria-selected={i === sel}>
                    <button
                      className={i === sel ? 'sel' : ''}
                      onMouseEnter={() => setSel(i)}
                      onClick={() => {
                        it.run();
                        if (it.id !== 'ask') close();
                      }}
                    >
                      <span className="grow">{it.label}</span>
                      <small>{it.hint}</small>
                    </button>
                  </li>
                ))}
                {!conciergeOk && (
                  <li className="cmd-tip hint-text">🚧 The Concierge is {UNAVAILABLE.toLowerCase()}: quick navigation only.</li>
                )}
                {!text && conciergeOk && (
                  <li className="cmd-tip hint-text">
                    Try: “every Monday at 9, Pinky audits Search Console and files tickets” · “send Boba Dev on T-3” · “sync the app repo”
                  </li>
                )}
              </ul>
            )}

            {phase.k === 'thinking' && (
              <div className="cmd-body thinking" role="status">
                <span className="dots">
                  <i />
                  <i />
                  <i />
                </span>
                <span>{thinkingRun?.activity ?? `${speakerName} is reading the board…`}</span>
              </div>
            )}

            {(phase.k === 'plan' || phase.k === 'executing') && (
              <div className="cmd-body">
                {phase.plan.reply && (
                  <p className="cmd-reply">
                    <b>{speakerName}:</b> {phase.plan.reply}
                  </p>
                )}
                {phase.plan.question && (
                  <form
                    className="cmd-question"
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (answer.trim()) void send(`${lastText.current}. ${answer.trim()}`, false);
                      setAnswer('');
                    }}
                  >
                    <p>🤔 {phase.plan.question}</p>
                    <div className="row">
                      <input className="grow" autoFocus value={answer} onChange={(e) => setAnswer(e.target.value)} placeholder="Your answer…" aria-label="Answer" />
                      <button className="btn sm gold">Reply</button>
                    </div>
                  </form>
                )}
                <div className="plan-list">
                  {phase.plan.actions.map((p, i) => (
                    <PlanCard key={i} p={p} checked={!!checked[i]} onCheck={(v) => setChecked((c) => ({ ...c, [i]: v }))} onEdit={(a) => editAction(i, a)} />
                  ))}
                </div>
                {phase.plan.actions.length > 0 && (
                  <div className="row end" style={{ marginTop: 10 }}>
                    <button className="btn ghost" onClick={close}>
                      Cancel
                    </button>
                    <button
                      className="btn gold"
                      disabled={phase.k === 'executing' || !phase.plan.actions.some((a, i) => a.allowed && checked[i])}
                      onClick={() => void execute(phase.plan)}
                    >
                      {phase.k === 'executing' ? 'Working…' : phase.plan.actions.filter((a, i) => a.allowed && checked[i]).length > 1 ? '✓ Confirm all' : '✓ Confirm'}
                    </button>
                  </div>
                )}
                <p className="hint-text" style={{ marginTop: 8 }}>
                  Nothing happens until you confirm. Actions run with your own permissions.
                </p>
              </div>
            )}
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

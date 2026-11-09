import { api } from '../api/client';
import { ApiFailure } from '../api/transport';
import type { CreateAgent, CreateTab, CreateTicket, Entity, Run, Ticket, UpdateAgent, UpdateTab, UpdateTicket } from '../api/types';
import { sfx } from '../audio/audio';
import { fxBus, toast } from './bus';
import { optimistic, useBoard } from './store';
import { useUi } from './ui';

export function errorText(e: unknown): string {
  if (e instanceof ApiFailure) {
    if (e.code === 'unavailable') return e.message;
    if (e.status === 403) return `Your role can't do that. ${e.message}`;
    return e.message;
  }
  return e instanceof Error ? e.message : 'Something went wrong';
}

/** Run a mutation, toast the error, never throw. Returns undefined on failure. */
export async function attempt<T>(fn: () => Promise<T>, rollback?: () => void): Promise<T | undefined> {
  try {
    return await fn();
  } catch (e) {
    rollback?.();
    toast(errorText(e), 'bad');
    return undefined;
  }
}

const up = (item: Entity) => useBoard.getState().upsert(item);

export async function moveTicket(t: Ticket, columnId: string, position: number) {
  if (t.column_id === columnId && t.position === position) return;
  const rb = optimistic('tickets', t.id, { ...t, column_id: columnId, position });
  const res = await attempt(() => api.moveTicket(t.id, { column_id: columnId, position }), rb);
  if (res) up({ entity: 'ticket', data: res });
}

export async function assignAgent(agentId: string, ticketId: string, via: 'drag' | 'tap' = 'tap', instructions: string | null = null): Promise<Run | undefined> {
  const st = useBoard.getState();
  const a = st.agents[agentId];
  const t = st.tickets[ticketId];
  if (!a || !t) return;
  if (a.status !== 'idle') {
    sfx('blocked');
    fxBus.emit({ fx: 'blocked', agent_id: a.id });
    toast(`${a.name} is busy right now!`, 'bad', a.sprite);
    return;
  }
  const rb = optimistic('agents', a.id, { ...a, status: 'working' });
  fxBus.emit({ fx: 'assigned', agent_id: a.id, ticket_id: t.id, via });
  const run = await attempt(() => api.assign(t.id, { agent_id: a.id, instructions }), () => {
    rb();
    sfx('blocked');
    fxBus.emit({ fx: 'blocked', agent_id: a.id });
  });
  if (run) {
    up({ entity: 'run', data: run });
    const st2 = useBoard.getState();
    const r = run.runner_id ? st2.runners[run.runner_id] : undefined;
    const mine = Object.values(st2.runners).some((x) => x.scope === 'personal' && x.owner_user_id === st2.me?.id);
    if (r && (r.auth === 'demo' || r.auth === 'unknown') && !mine && !simHintShown) {
      simHintShown = true;
      setTimeout(() => toast('🎭 Simulated run. Connect your own Claude Code (account menu → Connect) to run it for real.', 'info'), 900);
    }
  }
  return run;
}
let simHintShown = false;

export async function createTicket(b: CreateTicket) {
  const t = await attempt(() => api.createTicket(b));
  if (t) {
    up({ entity: 'ticket', data: t });
    sfx('pearl', 0.5);
    toast(`T-${t.number} pinned to the board`, 'good');
  }
  return t;
}

export async function updateTicket(t: Ticket, patch: Partial<UpdateTicket>) {
  const rb = optimistic('tickets', t.id, { ...t, ...(patch as Partial<Ticket>) });
  const res = await attempt(() => api.updateTicket(t.id, patch), rb);
  if (res) up({ entity: 'ticket', data: res });
  return res;
}

export async function deleteTicket(t: Ticket) {
  const rb = optimistic('tickets', t.id, null);
  const ui = useUi.getState();
  if (ui.panel && 'id' in ui.panel && ui.panel.id === t.id) ui.open(null);
  await attempt(() => api.deleteTicket(t.id), rb);
}

export async function createAgent(b: CreateAgent) {
  const a = await attempt(() => api.createAgent(b));
  if (a) {
    up({ entity: 'agent', data: a });
    sfx('done', 0.7);
    fxBus.emit({ fx: 'recruited', agent_id: a.id });
    toast(`${a.name} joined the tavern!`, 'gold', a.sprite);
  }
  return a;
}

export async function updateAgent(id: string, patch: Partial<UpdateAgent>) {
  const cur = useBoard.getState().agents[id];
  const rb = cur ? optimistic('agents', id, { ...cur, ...(patch as object) }) : undefined;
  const a = await attempt(() => api.updateAgent(id, patch), rb);
  if (a) {
    up({ entity: 'agent', data: a });
    toast(`${a.name} saved`, 'good');
  }
  return a;
}

export async function deleteAgent(id: string) {
  const a = useBoard.getState().agents[id];
  const rb = optimistic('agents', id, null);
  const ok = await attempt(() => api.deleteAgent(id).then(() => true), rb);
  if (ok && a) toast(`${a.name} left the tavern. Bye!`, 'info', a.sprite);
  return ok;
}

export async function createTab(b: CreateTab) {
  const t = await attempt(() => api.createTab(b));
  if (t) {
    up({ entity: 'tab', data: t });
    useUi.getState().setTab(t.id);
  }
  return t;
}

export async function updateTab(id: string, patch: Partial<UpdateTab>) {
  const t = await attempt(() => api.updateTab(id, patch));
  if (t) up({ entity: 'tab', data: t });
  return t;
}

export async function deleteTab(id: string) {
  const rb = optimistic('tabs', id, null);
  return attempt(() => api.deleteTab(id).then(() => true), rb);
}

export async function cancelRun(id: string) {
  const r = await attempt(() => api.cancelRun(id));
  if (r) up({ entity: 'run', data: r });
}

export async function messageRun(id: string, text: string) {
  const r = await attempt(() => api.messageRun(id, text));
  if (r) up({ entity: 'run', data: r });
  return r;
}

export async function answerPermission(runId: string, requestId: string, allow: boolean) {
  return attempt(() => api.answerPermission(runId, { request_id: requestId, allow }).then(() => true));
}

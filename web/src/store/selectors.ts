import { useMemo } from 'react';
import { useShallow } from 'zustand/react/shallow';
import type { Column, Permission, Run, Tab, Ticket } from '../api/types';
import { grants, isConcierge } from '../lib/game';
import { useBoard } from './store';

export const ACTIVE: Run['status'][] = ['queued', 'running', 'waiting'];
export const isActive = (r: Run | undefined | null) => !!r && ACTIVE.includes(r.status);

export function useCan(p: Permission): boolean {
  return useBoard((s) => grants(s.me?.role, p));
}

export function useTabs(): Tab[] {
  const tabs = useBoard((s) => s.tabs);
  return useMemo(() => Object.values(tabs).sort((a, b) => a.position - b.position), [tabs]);
}

export const sortedColumns = (tab: Tab): Column[] => [...tab.columns].sort((a, b) => a.position - b.position);

export function useTicketsByColumn(tabId: string | null): Record<string, Ticket[]> {
  const tickets = useBoard((s) => s.tickets);
  return useMemo(() => {
    const out: Record<string, Ticket[]> = {};
    for (const t of Object.values(tickets)) {
      if (t.tab_id !== tabId) continue;
      (out[t.column_id] ??= []).push(t);
    }
    for (const k of Object.keys(out)) out[k]?.sort((a, b) => a.position - b.position);
    return out;
  }, [tickets, tabId]);
}

/** The active run of each ticket (by ticket id). */
export function useActiveRuns(): Record<string, Run> {
  const runs = useBoard((s) => s.runs);
  return useMemo(() => {
    const out: Record<string, Run> = {};
    for (const r of Object.values(runs)) if (isActive(r) && r.ticket_id) out[r.ticket_id] = r;
    return out;
  }, [runs]);
}

export function latestRun(runs: Record<string, Run>, ticketId: string): Run | undefined {
  let best: Run | undefined;
  for (const r of Object.values(runs)) if (r.ticket_id === ticketId && (!best || r.created_at > best.created_at)) best = r;
  return best;
}

export function useTicketRuns(ticketId: string): Run[] {
  return useBoard(
    useShallow((s) =>
      Object.values(s.runs)
        .filter((r) => r.ticket_id === ticketId)
        .sort((a, b) => (a.created_at < b.created_at ? 1 : -1)),
    ),
  );
}

/** Agents you can drag, pick and assign: everyone but the built-in Concierge. */
export function useAgentsList() {
  const agents = useBoard((s) => s.agents);
  return useMemo(() => Object.values(agents).filter((a) => !isConcierge(a)).sort((a, b) => a.created_at.localeCompare(b.created_at)), [agents]);
}

/** The built-in Concierge agent, when the server has one. */
export function useConcierge() {
  return useBoard((s) => Object.values(s.agents).find(isConcierge));
}

export function useOpenTicketCount(tabId: string): number {
  return useBoard((s) => {
    const tab = s.tabs[tabId];
    if (!tab) return 0;
    const done = new Set(tab.columns.filter((c) => c.kind === 'done').map((c) => c.id));
    let n = 0;
    for (const t of Object.values(s.tickets)) if (t.tab_id === tabId && !done.has(t.column_id)) n++;
    return n;
  });
}

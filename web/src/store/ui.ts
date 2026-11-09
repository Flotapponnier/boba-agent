import { create } from 'zustand';

export type Panel =
  | { kind: 'ticket'; id: string; view?: 'live' | 'details' | 'history' }
  | { kind: 'agent'; id: string }
  | { kind: 'history' }
  | { kind: 'skills'; id?: string }
  | { kind: 'quest'; id: string };

export type Modal =
  | { kind: 'ticket-new'; tabId: string; columnId?: string }
  | { kind: 'agent-new' }
  | { kind: 'agent-edit'; id: string }
  | { kind: 'tab-new' }
  | { kind: 'tab-edit'; id: string }
  | { kind: 'settings'; section?: 'me' | 'workspace' | 'team' | 'repos' }
  | { kind: 'shortcuts' }
  | { kind: 'quest-new' }
  | { kind: 'quest-edit'; id: string }
  | { kind: 'portal-new'; preset?: string }
  | { kind: 'portal-edit'; id: string }
  | { kind: 'repo-new' };

export type View = 'board' | 'quests' | 'integrations' | 'portals';

interface UiState {
  view: View;
  setView(v: View): void;
  /** Command bar: closed, open with optional prefilled text. */
  command: { open: boolean; text?: string; voice?: boolean };
  setCommand(c: { open: boolean; text?: string; voice?: boolean }): void;
  /** Entity to briefly highlight after the concierge created it. */
  spotlight: string | null;
  setSpotlight(id: string | null): void;
  activeTab: string | null;
  panel: Panel | null;
  modal: Modal | null;
  /** Tap-to-assign: agent picked, waiting for a ticket click. */
  picked: string | null;
  /** Agent currently being dragged (for drop-target glow). */
  draggingAgent: string | null;
  setTab(id: string | null): void;
  open(p: Panel | null): void;
  show(m: Modal | null): void;
  pick(id: string | null): void;
  setDraggingAgent(id: string | null): void;
}

export const useUi = create<UiState>()((set) => ({
  view: 'board',
  setView: (view) => set({ view }),
  command: { open: false },
  setCommand: (command) => set({ command }),
  spotlight: null,
  setSpotlight: (spotlight) => set({ spotlight }),
  activeTab: null,
  panel: null,
  modal: null,
  picked: null,
  draggingAgent: null,
  setTab: (activeTab) => set({ activeTab, view: 'board' }),
  open: (panel) => set({ panel }),
  show: (modal) => set({ modal }),
  pick: (picked) => set({ picked }),
  setDraggingAgent: (draggingAgent) => set({ draggingAgent }),
}));

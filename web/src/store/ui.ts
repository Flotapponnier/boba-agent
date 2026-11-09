import { create } from 'zustand';

export type Panel =
  | { kind: 'ticket'; id: string; view?: 'live' | 'details' | 'history' }
  | { kind: 'agent'; id: string }
  | { kind: 'history' }
  | { kind: 'skills'; id?: string };

export type Modal =
  | { kind: 'ticket-new'; tabId: string; columnId?: string }
  | { kind: 'agent-new' }
  | { kind: 'agent-edit'; id: string }
  | { kind: 'tab-new' }
  | { kind: 'tab-edit'; id: string }
  | { kind: 'settings'; section?: 'me' | 'workspace' | 'team' | 'repos' }
  | { kind: 'shortcuts' };

interface UiState {
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
  activeTab: null,
  panel: null,
  modal: null,
  picked: null,
  draggingAgent: null,
  setTab: (activeTab) => set({ activeTab }),
  open: (panel) => set({ panel }),
  show: (modal) => set({ modal }),
  pick: (picked) => set({ picked }),
  setDraggingAgent: (draggingAgent) => set({ draggingAgent }),
}));

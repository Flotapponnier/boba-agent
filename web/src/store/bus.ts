import type { Fx } from '../api/types';

type Listener<T> = (v: T) => void;

function channel<T>() {
  const ls = new Set<Listener<T>>();
  return {
    emit: (v: T) => ls.forEach((l) => l(v)),
    on: (l: Listener<T>) => {
      ls.add(l);
      return () => {
        ls.delete(l);
      };
    },
  };
}

export type ToastTone = 'info' | 'good' | 'bad' | 'gold';
export interface ToastMsg {
  id: number;
  text: string;
  tone: ToastTone;
  sprite?: string;
}

/** Cosmetic one-shot effects (from the server, or local e.g. "agent landed on ticket"). */
export type LocalFx =
  | Fx
  | { fx: 'assigned'; agent_id: string; ticket_id: string; via: 'drag' | 'tap' }
  | { fx: 'recruited'; agent_id: string }
  | { fx: 'blocked'; agent_id: string };

export const fxBus = channel<LocalFx>();
export const toastBus = channel<ToastMsg>();

let toastSeq = 0;
export function toast(text: string, tone: ToastTone = 'info', sprite?: string) {
  toastBus.emit({ id: ++toastSeq, text, tone, sprite });
}

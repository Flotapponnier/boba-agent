import { create } from 'zustand';

export type ThemeChoice = 'workspace' | 'auto' | 'sky' | 'sunset' | 'night';
export type MotionChoice = 'system' | 'reduce' | 'full';

export interface Prefs {
  theme: ThemeChoice;
  music: boolean | null; // null = follow workspace default
  musicVolume: number;
  sfxVolume: number;
  motion: MotionChoice;
  lastTab: string | null;
  tavernCollapsed: boolean;
  demoBots: boolean;
  speakReplies: boolean;
}

const KEY = 'boba-prefs-v1';
const defaults: Prefs = {
  theme: 'workspace',
  music: null,
  musicVolume: 0.5,
  sfxVolume: 0.6,
  motion: 'system',
  lastTab: null,
  tavernCollapsed: false,
  demoBots: true,
  speakReplies: false,
};

function load(): Prefs {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...defaults, ...(JSON.parse(raw) as Partial<Prefs>) };
  } catch {
    /* storage blocked */
  }
  return defaults;
}

interface PrefsState extends Prefs {
  set(p: Partial<Prefs>): void;
}

export const usePrefs = create<PrefsState>()((set, get) => ({
  ...load(),
  set: (p) => {
    set(p);
    try {
      const { set: _s, ...rest } = { ...get(), ...p };
      localStorage.setItem(KEY, JSON.stringify(rest));
    } catch {
      /* storage blocked */
    }
  },
}));

const mq = typeof matchMedia !== 'undefined' ? matchMedia('(prefers-reduced-motion: reduce)') : null;
export const reducedMotion = () => {
  const m = usePrefs.getState().motion;
  return m === 'reduce' || (m === 'system' && !!mq?.matches);
};

export type ThemeName = 'sky' | 'sunset' | 'night';
export function resolveTheme(choice: ThemeChoice, workspace: string | undefined): ThemeName {
  const c = choice === 'workspace' ? workspace ?? 'sky' : choice;
  if (c === 'auto') {
    const h = new Date().getHours();
    return h >= 20 || h < 6 ? 'night' : h >= 17 ? 'sunset' : 'sky';
  }
  return c === 'sunset' || c === 'night' ? c : 'sky';
}

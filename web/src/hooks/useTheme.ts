import { useEffect, useState } from 'react';
import { resolveTheme, usePrefs, type ThemeName } from '../store/prefs';
import { useBoard } from '../store/store';

const META: Record<ThemeName, string> = { sky: '#4682b4', sunset: '#c0587e', night: '#141a3a' };

export function useTheme(): ThemeName {
  const choice = usePrefs((p) => p.theme);
  const ws = useBoard((s) => s.settings?.theme);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 5 * 60_000);
    return () => clearInterval(id);
  }, []);
  // `tick` re-evaluates the "auto" day/night theme every few minutes.
  void tick;
  const theme = resolveTheme(choice, ws);
  useEffect(() => {
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', META[theme]);
    document.documentElement.dataset.theme = theme === 'night' ? 'dark' : 'light';
  }, [theme]);
  return theme;
}

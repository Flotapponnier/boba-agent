import { useEffect } from 'react';
import { music } from '../audio/audio';
import { usePrefs } from '../store/prefs';
import { useBoard } from '../store/store';

/** Keeps the chiptune in sync with prefs; retries after the first user gesture (autoplay rules). */
export function MusicController() {
  const pref = usePrefs((p) => p.music);
  const volume = usePrefs((p) => p.musicVolume);
  const wsDefault = useBoard((s) => s.settings?.music_enabled_by_default ?? false);
  const on = (pref ?? wsDefault) && volume > 0;

  useEffect(() => {
    if (!on) {
      music.stop();
      return;
    }
    music.play(usePrefs.getState().musicVolume);
    const retry = () => music.play(usePrefs.getState().musicVolume);
    window.addEventListener('pointerdown', retry, { once: true });
    window.addEventListener('keydown', retry, { once: true });
    return () => {
      window.removeEventListener('pointerdown', retry);
      window.removeEventListener('keydown', retry);
    };
  }, [on]);

  useEffect(() => {
    if (on) music.setVolume(volume);
  }, [on, volume]);

  useEffect(() => () => music.stop(), []);
  return null;
}

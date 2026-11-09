import { memo, useMemo } from 'react';
import { asset } from '../lib/game';
import type { ThemeName } from '../store/prefs';

/** Drifting Bobavim clouds over a day/sunset/night sky, plus stars and a sun/moon. */
export const Sky = memo(function Sky({ theme }: { theme: ThemeName }) {
  const clouds = useMemo(
    () =>
      Array.from({ length: 12 }, (_, i) => {
        const dur = 50 + Math.random() * 50;
        return {
          src: asset(`sprites/clouds/cloud${1 + (i % 9)}.png`),
          width: 70 + Math.random() * 120,
          top: 2 + Math.random() * 72,
          dur,
          delay: -Math.random() * dur,
        };
      }),
    [],
  );
  const stars = useMemo(
    () => Array.from({ length: 60 }, () => ({ x: Math.random() * 100, y: Math.random() * 65, d: Math.random() * 3, s: Math.random() < 0.2 ? 4 : 2 })),
    [],
  );
  return (
    <>
      <div className="sky" aria-hidden="true" data-theme={theme}>
        <div className="stars">
          {stars.map((s, i) => (
            <i key={i} className="star" style={{ left: `${s.x}%`, top: `${s.y}%`, width: s.s, height: s.s, animationDelay: `${-s.d}s` }} />
          ))}
        </div>
        <div className="orb" />
        {clouds.map((c, i) => (
          <img
            key={i}
            className="cloud"
            src={c.src}
            alt=""
            draggable={false}
            style={{ width: c.width, top: `${c.top}%`, animationDuration: `${c.dur}s`, animationDelay: `${c.delay}s` }}
          />
        ))}
      </div>
      <div className="grass" aria-hidden="true" />
    </>
  );
});

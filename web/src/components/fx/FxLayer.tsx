import { AnimatePresence, motion } from 'motion/react';
import { useEffect, useRef, useState } from 'react';
import { sfx } from '../../audio/audio';
import { PEARL, bodyUrl } from '../../lib/game';
import { fxBus, toast, type LocalFx } from '../../store/bus';
import { reducedMotion } from '../../store/prefs';
import { useBoard } from '../../store/store';
import { Sprite } from '../ui/Sprite';

const CONFETTI = ['#ffd23f', '#ff6fb5', '#3498db', '#2ecc71', '#e74c3c', '#fff', '#9b59b6'];

type Rect = { x: number; y: number; w: number; h: number };
const rectOf = (el: Element | null | undefined): Rect | null => {
  if (!el) return null;
  const r = el.getBoundingClientRect();
  if (r.width === 0 && r.height === 0) return null;
  if (r.bottom < 0 || r.top > innerHeight || r.right < 0 || r.left > innerWidth) return null;
  return { x: r.left, y: r.top, w: r.width, h: r.height };
};
const center = (r: Rect) => ({ x: r.x + r.w / 2, y: r.y + r.h / 2 });
const ticketEl = (id: string) => document.querySelector(`[data-ticket="${id}"]`);
const agentEl = (id: string) => document.querySelector(`[data-agent-face="${id}"]`) ?? document.querySelector(`[data-agent-card="${id}"]`);
const later = (fn: () => void, ms = 60) => setTimeout(() => requestAnimationFrame(fn), ms);

interface Banner {
  id: number;
  agentId: string;
  level: number;
}

export function FxLayer() {
  const layer = useRef<HTMLDivElement>(null);
  const [banners, setBanners] = useState<Banner[]>([]);

  useEffect(() => {
    const add = <T extends HTMLElement>(el: T) => {
      layer.current?.appendChild(el);
      return el;
    };

    const fly = (src: string, from: Rect, to: Rect, size = 48, ms = 650) => {
      const img = add(Object.assign(document.createElement('img'), { src, className: 'fly', alt: '' }));
      img.style.width = img.style.height = `${size}px`;
      const a = center(from);
      const b = center(to);
      const x0 = a.x - size / 2;
      const y0 = a.y - size / 2;
      const x1 = b.x - size / 2;
      const y1 = b.y - size / 2;
      const anim = img.animate(
        [
          { transform: `translate(${x0}px, ${y0}px) scale(1)` },
          { transform: `translate(${(x0 + x1) / 2}px, ${Math.min(y0, y1) - 110}px) scale(1.5) rotate(-200deg)` },
          { transform: `translate(${x1}px, ${y1}px) scale(.8) rotate(-360deg)` },
        ],
        { duration: ms, easing: 'cubic-bezier(.3,.7,.4,1)' },
      );
      anim.onfinish = () => img.remove();
      return anim;
    };

    const confetti = (at: { x: number; y: number }, n = 36, spread = 1) => {
      for (let i = 0; i < n; i++) {
        const d = add(document.createElement('i'));
        d.className = 'confetti';
        d.style.background = CONFETTI[i % CONFETTI.length] as string;
        const ang = Math.random() * Math.PI * 2;
        const v = (80 + Math.random() * 160) * spread;
        const dx = Math.cos(ang) * v;
        const dy = Math.sin(ang) * v - 120 * spread;
        const rot = Math.random() * 720 - 360;
        const anim = d.animate(
          [
            { transform: `translate(${at.x}px, ${at.y}px) rotate(0)`, opacity: 1 },
            { transform: `translate(${at.x + dx}px, ${at.y + dy}px) rotate(${rot / 2}deg)`, opacity: 1, offset: 0.45 },
            { transform: `translate(${at.x + dx * 1.2}px, ${at.y + dy + 260}px) rotate(${rot}deg)`, opacity: 0 },
          ],
          { duration: 1300 + Math.random() * 600, easing: 'cubic-bezier(.2,.7,.4,1)' },
        );
        anim.onfinish = () => d.remove();
      }
    };

    const floatText = (text: string, at: { x: number; y: number }, cls = '') => {
      const d = add(document.createElement('div'));
      d.className = `float-text ${cls}`;
      d.textContent = text;
      const anim = d.animate(
        [
          { transform: `translate(${at.x - 30}px, ${at.y}px) scale(.6)`, opacity: 0 },
          { transform: `translate(${at.x - 30}px, ${at.y - 20}px) scale(1.15)`, opacity: 1, offset: 0.2 },
          { transform: `translate(${at.x - 30}px, ${at.y - 80}px) scale(1)`, opacity: 0 },
        ],
        { duration: 1500, easing: 'ease-out' },
      );
      anim.onfinish = () => d.remove();
    };

    const slam = (r: Rect, text: string, tone: 'ok' | 'bad' | 'fail') => {
      const d = add(document.createElement('div'));
      d.className = `slam ${tone}`;
      Object.assign(d.style, { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` });
      const s = document.createElement('span');
      s.textContent = text;
      d.appendChild(s);
      const anim = s.animate(
        [
          { transform: 'rotate(-10deg) scale(3)', opacity: 0 },
          { transform: 'rotate(-10deg) scale(.92)', opacity: 1, offset: 0.18 },
          { transform: 'rotate(-10deg) scale(1)', opacity: 1, offset: 0.26 },
          { transform: 'rotate(-10deg) scale(1)', opacity: 1, offset: 0.8 },
          { transform: 'rotate(-10deg) scale(1.05)', opacity: 0 },
        ],
        { duration: 1700, easing: 'ease-out' },
      );
      anim.onfinish = () => d.remove();
    };

    const pearlsTo = (from: Rect | null, amount: number) => {
      const counter = rectOf(document.getElementById('pearl-counter'));
      if (!counter) return;
      const origin = from ?? { x: innerWidth / 2 - 20, y: innerHeight / 2 - 20, w: 40, h: 40 };
      const n = Math.min(12, Math.max(4, Math.round(amount / 3)));
      for (let i = 0; i < n; i++) {
        const img = add(Object.assign(document.createElement('img'), { src: PEARL, className: 'fly', alt: '' }));
        img.style.width = img.style.height = '26px';
        const c = center(origin);
        const x0 = c.x + (Math.random() - 0.5) * 80;
        const y0 = c.y + (Math.random() - 0.5) * 40;
        const anim = img.animate(
          [
            { transform: `translate(${x0}px, ${y0}px) scale(.4)`, opacity: 0 },
            { transform: `translate(${x0 + (Math.random() - 0.5) * 140}px, ${y0 - 60 - Math.random() * 70}px) scale(1.15)`, opacity: 1, offset: 0.35 },
            { transform: `translate(${counter.x + 8}px, ${counter.y + 4}px) scale(.8)`, opacity: 1 },
          ],
          { duration: 900 + i * 70, easing: 'ease-in' },
        );
        anim.onfinish = () => {
          img.remove();
          const p = document.getElementById('pearl-counter');
          p?.classList.remove('bump');
          void p?.offsetWidth;
          p?.classList.add('bump');
          if (i % 3 === 0) sfx('pearl', 0.35);
        };
      }
    };

    const name = (id: string | undefined) => (id ? useBoard.getState().agents[id]?.name : undefined) ?? 'A boba';
    const tnum = (id: string) => useBoard.getState().tickets[id]?.number;
    const spriteOf = (id: string | undefined) => (id ? useBoard.getState().agents[id]?.sprite : undefined);

    const handle = (f: LocalFx) => {
      const calm = reducedMotion();
      switch (f.fx) {
        case 'assigned': {
          sfx('pearl', 0.5);
          if (calm || f.via === 'drag') return;
          const from = rectOf(agentEl(f.agent_id));
          const to = rectOf(ticketEl(f.ticket_id));
          if (from && to) fly(bodyUrl(spriteOf(f.agent_id)), from, to, 56);
          return;
        }
        case 'recruited':
          later(() => {
            const r = rectOf(document.querySelector(`[data-agent-card="${f.agent_id}"]`));
            if (r && !calm) confetti(center(r), 30, 0.6);
          }, 150);
          return;
        case 'work_finished':
          later(() => {
            const r = rectOf(ticketEl(f.ticket_id));
            if (r && !calm) confetti(center(r), 28, 0.7);
          });
          sfx('pearl');
          toast(`${name(f.agent_id)} finished T-${tnum(f.ticket_id) ?? '?'} and opened a PR 📬`, 'good', spriteOf(f.agent_id));
          return;
        case 'xp_gained':
          if (calm) return;
          later(() => {
            const r = rectOf(agentEl(f.agent_id));
            if (r) floatText(`+${f.amount} XP`, { x: r.x + r.w / 2, y: r.y }, 'xp');
          }, 250);
          return;
        case 'level_up':
          sfx('done');
          setBanners((b) => [...b, { id: Date.now() + Math.random(), agentId: f.agent_id, level: f.level }]);
          if (!calm) later(() => confetti({ x: innerWidth / 2, y: innerHeight * 0.3 }, 70, 1.4), 200);
          return;
        case 'pr_approved':
          sfx('done', 0.8);
          later(() => {
            const r = rectOf(ticketEl(f.ticket_id));
            if (r) {
              slam(r, 'APPROVED', 'ok');
              if (!calm) confetti(center(r), 48, 1);
            }
          }, 120);
          toast(`${name(f.reviewer_id)} approved T-${tnum(f.ticket_id) ?? '?'}! 🧋`, 'gold', spriteOf(f.reviewer_id));
          return;
        case 'pearls_earned':
          if (calm) return;
          later(() => pearlsTo(rectOf(ticketEl(f.ticket_id)), f.amount), 450);
          return;
        case 'changes_requested':
          sfx('blocked');
          later(() => {
            const r = rectOf(ticketEl(f.ticket_id));
            if (r) slam(r, 'CHANGES REQUESTED', 'bad');
          }, 120);
          toast(`${name(f.reviewer_id)} sent T-${tnum(f.ticket_id) ?? '?'} back with feedback`, 'bad', spriteOf(f.reviewer_id));
          return;
        case 'run_failed':
          sfx('timeout');
          later(() => {
            const r = rectOf(ticketEl(f.ticket_id));
            if (r) slam(r, 'RUN FAILED', 'fail');
          }, 120);
          toast(`${name(f.agent_id)} got stuck on T-${tnum(f.ticket_id) ?? '?'}`, 'bad', spriteOf(f.agent_id));
          return;
        case 'blocked':
          return;
      }
    };
    return fxBus.on(handle);
  }, []);

  useEffect(() => {
    if (banners.length === 0) return;
    const id = setTimeout(() => setBanners((b) => b.slice(1)), 2600);
    return () => clearTimeout(id);
  }, [banners]);

  const banner = banners[0];
  const agent = useBoard((s) => (banner ? s.agents[banner.agentId] : undefined));

  return (
    <>
      <div className="fx-layer" ref={layer} aria-hidden="true" />
      <AnimatePresence>
        {banner && (
          <motion.div
            key={banner.id}
            className="levelup-banner wood"
            role="status"
            initial={{ opacity: 0, scale: 0.4, y: -40, x: '-50%' }}
            animate={{ opacity: 1, scale: 1, y: 0, x: '-50%' }}
            exit={{ opacity: 0, scale: 0.8, y: -30, x: '-50%' }}
            transition={{ type: 'spring', stiffness: 380, damping: 18 }}
          >
            <span className="rays" />
            {agent && <Sprite sprite={agent.sprite} color={agent.color} size={110} body />}
            <h2>LEVEL UP!</h2>
            <p>
              {agent?.name ?? 'A boba'} reached level {banner.level}
            </p>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}

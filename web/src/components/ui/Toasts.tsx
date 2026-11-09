import { AnimatePresence, motion } from 'motion/react';
import { useEffect, useState } from 'react';
import { bodyUrl } from '../../lib/game';
import { toastBus, type ToastMsg } from '../../store/bus';

export function Toasts() {
  const [items, setItems] = useState<ToastMsg[]>([]);
  useEffect(
    () =>
      toastBus.on((t) => {
        setItems((xs) => [...xs.slice(-3), t]);
        setTimeout(() => setItems((xs) => xs.filter((x) => x.id !== t.id)), t.tone === 'bad' ? 4200 : 3000);
      }),
    [],
  );
  return (
    <div className="toasts" role="status" aria-live="polite">
      <AnimatePresence initial={false}>
        {items.map((t) => (
          <motion.div
            key={t.id}
            layout
            className={`toast ${t.tone}`}
            initial={{ opacity: 0, scale: 0.6, y: 10 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, y: 10, transition: { duration: 0.25 } }}
            transition={{ type: 'spring', stiffness: 500, damping: 28 }}
          >
            {t.sprite && <img src={bodyUrl(t.sprite)} alt="" />}
            <span>{t.text}</span>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}

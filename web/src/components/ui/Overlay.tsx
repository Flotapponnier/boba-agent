import { motion } from 'motion/react';
import { useEffect, useRef, type ReactNode } from 'react';
import { Icon } from './Icon';

function useEscape(onClose: () => void) {
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener('keydown', h);
    return () => window.removeEventListener('keydown', h);
  }, [onClose]);
}

/** Restores focus to whatever was focused when the overlay opened. */
function useFocusReturn(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    const el = ref.current;
    const first = el?.querySelector<HTMLElement>('[autofocus], input:not([type=hidden]), textarea, select, button:not(.close-x)');
    (first ?? el)?.focus({ preventScroll: true });
    return () => prev?.focus?.({ preventScroll: true });
  }, [ref]);
}

export function Modal({ title, onClose, children, wide, footer }: { title: string; onClose: () => void; children: ReactNode; wide?: boolean; footer?: ReactNode }) {
  useEscape(onClose);
  const ref = useRef<HTMLDivElement>(null);
  useFocusReturn(ref);
  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <motion.div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        className={`modal wood ${wide ? 'wide' : ''}`}
        initial={{ opacity: 0, scale: 0.92, y: 20 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        transition={{ type: 'spring', stiffness: 420, damping: 30 }}
      >
        <div className="modal-head">
          <div className="sign">{title}</div>
          <button className="icon-btn close-x" onClick={onClose} aria-label="Close">
            <Icon name="close" size={16} />
          </button>
        </div>
        <div className="sheet">{children}</div>
        {footer && <div className="row end" style={{ paddingTop: 10 }}>{footer}</div>}
      </motion.div>
    </div>
  );
}

export function Drawer({ title, onClose, children, head }: { title: string; onClose: () => void; children: ReactNode; head?: ReactNode }) {
  useEscape(onClose);
  const ref = useRef<HTMLElement>(null);
  useFocusReturn(ref);
  return (
    <>
      <motion.div className="drawer-scrim" onClick={onClose} initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} />
      <motion.aside
        ref={ref}
        className="drawer wood"
        role="dialog"
        aria-label={title}
        tabIndex={-1}
        initial={{ x: '105%' }}
        animate={{ x: 0 }}
        exit={{ x: '105%' }}
        transition={{ type: 'spring', stiffness: 380, damping: 38 }}
      >
        <div className="drawer-head">
          <div className="sign">{title}</div>
          {head}
          <button className="icon-btn close-x" onClick={onClose} aria-label="Close">
            <Icon name="close" size={16} />
          </button>
        </div>
        <div className="drawer-body">{children}</div>
      </motion.aside>
    </>
  );
}

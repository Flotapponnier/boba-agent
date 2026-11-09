import { useEffect, useRef, useState } from 'react';
import { Markdown } from '../ui/Markdown';
import { Icon } from '../ui/Icon';

type HighlightCtor = new (...ranges: Range[]) => unknown;
const highlights = (): { set(name: string, h: unknown): void; delete(name: string): void } | null => {
  const c = (globalThis as unknown as { CSS?: { highlights?: { set(n: string, h: unknown): void; delete(n: string): void } } }).CSS;
  return c?.highlights && 'Highlight' in globalThis ? c.highlights : null;
};

/** Find `quote` in the rendered text of `root` and return a DOM Range over it (first match). */
function findRange(root: HTMLElement, quote: string): Range | null {
  const needle = quote.replace(/\s+/g, ' ').trim().toLowerCase();
  if (!needle) return null;
  const nodes: { node: Text; start: number }[] = [];
  let full = '';
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    nodes.push({ node: n as Text, start: full.length });
    full += (n.textContent ?? '').replace(/\s+/g, ' ');
  }
  const at = full.toLowerCase().indexOf(needle);
  if (at < 0) return null;
  const end = at + needle.length;
  const locate = (pos: number) => {
    for (let i = nodes.length - 1; i >= 0; i--) {
      const n = nodes[i];
      if (n && n.start <= pos) return { node: n.node, offset: Math.min(pos - n.start, n.node.length) };
    }
    return null;
  };
  const a = locate(at);
  const b = locate(end);
  if (!a || !b) return null;
  const r = document.createRange();
  r.setStart(a.node, a.offset);
  r.setEnd(b.node, b.offset);
  return r;
}

/**
 * Ticket body you can comment on: select text → "Comment" bubble. Passages that already have a
 * thread are highlighted inline (CSS Custom Highlight API, no DOM mutation).
 */
export function QuotableBody({ text, quotes, onQuote, canComment }: { text: string; quotes: string[]; onQuote: (q: string) => void; canComment: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const [bubble, setBubble] = useState<{ x: number; y: number; q: string } | null>(null);

  useEffect(() => {
    const hl = highlights();
    const root = ref.current;
    if (!hl || !root) return;
    const ranges = quotes.map((q) => findRange(root, q)).filter((r): r is Range => !!r);
    const Ctor = (globalThis as unknown as { Highlight: HighlightCtor }).Highlight;
    hl.set('boba-quote', new Ctor(...ranges));
    return () => hl.delete('boba-quote');
  }, [quotes, text]);

  const onUp = () => {
    if (!canComment) return;
    const sel = window.getSelection();
    const root = ref.current;
    if (!sel || sel.isCollapsed || !root || !root.contains(sel.anchorNode) || !root.contains(sel.focusNode)) {
      setBubble(null);
      return;
    }
    const q = sel.toString().replace(/\s+/g, ' ').trim();
    if (q.length < 3) return setBubble(null);
    const r = sel.getRangeAt(0).getBoundingClientRect();
    const box = root.getBoundingClientRect();
    setBubble({ x: r.left - box.left + r.width / 2, y: r.top - box.top, q: q.slice(0, 400) });
  };

  return (
    <div className="quotable" ref={ref} onMouseUp={onUp} onKeyUp={onUp}>
      <Markdown text={text} />
      {bubble && (
        <button
          className="quote-bubble"
          style={{ left: bubble.x, top: bubble.y }}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            onQuote(bubble.q);
            setBubble(null);
            window.getSelection()?.removeAllRanges();
          }}
        >
          <Icon name="chat" size={12} /> Comment
        </button>
      )}
      {quotes.length > 0 && <span className="quote-count" title="Passages with a thread are highlighted"><Icon name="chat" size={12} /> {quotes.length}</span>}
    </div>
  );
}

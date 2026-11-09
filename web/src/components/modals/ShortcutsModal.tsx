import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';

const KEYS: [string, string][] = [
  ['⌘K / Ctrl+K', 'Command bar: jump anywhere or ask the Concierge'],
  ['Hold Space', 'Talk to the Concierge (voice), release to send'],
  ['N', 'New ticket in this tab'],
  ['A', 'Recruit an agent'],
  ['H', 'Timeline (history)'],
  ['K', 'Skill library'],
  ['Q / I / P', 'Quest board / Integrations / Portals'],
  ['M', 'Music on / off'],
  ['1 – 9', 'Switch tab'],
  ['Enter', 'On an agent: pick it. On a ticket: open it (or assign the picked agent)'],
  ['Space', 'On a focused ticket: lift it, then arrows to move, Space to drop'],
  ['Esc', 'Close the panel / cancel the pick'],
  ['?', 'This help'],
];

export function ShortcutsModal() {
  const show = useUi((u) => u.show);
  return (
    <Modal title="KEYBOARD" onClose={() => show(null)}>
      <dl className="kv" style={{ gridTemplateColumns: '130px 1fr' }}>
        {KEYS.map(([k, v]) => (
          <div key={k} style={{ display: 'contents' }}>
            <dt>
              <kbd>{k}</kbd>
            </dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
    </Modal>
  );
}

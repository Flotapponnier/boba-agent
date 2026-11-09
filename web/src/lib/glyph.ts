import { BRANDS } from './brands';
import { ICON_PATHS, type IconName } from './icons';

/** Emoji stored by earlier versions (tabs, quests) mapped onto the icon set. */
const LEGACY: Record<string, string> = {
  '🧋': 'boba', '🔎': 'search', '🔍': 'search', '🛠': 'tools', '🔧': 'tools', '🎨': 'palette', '🧪': 'beaker',
  '📣': 'megaphone', '📈': 'chart', '🐛': 'bug', '🚀': 'speed', '📚': 'book', '🔒': 'lock', '🌍': 'globe',
  '📜': 'quest', '🛡': 'shield', '🚨': 'siren', '📋': 'clipboard', '🧹': 'eraser', '⚔': 'sword', '🐙': 'brand:github',
  '📧': 'brand:gmail', '📐': 'brand:linear', '💬': 'slack', '📓': 'brand:notion', '🐘': 'brand:postgres', '🌀': 'portal',
  '🦀': 'brand:rust', '🟩': 'brand:node', '🐍': 'brand:python', '🐹': 'brand:go', '🌿': 'brand:git',
};

export function isIconName(v: string): v is IconName {
  return Object.prototype.hasOwnProperty.call(ICON_PATHS, v);
}

/** Normalised icon value: `icon-name`, `brand:<name>`, or null when it is free text. */
export function glyphKey(value: string | null | undefined): string | null {
  if (!value) return null;
  const v = value.replace(/️/g, '').trim();
  const k = LEGACY[v] ?? v;
  if (k.startsWith('brand:') && k.slice(6) in BRANDS) return k;
  return isIconName(k) ? k : null;
}

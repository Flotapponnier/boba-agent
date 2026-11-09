import type { ColumnKind, Permission, Priority, Role } from '../api/types';

/** Base URL of static assets (works under any sub-path thanks to Vite's relative base). */
export const asset = (p: string) => `${import.meta.env.BASE_URL}${p}`;

export interface SpriteDef {
  key: string;
  label: string;
  avatar: string;
  body: string;
}

/** Mirrors `SPRITES` in boba-core/src/model.rs. */
export const SPRITES: SpriteDef[] = [
  { key: 'boba', label: 'Boba', avatar: 'sprites/avatar/boba_avatar.webp', body: 'sprites/character/boba.webp' },
  { key: 'boba_uncle', label: 'Uncle', avatar: 'sprites/avatar/boba_uncle.webp', body: 'sprites/avatar/boba_uncle.webp' },
  { key: 'pinky_boba', label: 'Pinky', avatar: 'sprites/avatar/pinky_boba_avatar.webp', body: 'sprites/character/pinky_boba.webp' },
  { key: 'boba_diamond', label: 'Diamond', avatar: 'sprites/avatar/diamond_boba_avatar.webp', body: 'sprites/character/boba_diamond.webp' },
  { key: 'golden_boba', label: 'Golden', avatar: 'sprites/avatar/golden_boba_avatar.webp', body: 'sprites/character/golden_boba.webp' },
  { key: 'black_boba', label: 'Black', avatar: 'sprites/avatar/black_boba_avatar.webp', body: 'sprites/character/black_boba.webp' },
];

const fallback = SPRITES[0] as SpriteDef;
export const sprite = (key: string | null | undefined): SpriteDef =>
  SPRITES.find((s) => s.key === key) ?? fallback;
export const avatarUrl = (key: string | null | undefined) => asset(sprite(key).avatar);
export const bodyUrl = (key: string | null | undefined) => asset(sprite(key).body);
export const PEARL = asset('sprites/character/pearl.webp');

/** Mirrors `team::USER_AVATARS`: humans get square portraits, agents full-body sprites. */
export const USER_AVATARS = ['boba_avatar', 'pinky_boba_avatar', 'golden_boba_avatar', 'black_boba_avatar', 'diamond_boba_avatar', 'boba_uncle', 'uncle_song'];
/** Portrait of a human. Accepts portrait keys and legacy sprite keys. */
export const portraitUrl = (key: string | null | undefined) =>
  key && USER_AVATARS.includes(key) ? asset(`sprites/avatar/${key}.webp`) : avatarUrl(key);

/** Mirrors the server: `Agent.xp` is XP inside the current level. */
export const xpForNextLevel = (level: number) => 100 + 50 * (Math.max(1, level) - 1);
export const xpPct = (xp: number, level: number) =>
  Math.max(0, Math.min(100, (xp / xpForNextLevel(level)) * 100));

/** Mirrors `Role::grants` in boba-core/src/model.rs. */
export function grants(role: Role | undefined | null, p: Permission): boolean {
  switch (role) {
    case 'owner':
    case 'admin':
      return true;
    case 'member':
      return p === 'view' || p === 'edit_tickets' || p === 'run_agents' || p === 'manage_skills';
    case 'viewer':
      return p === 'view';
    default:
      return false;
  }
}

export const KIND_COLORS: Record<ColumnKind, string> = {
  backlog: '#95a5a6',
  todo: '#e74c3c',
  doing: '#f39c12',
  review: '#3498db',
  done: '#2ecc71',
  custom: '#9b59b6',
};
export const KIND_LABEL: Record<ColumnKind, string> = {
  backlog: 'Backlog',
  todo: 'To do',
  doing: 'Doing',
  review: 'Review',
  done: 'Done',
  custom: 'Custom',
};

export const PRIORITIES: Priority[] = ['low', 'normal', 'high', 'urgent'];
export const PRIORITY_LABEL: Record<Priority, string> = { low: 'Low', normal: 'Normal', high: 'High', urgent: 'Urgent' };

export const ROLE_PRESETS = ['Dev', 'Reviewer', 'SEO', 'QA', 'DevOps', 'Architect', 'Designer', 'Writer'];

export const MODELS: { id: string | null; label: string }[] = [
  { id: null, label: 'Claude Code default' },
  { id: 'claude-opus-5-5', label: 'Opus 5.5 (deepest)' },
  { id: 'claude-sonnet-5-5', label: 'Sonnet 5.5 (balanced)' },
  { id: 'claude-haiku-5-5', label: 'Haiku 5.5 (fastest)' },
  { id: 'claude-fable-5-1', label: 'Fable 5.1' },
];

/** One-click `--allowedTools` presets so test/build commands don't stop on a permission prompt. */
export const COMMAND_PRESETS: { stack: string; icon: string; tools: string[] }[] = [
  { stack: 'Rust', icon: '🦀', tools: ['Bash(cargo test:*)', 'Bash(cargo build:*)', 'Bash(cargo clippy:*)', 'Bash(cargo fmt:*)'] },
  { stack: 'Node', icon: '🟩', tools: ['Bash(pnpm test:*)', 'Bash(pnpm lint:*)', 'Bash(pnpm build:*)', 'Bash(pnpm typecheck:*)', 'Bash(npm test:*)', 'Bash(npx vitest:*)'] },
  { stack: 'Python', icon: '🐍', tools: ['Bash(python3 -m pytest:*)', 'Bash(pytest:*)', 'Bash(ruff check:*)', 'Bash(mypy:*)'] },
  { stack: 'Go', icon: '🐹', tools: ['Bash(go test:*)', 'Bash(go build:*)', 'Bash(go vet:*)'] },
  { stack: 'Git (read)', icon: '🌿', tools: ['Bash(git diff:*)', 'Bash(git status:*)', 'Bash(git log:*)'] },
];

/** `cargo test --all` → `Bash(cargo test:*)` (first two words, or one for single-word commands). */
export function commandPattern(command: string): string | null {
  const words = command.trim().split(/\s+/).filter((w) => !/^[A-Z_]+=/.test(w));
  if (!words.length || /[;&|`$<>]/.test(words.slice(0, 2).join(' '))) return null;
  const head = words[1] && !words[1].startsWith('-') ? `${words[0]} ${words[1]}` : words[0];
  return `Bash(${head}:*)`;
}

/** The built-in Concierge (role "Concierge") lives in Snapshot.agents but is not a draggable boba. */
export const isConcierge = (a: { role: string } | null | undefined) => !!a && a.role.trim().toLowerCase() === 'concierge';

export const SWATCHES = ['#f39c12', '#e74c3c', '#ff6fb5', '#9b59b6', '#3498db', '#1abc9c', '#2ecc71', '#ffd23f', '#8b4513', '#34495e'];

/** Stable pseudo-random rotation in [-2, 2] degrees for a ticket "paper". */
export function paperTilt(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return ((Math.abs(h) % 400) / 100 - 2) * 0.9;
}

/** Fractional position between two neighbours (server keeps whatever we send). */
export function positionBetween(before: number | undefined, after: number | undefined): number {
  if (before === undefined && after === undefined) return 1024;
  if (before === undefined) return (after as number) - 1024;
  if (after === undefined) return before + 1024;
  return (before + after) / 2;
}

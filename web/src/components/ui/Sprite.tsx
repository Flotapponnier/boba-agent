import type { CSSProperties } from 'react';
import type { AgentStatus } from '../../api/types';
import { avatarUrl, bodyUrl } from '../../lib/game';

/** A boba character. Working agents spin-hop inside a rotating aura in their colour. */
export function Sprite({
  sprite,
  color,
  status = 'idle',
  size = 40,
  body = true,
  className = '',
}: {
  sprite: string;
  color?: string;
  status?: AgentStatus;
  size?: number;
  body?: boolean;
  className?: string;
}) {
  const busy = status === 'working' || status === 'reviewing';
  const state = busy ? 'working' : status === 'waiting' ? 'waiting' : status === 'error' ? 'error' : 'idle';
  return (
    <span className={`sprite ${state} ${className}`} style={{ width: size, height: size, '--ac': color ?? '#f39c12' } as CSSProperties}>
      {(busy || status === 'waiting') && <span className="aura" />}
      <img src={body ? bodyUrl(sprite) : avatarUrl(sprite)} alt="" draggable={false} />
    </span>
  );
}

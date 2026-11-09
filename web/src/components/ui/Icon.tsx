import type { CSSProperties } from 'react';
import { BRANDS, type BrandName } from '../../lib/brands';
import { glyphKey } from '../../lib/glyph';
import { ICON_PATHS, type IconName } from '../../lib/icons';

interface Common {
  size?: number;
  className?: string;
  /** Accessible label. Without one the icon is decorative (`aria-hidden`). */
  title?: string;
  style?: CSSProperties;
}

function a11y(title?: string) {
  return title ? { role: 'img', 'aria-label': title } : { 'aria-hidden': true as const };
}

/** A pixel icon (24×24 grid), drawn in `currentColor`. */
export function Icon({ name, size = 16, className, title, style }: Common & { name: IconName }) {
  return (
    <svg
      className={`icon ${className ?? ''}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      shapeRendering="crispEdges"
      style={style}
      {...a11y(title)}
    >
      {title && <title>{title}</title>}
      <path d={ICON_PATHS[name]} />
    </svg>
  );
}

/** A brand mark, in its brand colour or in `currentColor` (`mono`). */
export function Brand({ name, size = 16, className, title, style, mono }: Common & { name: BrandName; mono?: boolean }) {
  const b = BRANDS[name];
  return (
    <svg
      className={`icon brand ${className ?? ''}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={mono ? 'currentColor' : b.hex}
      style={style}
      {...a11y(title ?? undefined)}
    >
      {title && <title>{title}</title>}
      <path d={b.d} />
    </svg>
  );
}

/**
 * Renders a user-chosen icon (tab, quest, portal): an icon name, `brand:<name>`, a legacy emoji, or
 * any other short text as-is.
 */
export function Glyph({ value, size = 16, className, mono, fallback = 'boba' }: Common & { value: string | null | undefined; mono?: boolean; fallback?: IconName }) {
  const k = glyphKey(value);
  if (k?.startsWith('brand:')) return <Brand name={k.slice(6) as BrandName} size={size} className={className} mono={mono} />;
  if (k) return <Icon name={k as IconName} size={size} className={className} />;
  if (value && value.trim()) return <span className={`glyph-text ${className ?? ''}`} aria-hidden="true">{value}</span>;
  return <Icon name={fallback} size={size} className={className} />;
}

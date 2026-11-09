import type { Schedule } from '../api/types';

export const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const DAYS_LONG = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

export const localTz = () => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
};

export function timezones(): string[] {
  try {
    const fn = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf;
    if (fn) return fn('timeZone');
  } catch {
    /* old browser */
  }
  return ['UTC', 'Europe/Paris', 'Europe/London', 'America/New_York', 'America/Los_Angeles', 'Asia/Tokyo', 'Asia/Singapore'];
}

/** The friendly schedule builder's model. */
export type Builder =
  | { mode: 'manual' }
  | { mode: 'interval'; minutes: number }
  | { mode: 'daily'; time: string; tz: string }
  | { mode: 'weekly'; days: number[]; time: string; tz: string }
  | { mode: 'cron'; expr: string; tz: string };

const pad = (n: number) => String(n).padStart(2, '0');

export function toSchedule(b: Builder): Schedule {
  switch (b.mode) {
    case 'manual':
      return { type: 'manual' };
    case 'interval':
      return { type: 'interval', minutes: Math.max(5, Math.round(b.minutes)) };
    case 'daily': {
      const [h, m] = b.time.split(':').map(Number);
      return { type: 'cron', expr: `${m ?? 0} ${h ?? 9} * * *`, timezone: b.tz };
    }
    case 'weekly': {
      const [h, m] = b.time.split(':').map(Number);
      const days = [...new Set(b.days)].sort().join(',') || '1';
      return { type: 'cron', expr: `${m ?? 0} ${h ?? 9} * * ${days}`, timezone: b.tz };
    }
    case 'cron':
      return { type: 'cron', expr: b.expr.trim(), timezone: b.tz };
  }
}

export function fromSchedule(s: Schedule | undefined): Builder {
  if (!s) return { mode: 'weekly', days: [1], time: '09:00', tz: localTz() };
  if (s.type === 'manual') return { mode: 'manual' };
  if (s.type === 'interval') return { mode: 'interval', minutes: s.minutes };
  const f = s.expr.trim().split(/\s+/);
  if (f.length === 5 && /^\d+$/.test(f[0] ?? '') && /^\d+$/.test(f[1] ?? '') && f[2] === '*' && f[3] === '*') {
    const time = `${pad(Number(f[1]))}:${pad(Number(f[0]))}`;
    if (f[4] === '*') return { mode: 'daily', time, tz: s.timezone };
    if (/^[0-6](,[0-6])*$/.test(f[4] ?? '')) return { mode: 'weekly', days: (f[4] as string).split(',').map(Number), time, tz: s.timezone };
  }
  return { mode: 'cron', expr: s.expr, tz: s.timezone };
}

export function describe(s: Schedule): string {
  if (s.type === 'manual') return 'Only when you press Run';
  if (s.type === 'interval') {
    const m = s.minutes;
    if (m % 1440 === 0) return m === 1440 ? 'Every day' : `Every ${m / 1440} days`;
    if (m % 60 === 0) return m === 60 ? 'Every hour' : `Every ${m / 60} hours`;
    return `Every ${m} minutes`;
  }
  const b = fromSchedule(s);
  const tz = s.timezone === localTz() ? '' : ` (${s.timezone})`;
  if (b.mode === 'daily') return `Every day at ${b.time}${tz}`;
  if (b.mode === 'weekly') {
    const d = [...b.days].sort();
    const names = d.join(',') === '1,2,3,4,5' ? 'weekday' : d.map((x) => DAYS_LONG[x]).join(', ');
    return `Every ${names} at ${b.time}${tz}`;
  }
  return `Cron ${s.expr}${tz}`;
}

// ------------------------------------------------------------ cron evaluation (mock + offline preview)

function parseField(f: string, min: number, max: number): Set<number> | null {
  const out = new Set<number>();
  for (const part of f.split(',')) {
    const m = part.match(/^(\*|\d+(?:-\d+)?)(?:\/(\d+))?$/);
    if (!m) return null;
    let lo = min;
    let hi = max;
    if (m[1] !== '*') {
      const [a, b] = (m[1] as string).split('-').map(Number);
      lo = a as number;
      hi = b ?? (m[2] ? max : (a as number));
    }
    const step = m[2] ? Number(m[2]) : 1;
    if (lo < min || hi > max || step < 1) return null;
    for (let v = lo; v <= hi; v += step) out.add(v === 7 && max === 7 ? 0 : v);
  }
  return out;
}

function wallParts(d: Date, tz: string) {
  const p = new Intl.DateTimeFormat('en-US', { timeZone: tz, hour12: false, year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', weekday: 'short' }).formatToParts(d);
  const g = (t: string) => p.find((x) => x.type === t)?.value ?? '0';
  return { month: Number(g('month')), day: Number(g('day')), hour: Number(g('hour')) % 24, minute: Number(g('minute')), dow: DAYS.indexOf(g('weekday')) };
}

export function validateCron(expr: string): string | null {
  const f = expr.trim().split(/\s+/);
  if (f.length !== 5) return 'A cron expression has 5 fields: minute hour day month weekday';
  const ranges: [number, number][] = [
    [0, 59],
    [0, 23],
    [1, 31],
    [1, 12],
    [0, 7],
  ];
  for (let i = 0; i < 5; i++) if (!parseField(f[i] as string, ...(ranges[i] as [number, number]))) return `Field ${i + 1} ("${f[i]}") is not valid`;
  return null;
}

/** Next fire times. Brute force by minute (skipping whole hours), fine for previews. */
export function nextRuns(s: Schedule, from = new Date(), n = 5): Date[] {
  if (s.type === 'manual') return [];
  if (s.type === 'interval') return Array.from({ length: n }, (_, i) => new Date(from.getTime() + (i + 1) * s.minutes * 60_000));
  if (validateCron(s.expr)) return [];
  const f = s.expr.trim().split(/\s+/);
  const mins = parseField(f[0] as string, 0, 59) as Set<number>;
  const hours = parseField(f[1] as string, 0, 23) as Set<number>;
  const doms = parseField(f[2] as string, 1, 31) as Set<number>;
  const months = parseField(f[3] as string, 1, 12) as Set<number>;
  const dows = parseField(f[4] as string, 0, 7) as Set<number>;
  const out: Date[] = [];
  let t = Math.ceil((from.getTime() + 1) / 60_000) * 60_000;
  const limit = t + 400 * 24 * 3600_000;
  while (out.length < n && t < limit) {
    const w = wallParts(new Date(t), s.timezone);
    if (!hours.has(w.hour) || !months.has(w.month) || !(f[2] === '*' || doms.has(w.day)) || !(f[4] === '*' || dows.has(w.dow))) {
      t += (60 - w.minute) * 60_000; // jump to the next hour
      continue;
    }
    if (mins.has(w.minute)) out.push(new Date(t));
    t += 60_000;
  }
  return out;
}

export function countdown(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return '—';
  let s = Math.round((new Date(iso).getTime() - now) / 1000);
  if (s <= 0) return 'any moment';
  const d = Math.floor(s / 86400);
  s -= d * 86400;
  const h = Math.floor(s / 3600);
  s -= h * 3600;
  const m = Math.floor(s / 60);
  s -= m * 60;
  if (d) return `${d}d ${h}h ${m}m`;
  if (h) return `${h}h ${pad(m)}m ${pad(s)}s`;
  return `${m}m ${pad(s)}s`;
}

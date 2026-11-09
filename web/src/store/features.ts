import { create } from 'zustand';

/** Server features that may not exist yet on an older/partial server (route missing → 404 "no such endpoint" or 501). */
export type Feature = 'github-manifest' | 'github' | 'jobs' | 'command' | 'comments' | 'connectors' | 'sync' | 'runners' | 'notifications' | 'profile' | 'speech' | 'skills' | 'repos';

const RULES: [RegExp, Feature][] = [
  [/^\/api\/github\/app\/manifest/, 'github-manifest'],
  [/^\/api\/github/, 'github'],
  [/^\/api\/jobs/, 'jobs'],
  [/^\/api\/command/, 'command'],
  [/^\/api\/comments|^\/api\/tickets\/[^/]+\/comments/, 'comments'],
  [/^\/api\/connectors/, 'connectors'],
  [/^\/api\/repos\/[^/]+\/sync|^\/api\/repos\/[^/]+$/, 'sync'],
  [/^\/api\/runners/, 'runners'],
  [/^\/api\/notifications/, 'notifications'],
  [/^\/api\/me$/, 'profile'],
  [/^\/api\/speech/, 'speech'],
  [/^\/api\/skills/, 'skills'],
];

export const featureOf = (path: string): Feature | null => {
  const p = path.split('?')[0] ?? path;
  return RULES.find(([re]) => re.test(p))?.[1] ?? null;
};

interface FeatureState {
  missing: Partial<Record<Feature, true>>;
  markMissing(f: Feature): void;
}

export const useFeatures = create<FeatureState>()((set) => ({
  missing: {},
  markMissing: (f) => set((s) => (s.missing[f] ? s : { missing: { ...s.missing, [f]: true } })),
}));

/** False once the server told us this feature's endpoint does not exist. */
export const useAvailable = (f: Feature) => useFeatures((s) => !s.missing[f]);
export const UNAVAILABLE = 'Not available on this server yet';

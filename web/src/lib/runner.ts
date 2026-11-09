import type { Run, RunnerInfo, User } from '../api/types';

export interface RunnerLabel {
  real: boolean;
  text: string;
  hint: string;
  /** Runner name, shown next to the label. */
  where: string | null;
  /** Personal runner of this user id, if any. */
  ownerId: string | null;
}

/** "Simulated" vs "Real · Florent's Claude": where a run actually executes. */
export function runnerLabel(run: Pick<Run, 'runner_id'>, runners: Record<string, RunnerInfo>, users: Record<string, User>): RunnerLabel | null {
  const r = run.runner_id ? runners[run.runner_id] : undefined;
  if (!r) return run.runner_id ? null : { real: false, text: 'Queued', hint: 'Waiting for a free runner', where: null, ownerId: null };
  const ownerId = r.scope === 'personal' ? r.owner_user_id : null;
  if (r.auth === 'demo' || r.auth === 'unknown')
    return { real: false, text: 'Simulated', hint: `Runs on ${r.name}, a demo runner: no real Claude Code.`, where: r.name, ownerId };
  if (r.scope === 'personal') {
    const owner = r.owner_user_id ? users[r.owner_user_id]?.name : undefined;
    return { real: true, text: `Real · ${owner ? `${owner}'s` : 'personal'} Claude`, hint: `Runs on ${r.host ?? r.name} with ${owner ?? 'its owner'}'s own Claude Code login`, where: r.name, ownerId };
  }
  return { real: true, text: `Real · ${r.auth === 'api_key' ? 'API key' : r.auth === 'bedrock' ? 'Bedrock' : r.auth === 'vertex' ? 'Vertex' : 'shared'}`, hint: `Runs on the shared runner ${r.name}`, where: r.name, ownerId };
}

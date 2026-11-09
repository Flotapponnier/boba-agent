import type { Run, RunnerInfo, User } from '../api/types';

export interface RunnerLabel {
  real: boolean;
  text: string;
  hint: string;
}

/** "Simulated" vs "Real · Florent's Claude": where a run actually executes. */
export function runnerLabel(run: Pick<Run, 'runner_id'>, runners: Record<string, RunnerInfo>, users: Record<string, User>): RunnerLabel | null {
  const r = run.runner_id ? runners[run.runner_id] : undefined;
  if (!r) return run.runner_id ? null : { real: false, text: 'Queued', hint: 'Waiting for a free runner' };
  if (r.auth === 'demo' || r.auth === 'unknown')
    return { real: false, text: 'Simulated', hint: `Runs on ${r.name}, a demo runner: no real Claude Code. Connect your own Claude Code to run it for real.` };
  if (r.scope === 'personal') {
    const owner = r.owner_user_id ? users[r.owner_user_id]?.name : undefined;
    return { real: true, text: `Real · ${owner ? `${owner}'s` : 'personal'} Claude`, hint: `Runs on ${r.host ?? r.name} with ${owner ?? 'its owner'}'s own Claude Code login` };
  }
  return { real: true, text: `Real · ${r.auth === 'api_key' ? 'API key' : r.auth === 'bedrock' ? 'Bedrock' : r.auth === 'vertex' ? 'Vertex' : 'shared'}`, hint: `Runs on the shared runner ${r.name}` };
}

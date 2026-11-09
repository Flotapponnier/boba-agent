import type { Agent, LogKind, ReviewVerdict, Ticket } from '../api/types';

export interface Step {
  kind: LogKind | 'permission' | 'fail';
  text: string;
  data?: unknown;
  wait?: number;
  activity?: string;
  tool?: string;
  input?: unknown;
  verdict?: ReviewVerdict;
}

export interface ScriptCtx {
  ticket: Ticket;
  agent: Agent;
  repo: string;
  skills: string[];
  instructions: string | null;
  resume: boolean;
  prNumber: number;
  /** Seeded history: force the review outcome. */
  verdict?: ReviewVerdict;
}

let tid = 0;
const toolId = () => `toolu_mock${(++tid).toString(36)}${Math.random().toString(36).slice(2, 7)}`;

function files(t: Ticket): string[] {
  const tags = t.tags.join(' ') + ' ' + t.title.toLowerCase();
  if (/seo|meta|sitemap|canonical|landing|copy/.test(tags)) return ['src/pages/trade/[pair].astro', 'src/layouts/Base.astro', 'public/llms.txt'];
  if (/docker|ci|node|runner|infra/.test(tags)) return ['.github/workflows/ci.yml', 'docker/runner.Dockerfile', 'scripts/bootstrap.sh'];
  if (/postgres|index|perf|sql/.test(tags)) return ['migrations/0042_trades_idx.sql', 'src/db/trades.ts', 'src/db/trades.test.ts'];
  if (/react|ui|dark|theme/.test(tags)) return ['src/components/Dashboard.tsx', 'src/theme/tokens.css', 'src/components/Dashboard.test.tsx'];
  return ['src/fees.ts', 'src/fees.test.ts', 'src/api/client.ts'];
}

function tool(name: string, input: Record<string, unknown>, summary: string, output: string, activity: string, isError = false): Step[] {
  const id = toolId();
  return [
    { kind: 'tool_use', text: `${name} ${summary}`, data: { id, name, input }, activity, wait: 700 },
    { kind: 'tool_result', text: output.split('\n')[0] ?? '', data: { tool_use_id: id, output, is_error: isError }, wait: 900 },
  ];
}

export function workScript(c: ScriptCtx): Step[] {
  const [main, test, other] = files(c.ticket) as [string, string, string];
  const steps: Step[] = [];
  const model = c.agent.model ?? 'claude-sonnet-5-5';
  if (c.resume) steps.push({ kind: 'system', text: `Resuming Claude Code session · ${model}`, activity: 'Resuming the session', wait: 600 });
  else {
    steps.push({ kind: 'system', text: `git worktree add /data/worktrees/T-${c.ticket.number} -b ${c.ticket.branch ?? 'boba/T-' + c.ticket.number}`, activity: 'Creating the worktree', wait: 700 });
    steps.push({ kind: 'system', text: `Session started · ${model} · permission mode ${c.agent.permission_mode} · cwd /data/worktrees/T-${c.ticket.number}`, wait: 600 });
  }
  for (const s of c.skills.slice(0, 2)) steps.push({ kind: 'system', text: `Skill loaded: ${s}`, wait: 350 });
  if (c.ticket.review_feedback) steps.push({ kind: 'thinking', text: `Last review said: "${c.ticket.review_feedback}". I need to fix exactly that and nothing else.`, activity: 'Reading the review feedback', wait: 1400 });
  if (c.instructions) steps.push({ kind: 'thinking', text: `Extra instructions from the team: ${c.instructions}`, wait: 1000 });
  steps.push({ kind: 'thinking', text: `The ticket "${c.ticket.title}" is about ${c.ticket.tags.join(', ') || 'this repo'}. Let me look at the layout before touching anything, then write a failing test first.`, activity: 'Thinking', wait: 1600 });
  steps.push({ kind: 'text', text: `I'll start by exploring **${c.repo}** to find where this lives.`, wait: 800 });
  steps.push(...tool('Glob', { pattern: 'src/**/*' }, 'src/**/*', [main, test, other, 'src/index.ts', 'README.md'].join('\n'), 'Exploring the repo'));
  steps.push(...tool('Read', { file_path: `/data/worktrees/T-${c.ticket.number}/${main}` }, main, `   1  // ${main}\n   2  export function compute(input) {\n   3    // TODO: handle edge cases\n   4    return input.size * input.price;\n   5  }`, `Reading ${main}`));
  steps.push(
    ...tool(
      'TodoWrite',
      {
        todos: [
          { content: 'Write a failing test', status: 'in_progress', activeForm: 'Writing a failing test' },
          { content: `Fix ${main}`, status: 'pending', activeForm: `Fixing ${main}` },
          { content: 'Run the test suite', status: 'pending', activeForm: 'Running the test suite' },
          { content: 'Open the pull request', status: 'pending', activeForm: 'Opening the pull request' },
        ],
      },
      '4 todos',
      'Todos have been modified successfully.',
      'Planning',
    ),
  );
  steps.push(...tool('Write', { file_path: test, content: `import { compute } from './${main.split('/').pop()}';\n\ntest('handles the edge case', () => {\n  expect(compute({ size: 0, price: 10 })).toBe(0);\n});\n` }, test, `File created successfully at: ${test}`, `Writing ${test}`));
  steps.push({ kind: 'thinking', text: 'The test fails as expected. Now the actual fix: guard the edge case and keep the public signature.', wait: 1300 });
  steps.push(...tool('Edit', { file_path: main, old_string: '  return input.size * input.price;', new_string: '  if (!input.size) return 0;\n  return input.size * input.price;' }, main, `The file ${main} has been updated.`, `Editing ${main}`));
  const testCmd = /docker|ci/.test(main) ? 'act -j build' : /\.sql|db\//.test(main) ? 'pnpm test src/db' : 'pnpm test';
  if (c.agent.permission_mode === 'default' && !c.verdict) steps.push({ kind: 'permission', text: testCmd, tool: 'Bash', input: { command: testCmd, description: 'Run the test suite' } });
  if (!c.verdict && Math.random() < 0.07) {
    steps.push(...tool('Bash', { command: testCmd, description: 'Run the test suite' }, testCmd, `FAIL ${test}\n  ✕ handles the edge case (12 ms)\n\nTests: 1 failed, 41 passed`, 'Running tests', true));
    steps.push({ kind: 'fail', text: 'Tests still fail after 3 attempts; giving up so a human can look.' });
    return steps;
  }
  steps.push(...tool('Bash', { command: testCmd, description: 'Run the test suite' }, testCmd, `✓ ${test} (3 tests) 18ms\n✓ 42 tests passed\n\nTest Files  9 passed (9)`, 'Running tests'));
  steps.push(...tool('Bash', { command: `git add -A && git commit -m "T-${c.ticket.number}: ${c.ticket.title}" && git push -u origin HEAD` }, 'git commit && git push', `[${c.ticket.branch}] T-${c.ticket.number}: ${c.ticket.title}\n 2 files changed, 14 insertions(+), 1 deletion(-)`, 'Pushing the branch'));
  steps.push(...tool('Bash', { command: `gh pr create --fill --base main` }, 'gh pr create', `https://github.com/${c.repo}/pull/${c.prNumber}`, 'Opening pull request'));
  steps.push({
    kind: 'text',
    text: `Done! Summary of **T-${c.ticket.number}**:\n\n- Added a failing test in \`${test}\`\n- Fixed the edge case in \`${main}\`\n- All 42 tests pass\n- Opened PR #${c.prNumber}`,
    wait: 600,
  });
  return steps;
}

export function reviewScript(c: ScriptCtx): Step[] {
  const [main, test] = files(c.ticket) as [string, string, string];
  const approve = c.verdict ? c.verdict === 'approved' : Math.random() < (c.ticket.review_rounds ? 0.9 : 0.7);
  const verdict: ReviewVerdict = approve ? 'approved' : 'changes_requested';
  const feedback = approve ? 'Clean diff, focused on the ticket, tests cover the change.' : c.ticket.review_feedback ?? 'Missing a test for the empty input case.';
  return [
    { kind: 'system', text: `Session started · ${c.agent.model ?? 'claude-sonnet-5-5'} · permission mode plan (read-only)`, activity: `Opening PR #${c.prNumber}`, wait: 600 },
    ...c.skills.slice(0, 2).map((s): Step => ({ kind: 'system', text: `Skill loaded: ${s}`, wait: 300 })),
    ...tool('Bash', { command: `gh pr diff ${c.prNumber}` }, `gh pr diff ${c.prNumber}`, `diff --git a/${main} b/${main}\n@@ -1,5 +1,6 @@\n export function compute(input) {\n-  // TODO: handle edge cases\n+  if (!input.size) return 0;\n   return input.size * input.price;\n }\ndiff --git a/${test} b/${test}\nnew file mode 100644`, 'Reading the diff'),
    { kind: 'thinking', text: 'The guard is correct. Is there a test for every new branch? Let me read the test file.', activity: 'Checking tests and edge cases', wait: 1500 },
    ...tool('Read', { file_path: test }, test, `   1  import { compute } from './fees';\n   2\n   3  test('handles the edge case', () => {\n   4    expect(compute({ size: 0, price: 10 })).toBe(0);\n   5  });`, `Reading ${test}`),
    { kind: 'thinking', text: approve ? 'Everything is in scope and tested. Approving.' : 'The empty input path is not covered. Requesting changes.', activity: 'Writing the verdict', wait: 1300 },
    {
      kind: 'text',
      text: `${approve ? 'LGTM' : 'Almost there'} for PR #${c.prNumber}.\n\n${approve ? `- \`${main}\`: guard is correct\n- \`${test}\`: covers the new branch` : `- \`${test}\`: ${feedback}`}\n\n\`\`\`boba-verdict\n${JSON.stringify({ verdict, feedback })}\n\`\`\``,
      verdict,
      wait: 500,
    },
  ];
}

import { nextRuns } from '../lib/schedule';
import type {
  Agent,
  Column,
  ColumnKind,
  HistoryEvent,
  HistoryKind,
  Priority,
  Repo,
  Run,
  RunnerInfo,
  Skill,
  Tab,
  Ticket,
  User,
  WorkspaceSettings,
  WorkspaceStats,
} from '../api/types';

export interface MockUser extends User {
  /** Mock auth: any non-empty password logs an activated user in. */
  activated: boolean;
}

export interface MockState {
  jobs: import('../api/types').Job[];
  connectors: import('../api/types').Connector[];
  users: MockUser[];
  tabs: Tab[];
  agents: Agent[];
  tickets: Ticket[];
  runs: Run[];
  logs: Record<string, import('../api/types').RunLogLine[]>;
  skills: Skill[];
  skillBodies: Record<string, string>;
  repos: Repo[];
  runners: RunnerInfo[];
  history: HistoryEvent[];
  settings: WorkspaceSettings;
  stats: WorkspaceStats;
  invites: Record<string, string>;
  seq: number;
  prSeq: number;
  idSeq: number;
  sessionUserId: string | null;
}

const iso = (msAgo: number) => new Date(Date.now() - msAgo).toISOString();
const H = 3600_000;
const D = 24 * H;

export const defaultSync = (): import('../api/types').RepoSync => ({
  enabled: false,
  target_tab_id: null,
  import_issues: true,
  issue_labels: [],
  import_prs: true,
  skip_draft_prs: true,
  comment_back: true,
  post_reviews: false,
  close_issues_on_done: true,
  poll_minutes: 10,
  last_synced_at: null,
  last_error: null,
});

function job(id: string, name: string, icon: string, agent_id: string, prompt: string, schedule: import('../api/types').Schedule, output: import('../api/types').JobOutput, extra: Partial<import('../api/types').Job> = {}): import('../api/types').Job {
  const j: import('../api/types').Job = { id, name, description: '', icon, agent_id, prompt, schedule, output, enabled: true, repo_id: null, tab_id: null, skip_if_running: true, last_run_id: null, last_run_at: null, next_run_at: null, last_result: null, runs_count: 0, created_by: 'u_owner', created_at: iso(8 * D), ...extra };
  j.next_run_at = j.enabled ? nextRuns(schedule, new Date(), 1)[0]?.toISOString() ?? null : null;
  return j;
}

let n = 0;
export const mkId = (p: string) => `${p}_${(++n).toString(36)}${Math.random().toString(36).slice(2, 6)}`;

function cols(tabId: string, specs: [string, ColumnKind, string][], reviewer: string | null): Column[] {
  return specs.map(([name, kind, color], i) => ({
    id: `${tabId}_c${i}`,
    tab_id: tabId,
    name,
    kind,
    color,
    position: i,
    auto_reviewer_agent_id: kind === 'review' ? reviewer : null,
    on_approve_column_id: null,
    on_changes_column_id: null,
  }));
}

function agent(id: string, name: string, role: string, sprite: string, color: string, tagline: string, prompt: string, skills: string[], extra: Partial<Agent> = {}): Agent {
  return {
    id,
    name,
    role,
    sprite,
    color,
    tagline,
    system_prompt: prompt,
    skill_ids: skills,
    connector_ids: [],
    board_tools: false,
    model: null,
    permission_mode: 'acceptEdits',
    allowed_tools: [],
    max_turns: null,
    xp: 0,
    level: 1,
    runs_completed: 0,
    prs_approved: 0,
    status: 'idle',
    current_run_id: null,
    created_by: 'u_owner',
    created_at: iso(9 * D),
    ...extra,
  };
}

const SKILL_BODIES: Record<string, string> = {
  sk_review: `# PR review\n\nRead the whole diff before judging.\n\n1. Run \`gh pr diff\` and list every changed file.\n2. For each finding, cite \`file:line\` and explain the failure scenario.\n3. Check tests exist for new behaviour.\n4. Never merge yourself.\n\nEnd with a \`boba-verdict\` block.`,
  sk_tests: `# Write tests\n\nEvery bug fix starts with a failing test.\n\n- Prefer small unit tests next to the code.\n- Use the project's existing runner (vitest, cargo test, go test…).\n- Name tests after the behaviour, not the function.`,
  sk_seo: `# SEO audit\n\nCheck title, meta description, canonical, Open Graph, sitemap and \`llms.txt\` for each page.\nReport missing items as a table: page, issue, fix.`,
  sk_ts: `# TypeScript\n\nStrict mode, no \`any\`. Narrow with type guards. Prefer discriminated unions.`,
  sk_react: `# React\n\nFunction components and hooks. Keep state close to where it is used. Memoise only when measured.`,
  sk_security: `# Security review\n\nLook for injection, authz gaps, secrets in code, unsafe deserialisation and SSRF.`,
  sk_design: `# Frontend design\n\nDistinctive, production-grade interfaces. Respect the existing design tokens.`,
  sk_deploy: `# Deploy checklist\n\n1. CI green\n2. Migrations reviewed\n3. Feature flag ready\n4. Rollback plan written`,
  sk_docker: `# Docker\n\nSmall images, non-root user, pinned base image digests, healthchecks.`,
};

export function seed(): MockState {
  const owner: MockUser = {
    id: 'u_owner',
    name: 'You',
    email: 'owner@boba.local',
    role: 'owner',
    avatar: 'boba',
    created_at: iso(10 * D),
    activated: true,
  };
  const mika: MockUser = { id: 'u_mika', name: 'Mika', email: 'mika@boba.local', role: 'admin', avatar: 'pinky_boba', created_at: iso(8 * D), activated: true };
  const sam: MockUser = { id: 'u_sam', name: 'Sam', email: 'sam@boba.local', role: 'member', avatar: 'golden_boba', created_at: iso(6 * D), activated: true };
  const lou: MockUser = { id: 'u_lou', name: 'Lou', email: 'lou@boba.local', role: 'viewer', avatar: 'black_boba', created_at: iso(2 * D), activated: true };

  const skills: Skill[] = [
    { id: 'sk_review', name: 'pr-review', description: 'Review a pull request and cite file:line for every finding.', source: 'boba', path: '/data/skills/pr-review', editable: true },
    { id: 'sk_tests', name: 'write-tests', description: 'Write a failing test first, then fix.', source: 'boba', path: '/data/skills/write-tests', editable: true },
    { id: 'sk_seo', name: 'seo-audit', description: 'Audit meta tags, sitemap and llms.txt.', source: 'boba', path: '/data/skills/seo-audit', editable: true },
    { id: 'sk_ts', name: 'typescript', description: 'Strict TypeScript conventions.', source: 'user', path: '~/.claude/skills/typescript', editable: false },
    { id: 'sk_react', name: 'react', description: 'React 19 patterns and hooks.', source: 'user', path: '~/.claude/skills/react', editable: false },
    { id: 'sk_deploy', name: 'deploy-checklist', description: 'Pre-deploy checklist for this repo.', source: 'project', path: '.claude/skills/deploy-checklist', editable: false },
    { id: 'sk_docker', name: 'docker', description: 'Small, hardened container images.', source: 'project', path: '.claude/skills/docker', editable: false },
    { id: 'sk_security', name: 'security-review', description: 'Find injection, authz gaps and leaked secrets.', source: 'plugin', path: '~/.claude/plugins/security/skills/security-review', editable: false },
    { id: 'sk_design', name: 'frontend-design', description: 'Distinctive, production-grade UI.', source: 'plugin', path: '~/.claude/plugins/design/skills/frontend-design', editable: false },
  ];

  const agents: Agent[] = [
    agent('a_dev', 'Boba Dev', 'Dev', 'boba', '#f39c12', 'Ships it with tests, every time.', 'You are Boba Dev, a full-stack developer. Work in your worktree, write tests first and open a clean PR.', ['sk_ts', 'sk_react', 'sk_tests'], { xp: 60, level: 3, runs_completed: 14, prs_approved: 11 }),
    agent('a_uncle', 'Uncle Review', 'Reviewer', 'boba_uncle', '#3498db', 'No file:line, no approval.', 'You are Uncle Review. Review every PR with the pr-review skill, cite file:line, never merge yourself.', ['sk_review', 'sk_security'], { xp: 120, level: 4, runs_completed: 23, permission_mode: 'plan' }),
    agent('a_pinky', 'Pinky SEO', 'SEO', 'pinky_boba', '#ff6fb5', 'Every page deserves a meta.', 'You are Pinky SEO. Audit pages, meta tags, sitemap and llms.txt.', ['sk_seo'], { xp: 40, level: 2, runs_completed: 5, prs_approved: 4, connector_ids: ['cn_gsc'], board_tools: true }),
    agent('a_diamond', 'Diamond Architect', 'Architect', 'boba_diamond', '#1abc9c', 'Design first, measure always.', 'You are Diamond Architect. Design before coding and measure performance.', ['sk_ts', 'sk_deploy'], { xp: 10, level: 2, runs_completed: 3, prs_approved: 2, model: 'claude-opus-5-5' }),
    agent('a_golden', 'Golden QA', 'QA', 'golden_boba', '#ffd23f', 'If it is not tested, it is broken.', 'You are Golden QA. Reproduce bugs and write end-to-end tests.', ['sk_tests'], { xp: 80, level: 1, runs_completed: 2, prs_approved: 1, model: 'claude-haiku-5-5', connector_ids: ['cn_sentry'], board_tools: true }),
    agent('a_black', 'Black Ops', 'DevOps', 'black_boba', '#34495e', 'CI green or it did not happen.', 'You are Black Ops. You own CI, deployments and the runners.', ['sk_docker', 'sk_deploy'], { xp: 0, level: 1, runs_completed: 0, permission_mode: 'default', connector_ids: ['cn_web'] }),
  ];

  const tabs: Tab[] = [
    {
      id: 'tab_dev', name: 'Dev', icon: '🧋', color: '#f39c12', position: 0, repo_id: 'r_app',
      columns: cols('tab_dev', [['Backlog', 'backlog', '#95a5a6'], ['To do', 'todo', '#e74c3c'], ['Doing', 'doing', '#f39c12'], ['PR · Review', 'review', '#3498db'], ['Done', 'done', '#2ecc71']], 'a_uncle'),
    },
    {
      id: 'tab_seo', name: 'SEO', icon: '🔎', color: '#ff6fb5', position: 1, repo_id: 'r_site',
      columns: cols('tab_seo', [['To do', 'todo', '#e74c3c'], ['Doing', 'doing', '#f39c12'], ['Review', 'review', '#3498db'], ['Done', 'done', '#2ecc71']], 'a_uncle'),
    },
    {
      id: 'tab_infra', name: 'Infra', icon: '🛠️', color: '#3498db', position: 2, repo_id: 'r_infra',
      columns: cols('tab_infra', [['To do', 'todo', '#e74c3c'], ['Doing', 'doing', '#f39c12'], ['Review', 'review', '#3498db'], ['Shipped', 'done', '#2ecc71']], 'a_uncle'),
    },
  ];

  let seq = 0;
  const tickets: Ticket[] = [];
  const t = (tab: string, col: number, title: string, body: string, tags: string[], priority: Priority = 'normal', extra: Partial<Ticket> = {}) => {
    seq++;
    const tk: Ticket = {
      id: `tk_${seq}`,
      number: seq,
      tab_id: tab,
      column_id: `${tab}_c${col}`,
      position: seq * 1024,
      title,
      comment_count: 0,
      open_thread_count: 0,
      body,
      tags,
      priority,
      color: null,
      repo_id: null,
      agent_id: null,
      branch: null,
      pr_url: null,
      pr_number: null,
      review_feedback: null,
      review_rounds: 0,
      source: { kind: 'manual' },
      created_by: 'u_owner',
      created_at: iso((20 - seq) * 5 * H),
      updated_at: iso((20 - seq) * 3 * H),
      ...extra,
    };
    tickets.push(tk);
    return tk;
  };

  t('tab_dev', 1, 'Dark mode for the dashboard', 'Add a dark theme to the trader pages.\n\n- Respect `prefers-color-scheme`\n- Persist the choice', ['react', 'ui'], 'normal');
  t('tab_dev', 1, 'Fee calculation bug', 'Displayed fees differ from real fees on forex pairs. Reproduce with a test first.', ['typescript', 'bug'], 'urgent', {
    review_feedback: 'Missing a test for the zero-size position case (src/fees.ts:88).',
    review_rounds: 1,
    agent_id: 'a_dev',
    branch: 'boba/T-2-fee-calculation-bug',
    pr_number: 1201,
    pr_url: 'https://github.com/boba-labs/app/pull/1201',
  });
  t('tab_dev', 1, 'Slow index on trades', 'The history query takes 4 s. Add an index and measure.', ['postgres', 'perf'], 'high');
  t('tab_dev', 0, 'E2E tests for checkout', 'Cover opening and closing a position.', ['playwright', 'tests'], 'normal');
  t('tab_dev', 0, 'Upgrade to React 19.3', 'Read the changelog, run the codemods.', ['react'], 'low');
  t('tab_dev', 4, 'Rate-limit the public API', 'Token bucket per API key, 429 with Retry-After.', ['backend'], 'high', {
    agent_id: 'a_dev', branch: 'boba/T-6-rate-limit', pr_number: 1188, pr_url: 'https://github.com/boba-labs/app/pull/1188', review_rounds: 0,
  });
  t('tab_seo', 0, 'Meta descriptions for /trade', '60 pages have no meta description.', ['seo'], 'high');
  t('tab_seo', 0, 'Sitemap + llms.txt', 'Generate both and submit to Search Console.', ['seo'], 'normal');
  t('tab_seo', 0, 'Rewrite the landing page', 'More fun, less jargon. Keep it under 120 words above the fold.', ['copywriting'], 'normal');
  t('tab_seo', 3, 'Canonical URLs on blog', 'Duplicate content between /blog and /news.', ['seo'], 'normal', {
    agent_id: 'a_pinky', branch: 'boba/T-10-canonical', pr_number: 1170, pr_url: 'https://github.com/boba-labs/site/pull/1170',
  });
  t('tab_dev', 1, 'Login button misaligned on Safari', 'Reported on GitHub: the login button overflows its container on Safari 19.', ['bug', 'ui'], 'normal', {
    repo_id: 'r_app',
    source: { kind: 'github_issue', repo_id: 'r_app', number: 482, url: 'https://github.com/boba-labs/app/issues/482', author: 'octo-lea' },
  });
  t('tab_infra', 0, 'Node 26 on the runners', 'Node 22 reaches end of life. Bump images and CI.', ['ci'], 'high');
  t('tab_infra', 0, 'Hardened runner image', 'One container per task, non-root, read-only rootfs.', ['docker', 'security'], 'normal');

  const runs: Run[] = [];
  const history: HistoryEvent[] = [];
  const h = (ago: number, kind: HistoryKind, message: string, extra: Partial<HistoryEvent> = {}) =>
    history.push({
      id: mkId('h'),
      ts: iso(ago),
      kind,
      actor_kind: 'user',
      actor_id: 'u_owner',
      ticket_id: null,
      agent_id: null,
      run_id: null,
      tab_id: null,
      message,
      data: {},
      ...extra,
    });

  h(9 * D, 'user_joined', 'You created the workspace Boba HQ', { actor_kind: 'system' });
  h(9 * D - H, 'agent_created', 'You recruited Boba Dev', { agent_id: 'a_dev' });
  h(9 * D - 2 * H, 'agent_created', 'You recruited Uncle Review', { agent_id: 'a_uncle' });
  h(8 * D, 'user_joined', 'Mika joined as Admin', { actor_id: 'u_mika' });
  h(6 * D, 'user_joined', 'Sam joined as Member', { actor_id: 'u_sam' });
  h(3 * D, 'run_succeeded', 'Boba Dev finished T-6 Rate-limit the public API', { actor_kind: 'agent', actor_id: 'a_dev', agent_id: 'a_dev', ticket_id: 'tk_6', tab_id: 'tab_dev' });
  h(3 * D - 60_000, 'pr_opened', 'Boba Dev opened PR #1188 for T-6', { actor_kind: 'agent', actor_id: 'a_dev', agent_id: 'a_dev', ticket_id: 'tk_6', tab_id: 'tab_dev' });
  h(3 * D - 20 * 60_000, 'review_approved', 'Uncle Review approved PR #1188 (+15 pearls)', { actor_kind: 'agent', actor_id: 'a_uncle', agent_id: 'a_uncle', ticket_id: 'tk_6', tab_id: 'tab_dev' });
  h(D, 'run_succeeded', 'Boba Dev finished T-2 Fee calculation bug', { actor_kind: 'agent', actor_id: 'a_dev', agent_id: 'a_dev', ticket_id: 'tk_2', tab_id: 'tab_dev' });
  h(D - 60_000, 'pr_opened', 'Boba Dev opened PR #1201 for T-2', { actor_kind: 'agent', actor_id: 'a_dev', agent_id: 'a_dev', ticket_id: 'tk_2', tab_id: 'tab_dev' });
  h(D - 25 * 60_000, 'review_changes_requested', 'Uncle Review requested changes on PR #1201: Missing a test for the zero-size position case (src/fees.ts:88).', { actor_kind: 'agent', actor_id: 'a_uncle', agent_id: 'a_uncle', ticket_id: 'tk_2', tab_id: 'tab_dev' });
  h(5 * H, 'pr_opened', 'Pinky SEO opened PR #1170 for T-10', { actor_kind: 'agent', actor_id: 'a_pinky', agent_id: 'a_pinky', ticket_id: 'tk_10', tab_id: 'tab_seo' });
  h(2 * H, 'comment', 'Sam commented on T-3: "Seen it in prod too, 4.2 s on the big accounts."', { actor_id: 'u_sam', ticket_id: 'tk_3', tab_id: 'tab_dev' });
  history.reverse();

  return {
    users: [owner, mika, sam, lou],
    tabs,
    agents,
    tickets,
    runs,
    logs: {},
    skills,
    skillBodies: { ...SKILL_BODIES },
    repos: [
      { id: 'r_app', name: 'boba-labs/app', path: '/srv/repos/app', remote_url: 'git@github.com:boba-labs/app.git', default_branch: 'main', runner_id: null, sync: { ...defaultSync(), enabled: true, target_tab_id: 'tab_dev', issue_labels: ['bug', 'feature'], last_synced_at: iso(12 * 60_000) } },
      { id: 'r_site', name: 'boba-labs/site', path: '/srv/repos/site', remote_url: 'git@github.com:boba-labs/site.git', default_branch: 'main', runner_id: null, sync: defaultSync() },
      { id: 'r_infra', name: 'boba-labs/infra', path: '/srv/repos/infra', remote_url: 'git@github.com:boba-labs/infra.git', default_branch: 'main', runner_id: null, sync: defaultSync() },
    ],
    runners: [{ id: 'local', name: 'local (this server)', online: true, max_concurrent_runs: 4, active_runs: 0, last_seen_at: iso(0), scope: 'shared', owner_user_id: null, auth: 'demo', host: 'boba-hq', claude_version: null }],
    history,
    settings: { name: 'Boba HQ', theme: 'sky', music_enabled_by_default: false, demo_mode: true, speech: { type: 'browser' } },
    stats: { pearls: 128, runs_completed: 47, prs_approved: 18, total_cost_usd: 23.41 },
    invites: {},
    jobs: [
      job('job_seo', 'Weekly SEO audit', '🔎', 'a_pinky', 'Check Search Console for pages that lost clicks or have indexing issues and file one ticket per fix in the SEO tab.', { type: 'cron', expr: '0 9 * * 1', timezone: 'Europe/Paris' }, 'tickets', { tab_id: 'tab_seo', runs_count: 6, last_run_at: iso(2 * D), last_result: 'Filed 2 tickets: canonical mismatch on /trade/btc-usd; missing meta description on /blog/fees-explained.' }),
      job('job_deps', 'Nightly dependency check', '🛡️', 'a_black', 'List outdated and vulnerable dependencies of the app and summarise what to upgrade.', { type: 'cron', expr: '30 2 * * *', timezone: 'Europe/Paris' }, 'report', { repo_id: 'r_app', runs_count: 21, last_run_at: iso(18 * H), last_result: '## Nightly dependency check\n\n- 4 outdated packages (1 security advisory, moderate)\n- Recommend upgrading `undici` this week' }),
      job('job_crash', 'Crash triage', '🚨', 'a_golden', 'Read new unresolved Sentry issues and file a ticket for each crash with more than 50 events.', { type: 'interval', minutes: 120 }, 'tickets', { tab_id: 'tab_dev', enabled: false, runs_count: 3, last_run_at: iso(3 * D), last_result: 'Filed 1 ticket: TypeError in fees.ts:88 (312 events).' }),
    ],
    connectors: [
      { id: 'cn_gsc', name: 'Search Console', kind: 'google_search_console', transport: { type: 'stdio', command: 'npx', args: ['-y', 'mcp-server-gsc'] }, secret_keys: ['GOOGLE_APPLICATION_CREDENTIALS_JSON'], allowed_tools: [], created_by: 'u_owner', created_at: iso(7 * D) },
      { id: 'cn_sentry', name: 'Sentry', kind: 'sentry', transport: { type: 'http', url: 'https://mcp.sentry.dev/mcp', header_secrets: { Authorization: 'SENTRY_TOKEN' } }, secret_keys: ['SENTRY_TOKEN'], allowed_tools: ['list_issues', 'get_issue'], created_by: 'u_mika', created_at: iso(5 * D) },
      { id: 'cn_web', name: 'Web search', kind: 'web_search', transport: { type: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-brave-search'] }, secret_keys: ['BRAVE_API_KEY'], allowed_tools: [], created_by: 'u_owner', created_at: iso(4 * D) },
    ],
    seq,
    prSeq: 1210,
    idSeq: 0,
    sessionUserId: 'u_owner',
  };
}

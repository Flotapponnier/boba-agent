/** Mock pieces for jobs, connectors, repo sync and the concierge (kept apart from server.ts). */
import type { Agent, CommandAction, Connector, ConnectorKind, Job, Permission, PlannedAction, Schedule, Tab, Ticket } from '../api/types';
import { DAYS, localTz } from '../lib/schedule';
import type { Step } from './scripts';

let tid = 0;
const toolId = () => `toolu_job${(++tid).toString(36)}${Math.random().toString(36).slice(2, 6)}`;
const tool = (name: string, input: Record<string, unknown>, summary: string, output: string, activity: string): Step[] => {
  const id = toolId();
  return [
    { kind: 'tool_use', text: `${name} ${summary}`, data: { id, name, input }, activity, wait: 800 },
    { kind: 'tool_result', text: output.split('\n')[0] ?? '', data: { tool_use_id: id, output, is_error: false }, wait: 900 },
  ];
};

export const TOOLS: Record<ConnectorKind, string[]> = {
  google_search_console: ['search_analytics', 'list_sites', 'inspect_url', 'list_sitemaps'],
  google_workspace: ['gmail_search', 'calendar_list_events', 'drive_search', 'docs_read'],
  linear: ['list_issues', 'create_issue', 'get_issue', 'list_projects'],
  slack: ['list_channels', 'read_channel', 'post_message'],
  sentry: ['list_issues', 'get_issue', 'get_event', 'list_projects'],
  notion: ['search', 'read_page', 'query_database'],
  web_search: ['search', 'fetch_page'],
  postgres: ['query', 'list_tables', 'describe_table'],
  github: ['search_issues', 'get_pull_request', 'search_code'],
  custom_mcp: ['ping', 'run'],
};

export const slugName = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_|_$/g, '') || 'portal';

export function jobScript(job: Job, agent: Agent, connectors: Connector[]): { steps: Step[]; tickets: { title: string; body: string; tags: string[] }[]; report: string } {
  const steps: Step[] = [
    { kind: 'system', text: `Quest "${job.name}" started · ${agent.model ?? 'claude-sonnet-5-5'} · board tools ${agent.board_tools ? 'on' : 'off'}`, activity: 'Reading the quest', wait: 700 },
  ];
  if (job.last_result) steps.push({ kind: 'thinking', text: `Last time I reported: "${job.last_result.slice(0, 120)}…". I should focus on what changed since.`, wait: 1200 });
  steps.push({ kind: 'thinking', text: `Quest: ${job.prompt}`, activity: 'Planning', wait: 1100 });
  const used = connectors.length ? connectors : [{ id: 'web', name: 'web_search', kind: 'web_search' } as Connector];
  for (const c of used.slice(0, 2)) {
    const n = slugName(c.name);
    if (c.kind === 'google_search_console')
      steps.push(
        ...tool(`mcp__${n}__search_analytics`, { site: 'sc-domain:boba-labs.dev', start_date: '7daysAgo', dimensions: ['page'] }, 'last 7 days by page', 'page                         clicks  impr   ctr\n/trade/eur-usd                 412   9.8k  4.2%\n/trade/btc-usd                  57   7.1k  0.8%  ← CTR drop\n/blog/fees-explained             9   3.0k  0.3%  ← no meta description', 'Querying Search Console'),
        ...tool(`mcp__${n}__inspect_url`, { url: 'https://boba-labs.dev/trade/btc-usd' }, '/trade/btc-usd', 'Indexed, but canonical points to /trade/BTC-USD (case mismatch)', 'Inspecting URLs'),
      );
    else if (c.kind === 'sentry')
      steps.push(...tool(`mcp__${n}__list_issues`, { project: 'app', query: 'is:unresolved firstSeen:-24h' }, 'unresolved, last 24h', 'APP-4F2  TypeError: cannot read "size" of undefined  (fees.ts:88)  312 events\nAPP-4F9  ChunkLoadError on /checkout  41 events', 'Reading Sentry issues'));
    else steps.push(...tool(`mcp__${n}__${TOOLS[c.kind][0]}`, { query: job.name }, job.name, '3 results\n1. Upgrade guide: breaking changes in 26.x\n2. Security advisory GHSA-x2f9 (moderate)\n3. Changelog', `Using ${c.name}`));
  }
  const tickets =
    job.output === 'tickets'
      ? [
          { title: 'Fix canonical case mismatch on /trade/btc-usd', body: 'Search Console: canonical points to /trade/BTC-USD. CTR dropped to 0.8%.\n\n- Lowercase canonicals\n- 301 the uppercase URL', tags: ['seo', 'quest'] },
          { title: 'Add meta description to /blog/fees-explained', body: '3.0k impressions, 0.3% CTR, no description.', tags: ['seo', 'quest'] },
        ]
      : [];
  if (job.output === 'tickets' && agent.board_tools) {
    for (const t of tickets) steps.push(...tool('mcp__boba__create_ticket', { tab_id: job.tab_id, title: t.title, body: t.body, tags: t.tags, priority: 'normal' }, t.title, `Created ticket "${t.title}"`, 'Filing tickets'));
  }
  if (job.output === 'pull_request') {
    steps.push(...tool('Bash', { command: 'pnpm up --latest && pnpm test' }, 'pnpm up --latest && pnpm test', '✓ 42 tests passed', 'Upgrading dependencies'));
    steps.push(...tool('Bash', { command: 'gh pr create --fill' }, 'gh pr create', 'https://github.com/boba-labs/app/pull/1300', 'Opening pull request'));
  }
  const report =
    job.output === 'tickets'
      ? agent.board_tools
        ? `Filed ${tickets.length} tickets: ${tickets.map((t) => t.title).join('; ')}.`
        : 'Found 2 issues, but board tools are off so I could not file tickets. Turn on "Board tools" for this agent.'
      : job.output === 'pull_request'
        ? 'Opened a PR upgrading 4 dependencies; all tests pass.'
        : `## ${job.name}\n\n- 4 outdated packages (1 security advisory, moderate)\n- No breaking changes expected\n- Recommend upgrading \`undici\` this week`;
  steps.push({ kind: 'text', text: report, activity: 'Writing the report', wait: 600 });
  return { steps, tickets: agent.board_tools ? tickets : [], report };
}

// ------------------------------------------------------------ concierge (keyword heuristics)

export interface PlanCtx {
  text: string;
  tab_id: string | null;
  ticket_id: string | null;
  agent_id: string | null;
  tabs: Tab[];
  agents: Agent[];
  tickets: Ticket[];
  jobs: Job[];
  repos: { id: string; name: string }[];
  grants: (p: Permission) => boolean;
}

const DAY_WORDS: Record<string, number> = { sunday: 0, monday: 1, tuesday: 2, wednesday: 3, thursday: 4, friday: 5, saturday: 6 };

function parseSchedule(t: string): Schedule {
  const tz = localTz();
  const at = t.match(/at (\d{1,2})(?::(\d{2}))?\s*(am|pm)?/);
  let h = at ? Number(at[1]) : 9;
  const m = at?.[2] ? Number(at[2]) : 0;
  if (at?.[3] === 'pm' && h < 12) h += 12;
  const every = t.match(/every (\d+) ?(minutes?|mins?|hours?|h)\b/);
  if (every) return { type: 'interval', minutes: Math.max(5, Number(every[1]) * (/h/.test(every[2] ?? '') ? 60 : 1)) };
  if (/every hour|hourly/.test(t)) return { type: 'interval', minutes: 60 };
  const days = Object.entries(DAY_WORDS).filter(([w]) => t.includes(w) || t.includes(w.slice(0, 3) + ' ')).map(([, d]) => d);
  if (/weekday/.test(t)) days.push(1, 2, 3, 4, 5);
  if (days.length) return { type: 'cron', expr: `${m} ${h} * * ${[...new Set(days)].sort().join(',')}`, timezone: tz };
  if (/weekly|every week/.test(t)) return { type: 'cron', expr: `${m} ${h} * * 1`, timezone: tz };
  if (/daily|every day|each day|every morning|nightly|every night/.test(t)) return { type: 'cron', expr: `${m} ${/night/.test(t) && !at ? 2 : h} * * *`, timezone: tz };
  return { type: 'manual' };
}

const clean = (s: string) =>
  s
    .replace(/^(please |hey |ok |boba,? )+/i, '')
    .replace(/^(create|add|make|open|pin|file|new)( a| an| the)?( new)? (ticket|task|card|issue)( to| for| about|:)?\s*/i, '')
    .replace(/\s+/g, ' ')
    .trim();
const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

export function plan(c: PlanCtx): { reply: string; actions: PlannedAction[]; question: string | null } {
  const t = c.text.toLowerCase();
  const agent = c.agents.find((a) => t.includes(a.name.toLowerCase())) ?? c.agents.find((a) => t.includes(a.name.split(' ')[0]!.toLowerCase())) ?? c.agents.find((a) => a.id === c.agent_id);
  const num = t.match(/\bt-?(\d+)\b/);
  const ticket = num ? c.tickets.find((x) => x.number === Number(num[1])) : /this ticket|\bit\b|this one/.test(t) ? c.tickets.find((x) => x.id === c.ticket_id) : undefined;
  const tab = c.tabs.find((x) => new RegExp(`\\b${x.name.toLowerCase()}\\b`).test(t)) ?? c.tabs.find((x) => x.id === c.tab_id) ?? c.tabs[0];
  const act = (label: string, action: CommandAction, required: Permission): PlannedAction => ({ label, action, required, allowed: c.grants(required) });

  if (/\b(quest|job|every|daily|weekly|nightly|hourly|each (monday|tuesday|wednesday|thursday|friday|morning|day)|schedule)\b/.test(t) && !/run (the )?(quest|job)/.test(t)) {
    const who = agent ?? c.agents.find((a) => a.role.toLowerCase() !== 'reviewer') ?? c.agents[0];
    if (!who) return { reply: '', actions: [], question: 'Which agent should take this quest? Recruit one first.' };
    const schedule = parseSchedule(t);
    const output = /ticket/.test(t) ? 'tickets' : /\bpr\b|pull request/.test(t) ? 'pull_request' : 'report';
    const words = c.text
      .replace(/^(please |hey )?(create|add|make|set up|schedule)( a| an)?( new)? (quest|job|recurring task)( to| that| for)?\s*/i, '')
      .replace(/^(every|each|on)\b[^,]*,\s*/i, '')
      .replace(/\b(every|each)\b.*$/i, '')
      .replace(new RegExp(`\\b${esc(who.name)}\\b|\\b${esc(who.name.split(' ')[0] ?? '')}\\b`, 'ig'), '')
      .replace(/\band (files?|opens?|writes?) .*$/i, '')
      .replace(/[^\w' -]/g, ' ')
      .trim()
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 6);
    const name = cap(words.join(' ').replace(/^(audits?|checks?|reviews?|scans?)\b/i, (m) => `${m.replace(/s$/, '')}:`).replace(/:\s*/, ' ')) || 'New quest';
    return {
      reply: `A new quest for ${who.name}. Check the schedule before I pin it.`,
      question: null,
      actions: [
        act(
          `Create quest "${name}" for ${who.name}`,
          { action: 'create_job', job: { name, description: null, icon: output === 'tickets' ? '📋' : output === 'pull_request' ? '🔧' : '📜', agent_id: who.id, prompt: c.text, schedule, output, enabled: true, repo_id: tab?.repo_id ?? null, tab_id: tab?.id ?? null, skip_if_running: true } },
          'manage_automations',
        ),
      ],
    };
  }
  if (/run (the )?(quest|job)|launch (the )?(quest|job)/.test(t)) {
    const job = c.jobs.find((j) => t.includes(j.name.toLowerCase())) ?? c.jobs.find((j) => j.name.toLowerCase().split(' ').some((w) => w.length > 3 && t.includes(w)));
    if (!job) return { reply: '', actions: [], question: `Which quest? I know: ${c.jobs.map((j) => j.name).join(', ') || 'none yet'}.` };
    return { reply: `Running "${job.name}" now.`, question: null, actions: [act(`Run quest "${job.name}" now`, { action: 'run_job', job_id: job.id }, 'run_agents')] };
  }
  if (/\b(recruit|hire)\b|new agent|create (an )?agent/.test(t)) {
    const nm = c.text.match(/(?:named|called)\s+([\w' -]{2,30})/i)?.[1]?.trim() ?? 'Boba Recruit';
    const role = /review/.test(t) ? 'Reviewer' : /seo/.test(t) ? 'SEO' : /qa|test/.test(t) ? 'QA' : /devops|ci\b|deploy/.test(t) ? 'DevOps' : 'Dev';
    return {
      reply: `Say hi to ${nm}!`,
      question: null,
      actions: [
        act(
          `Recruit ${nm} (${role})`,
          { action: 'create_agent', agent: { name: nm, role, sprite: ['boba', 'pinky_boba', 'golden_boba', 'boba_diamond', 'black_boba'][Math.floor(Math.random() * 5)] as string, color: null, tagline: null, system_prompt: `You are ${nm}, a careful ${role}.`, skill_ids: [], connector_ids: null, board_tools: null, model: null, permission_mode: null, allowed_tools: null, max_turns: null } },
          'manage_agents',
        ),
      ],
    };
  }
  if (/\bsync\b/.test(t)) {
    const repo = c.repos.find((r) => t.includes(r.name.toLowerCase().split('/').pop() ?? r.name)) ?? c.repos[0];
    if (!repo) return { reply: '', actions: [], question: 'There is no repository to sync yet.' };
    return { reply: `Syncing ${repo.name} with GitHub.`, question: null, actions: [act(`Sync ${repo.name} now`, { action: 'sync_repo', repo_id: repo.id }, 'run_agents')] };
  }
  if (/\bmove\b/.test(t) && ticket) {
    const tb = c.tabs.find((x) => x.id === ticket.tab_id);
    const col = tb?.columns.find((x) => t.includes(x.name.toLowerCase())) ?? tb?.columns.find((x) => t.includes(x.kind));
    if (!col) return { reply: '', actions: [], question: `Move T-${ticket.number} where? Columns: ${tb?.columns.map((x) => x.name).join(', ')}.` };
    return { reply: 'Moving it.', question: null, actions: [act(`Move T-${ticket.number} to ${col.name}`, { action: 'move_ticket', ticket_id: ticket.id, column_id: col.id }, 'edit_tickets')] };
  }
  if (/\b(cancel|stop|call back)\b/.test(t) && agent && agent.current_run_id) {
    return { reply: `Calling ${agent.name} back.`, question: null, actions: [act(`Cancel ${agent.name}'s current run`, { action: 'cancel_run', run_id: agent.current_run_id }, 'run_agents')] };
  }
  if (agent && ticket && /\b(assign|send|put|ask|have|review|work on|take|give)\b/.test(t)) {
    return {
      reply: `${agent.name} is on it.`,
      question: null,
      actions: [act(`Send ${agent.name} on T-${ticket.number} ${ticket.title}`, { action: 'assign_agent', ticket_id: ticket.id, assign: { agent_id: agent.id, instructions: null } }, 'run_agents')],
    };
  }
  if (/(new|create|add|open) (a )?tab/.test(t)) {
    const nm = cap(c.text.match(/tab (?:called |named |for )?["']?([\w -]{2,20})/i)?.[1]?.trim() ?? 'New tab');
    return { reply: `A fresh board for ${nm}.`, question: null, actions: [act(`Create the tab "${nm}"`, { action: 'create_tab', tab: { name: nm, icon: null, color: null, repo_id: null, columns: null } }, 'manage_workspace')] };
  }
  if (/\b(ticket|task|bug|fix|add|create|todo|implement|write|improve|investigate)\b/.test(t) && tab) {
    const title = cap(clean(c.text).replace(/\b(in|on) the \w+ tab\b/i, '').slice(0, 100));
    const prio = /urgent|asap|critical/.test(t) ? 'urgent' : /important|high/.test(t) ? 'high' : 'normal';
    const actions = [
      act(`Pin "${title}" in ${tab.name}`, { action: 'create_ticket', ticket: { tab_id: tab.id, column_id: null, title, body: c.text, tags: /bug|fix/.test(t) ? ['bug'] : [], priority: prio, color: null, repo_id: null } }, 'edit_tickets'),
    ];
    return { reply: agent ? `Pinned. Drag ${agent.name} onto it when you're ready.` : 'Pinned to the board.', question: null, actions };
  }
  return {
    reply: '',
    actions: [],
    question: `I'm not sure what to do with "${c.text}". Try "create a ticket to …", "send Boba Dev on T-3", "every Monday at 9, Pinky audits SEO and files tickets", or "sync the app repo".`,
  };
}

export const DAY_NAMES = DAYS;

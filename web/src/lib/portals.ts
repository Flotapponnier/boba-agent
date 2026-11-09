import type { ConnectorKind, McpTransport } from '../api/types';

export interface PortalPreset {
  kind: ConnectorKind;
  name: string;
  /** Icon value for `<Glyph>`: an icon name or `brand:<name>`. */
  icon: string;
  color: string;
  blurb: string;
  transport: McpTransport;
  /** Secret keys the preset expects (values are write-only). */
  secrets: { key: string; hint: string }[];
  docs?: string;
}

/** Presets only pre-fill the transport; every connector is an MCP server under the hood. */
export const PRESETS: PortalPreset[] = [
  {
    kind: 'google_search_console',
    name: 'Search Console',
    icon: 'brand:search-console',
    color: '#4285f4',
    blurb: 'Clicks, impressions, queries and indexing issues for your sites.',
    transport: { type: 'stdio', command: 'npx', args: ['-y', 'mcp-server-gsc'] },
    secrets: [{ key: 'GOOGLE_APPLICATION_CREDENTIALS_JSON', hint: 'Service account JSON with Search Console access' }],
  },
  {
    kind: 'google_workspace',
    name: 'Google Workspace',
    icon: 'brand:google',
    color: '#34a853',
    blurb: 'Gmail, Calendar, Drive and Docs.',
    transport: { type: 'stdio', command: 'npx', args: ['-y', '@googleworkspace/mcp'] },
    secrets: [
      { key: 'GOOGLE_CLIENT_ID', hint: 'OAuth client id' },
      { key: 'GOOGLE_CLIENT_SECRET', hint: 'OAuth client secret' },
      { key: 'GOOGLE_REFRESH_TOKEN', hint: 'Refresh token for the shared account' },
    ],
  },
  {
    kind: 'linear',
    name: 'Linear',
    icon: 'brand:linear',
    color: '#5e6ad2',
    blurb: 'Read and file Linear issues and projects.',
    transport: { type: 'http', url: 'https://mcp.linear.app/mcp', header_secrets: { Authorization: 'LINEAR_API_KEY' } },
    secrets: [{ key: 'LINEAR_API_KEY', hint: 'Personal or workspace API key' }],
  },
  {
    kind: 'slack',
    name: 'Slack',
    icon: 'slack',
    color: '#4a154b',
    blurb: 'Read channels and post updates.',
    transport: { type: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-slack'] },
    secrets: [
      { key: 'SLACK_BOT_TOKEN', hint: 'xoxb-… bot token' },
      { key: 'SLACK_TEAM_ID', hint: 'Workspace id (T…)' },
    ],
  },
  {
    kind: 'sentry',
    name: 'Sentry',
    icon: 'brand:sentry',
    color: '#362d59',
    blurb: 'Errors and issues, to turn crashes into tickets.',
    transport: { type: 'http', url: 'https://mcp.sentry.dev/mcp', header_secrets: { Authorization: 'SENTRY_TOKEN' } },
    secrets: [{ key: 'SENTRY_TOKEN', hint: 'Auth token with issue:read' }],
  },
  {
    kind: 'notion',
    name: 'Notion',
    icon: 'brand:notion',
    color: '#000000',
    blurb: 'Search and read pages and databases.',
    transport: { type: 'http', url: 'https://mcp.notion.com/mcp', header_secrets: { Authorization: 'NOTION_TOKEN' } },
    secrets: [{ key: 'NOTION_TOKEN', hint: 'Integration token' }],
  },
  {
    kind: 'web_search',
    name: 'Web search',
    icon: 'brand:brave',
    color: '#ff7a00',
    blurb: 'Search the web and read pages.',
    transport: { type: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-brave-search'] },
    secrets: [{ key: 'BRAVE_API_KEY', hint: 'Brave Search API key' }],
  },
  {
    kind: 'postgres',
    name: 'Postgres',
    icon: 'brand:postgres',
    color: '#336791',
    blurb: 'Read-only SQL on a database.',
    transport: { type: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-postgres', '${DATABASE_URL}'] },
    secrets: [{ key: 'DATABASE_URL', hint: 'postgres://readonly@host/db' }],
  },
  {
    kind: 'github',
    name: 'GitHub',
    icon: 'brand:github',
    color: '#24292f',
    blurb: 'Issues, PRs and code search beyond the repo sync.',
    transport: { type: 'http', url: 'https://api.githubcopilot.com/mcp/', header_secrets: { Authorization: 'GITHUB_TOKEN' } },
    secrets: [{ key: 'GITHUB_TOKEN', hint: 'Fine-grained token' }],
  },
  {
    kind: 'custom_mcp',
    name: 'Custom MCP',
    icon: 'brand:mcp',
    color: '#9b59b6',
    blurb: 'Any MCP server: a command on the runner or a remote URL.',
    transport: { type: 'stdio', command: '', args: [] },
    secrets: [],
  },
];

export const preset = (k: ConnectorKind) => PRESETS.find((p) => p.kind === k) ?? (PRESETS[PRESETS.length - 1] as PortalPreset);

export function transportSummary(t: McpTransport): string {
  return t.type === 'stdio' ? `$ ${[t.command, ...t.args].join(' ')}`.trim() : t.url;
}

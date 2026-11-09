import { memo, useState, type CSSProperties } from 'react';
import { api } from '../../api/client';
import type { Connector, ConnectorKind, ConnectorTestResult, McpTransport } from '../../api/types';
import { sfx } from '../../audio/audio';
import { PRESETS, preset, transportSummary } from '../../lib/portals';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { useAgentsList, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';
import { UNAVAILABLE, useAvailable } from '../../store/features';
import { Sprite } from '../ui/Sprite';

const PortalCard = memo(function PortalCard({ c }: { c: Connector }) {
  const p = preset(c.kind);
  const agents = useAgentsList().filter((a) => a.connector_ids.includes(c.id));
  const show = useUi((u) => u.show);
  const canManage = useCan('manage_automations');
  const [test, setTest] = useState<ConnectorTestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const run = async () => {
    setTesting(true);
    const r = await attempt(() => api.testConnector(c.id));
    setTesting(false);
    if (r) {
      setTest(r);
      sfx(r.ok ? 'pearl' : 'blocked', 0.6);
    }
  };
  return (
    <article className="portal sheet" style={{ '--pc': p.color } as CSSProperties}>
      <div className="portal-ring" aria-hidden="true">
        <span>{p.icon}</span>
      </div>
      <div className="portal-body">
        <h3>{c.name}</h3>
        <div className="hint-text">{p.name}</div>
        <code className="portal-transport">{transportSummary(c.transport)}</code>
        <div className="chips" style={{ marginTop: 4 }}>
          {c.secret_keys.map((k) => (
            <span key={k} className="chip" title="Stored encrypted. Values are never shown again.">
              🔒 {k}
            </span>
          ))}
          {c.secret_keys.length === 0 && <span className="hint-text">No secret</span>}
        </div>
        {c.allowed_tools.length > 0 && <div className="hint-text">Tools: {c.allowed_tools.join(', ')}</div>}
        <div className="row" style={{ marginTop: 6 }}>
          {agents.map((a) => (
            <span key={a.id} title={a.name}>
              <Sprite sprite={a.sprite} color={a.color} size={26} />
            </span>
          ))}
          {agents.length === 0 && <span className="hint-text">No boba uses this portal yet</span>}
        </div>
        {test && (
          <div className={`portal-test ${test.ok ? 'ok' : 'bad'}`} role="status">
            {test.ok ? (
              <>
                ✓ Portal open · {test.tools.length} tool{test.tools.length === 1 ? '' : 's'}
                <div className="chips" style={{ marginTop: 4 }}>
                  {test.tools.map((t) => (
                    <code key={t}>{t}</code>
                  ))}
                </div>
              </>
            ) : (
              <>✗ {test.error}</>
            )}
          </div>
        )}
        {canManage && (
          <div className="row end" style={{ marginTop: 8 }}>
            <button className="btn sm" disabled={testing} onClick={() => void run()}>
              {testing ? 'Opening…' : '⚡ Test'}
            </button>
            <button className="btn sm" onClick={() => show({ kind: 'portal-edit', id: c.id })}>
              ✎ Edit
            </button>
          </div>
        )}
      </div>
    </article>
  );
});

export function Portals() {
  const connectors = useBoard((s) => s.connectors);
  const show = useUi((u) => u.show);
  const canManage = useCan('manage_automations');
  const avail = useAvailable('connectors');
  const list = Object.values(connectors).sort((a, b) => a.name.localeCompare(b.name));
  return (
    <section className="page wood" aria-label="Portals">
      <div className="page-head">
        <div>
          <div className="sign" style={{ textAlign: 'left' }}>
            🌀 PORTALS
          </div>
          <p className="page-sub">Portals are MCP connectors that open tools to your bobas: Search Console, Sentry, Linear, Slack… Give them to an agent in its persona.</p>
        </div>
      </div>
      <div className="portal-grid">
        {list.map((c) => (
          <PortalCard key={c.id} c={c} />
        ))}
      </div>
      {canManage && (
        <>
          <div className="section-title" style={{ color: 'var(--gold)' }}>
            OPEN A NEW PORTAL
          </div>
          <div className="preset-grid">
            {PRESETS.map((p) => (
              <button key={p.kind} className="preset" disabled={!avail} title={avail ? undefined : UNAVAILABLE} style={{ '--pc': p.color } as CSSProperties} onClick={() => show({ kind: 'portal-new', preset: p.kind })}>
                <span className="preset-ico">{p.icon}</span>
                <b>{p.name}</b>
                <small>{p.blurb}</small>
              </button>
            ))}
          </div>
        </>
      )}
    </section>
  );
}

export function PortalEditor({ connectorId, presetKind }: { connectorId?: string; presetKind?: string }) {
  const existing = useBoard((s) => (connectorId ? s.connectors[connectorId] : undefined));
  const show = useUi((u) => u.show);
  const p = preset((existing?.kind ?? presetKind ?? 'custom_mcp') as ConnectorKind);
  const [name, setName] = useState(existing?.name ?? (p.kind === 'custom_mcp' ? '' : p.name));
  const [tr, setTr] = useState<McpTransport>(existing?.transport ?? p.transport);
  const [argsText, setArgsText] = useState(tr.type === 'stdio' ? tr.args.join(' ') : '');
  const [secrets, setSecrets] = useState<{ key: string; value: string; stored: boolean; remove?: boolean }[]>(() =>
    existing ? existing.secret_keys.map((k) => ({ key: k, value: '', stored: true })) : p.secrets.map((s) => ({ key: s.key, value: '', stored: false })),
  );
  const [tools, setTools] = useState((existing?.allowed_tools ?? []).join(', '));
  const [busy, setBusy] = useState(false);
  const close = () => show(null);
  const hint = (k: string) => p.secrets.find((s) => s.key === k)?.hint;

  const save = async () => {
    setBusy(true);
    const transport: McpTransport = tr.type === 'stdio' ? { type: 'stdio', command: tr.command.trim(), args: argsText.trim() ? argsText.trim().split(/\s+/) : [] } : tr;
    const map: Record<string, string> = {};
    for (const s of secrets) {
      if (!s.key.trim()) continue;
      if (s.remove) map[s.key.trim()] = '';
      else if (s.value) map[s.key.trim()] = s.value;
    }
    const allowed = tools
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean);
    const c = await attempt(() =>
      existing
        ? api.updateConnector(existing.id, { name: name.trim(), transport, secrets: map, allowed_tools: allowed })
        : api.createConnector({ name: name.trim(), kind: p.kind, transport, secrets: map, allowed_tools: allowed }),
    );
    setBusy(false);
    if (c) {
      useBoard.getState().upsert({ entity: 'connector', data: c });
      sfx('done', 0.6);
      toast(existing ? `${c.name} updated` : `Portal ${c.name} is open! 🌀`, 'gold');
      close();
    }
  };

  return (
    <Modal title={existing ? `EDIT ${existing.name.toUpperCase()}` : `OPEN ${p.name.toUpperCase()}`} onClose={close} wide>
      <div className="row" style={{ marginBottom: 10 }}>
        <span className="preset-ico" style={{ '--pc': p.color } as CSSProperties}>
          {p.icon}
        </span>
        <p className="hint-text grow">{p.blurb}</p>
      </div>
      <label className="field">
        <span className="label">Name (agents see tools as mcp__name__tool)</span>
        <input autoFocus value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder="Search Console" />
      </label>
      <div className="field" style={{ marginTop: 10 }}>
        <span className="label">Transport</span>
        <div className="segmented" role="group">
          <button type="button" aria-pressed={tr.type === 'stdio'} onClick={() => setTr({ type: 'stdio', command: tr.type === 'stdio' ? tr.command : 'npx', args: [] })}>
            Command on the runner
          </button>
          <button type="button" aria-pressed={tr.type === 'http'} onClick={() => setTr({ type: 'http', url: tr.type === 'http' ? tr.url : 'https://', header_secrets: tr.type === 'http' ? tr.header_secrets : {} })}>
            Remote URL
          </button>
        </div>
      </div>
      {tr.type === 'stdio' ? (
        <div className="field-row" style={{ marginTop: 8 }}>
          <label className="field">
            <span className="label">Command</span>
            <input value={tr.command} onChange={(e) => setTr({ ...tr, command: e.target.value })} placeholder="npx" />
          </label>
          <label className="field">
            <span className="label">Arguments</span>
            <input value={argsText} onChange={(e) => setArgsText(e.target.value)} placeholder="-y @scope/mcp-server" />
          </label>
        </div>
      ) : (
        <>
          <label className="field" style={{ marginTop: 8 }}>
            <span className="label">URL</span>
            <input value={tr.url} onChange={(e) => setTr({ ...tr, url: e.target.value })} placeholder="https://mcp.example.com/mcp" />
          </label>
          <div className="field">
            <span className="label">Auth headers (header → secret key, sent as "Bearer &lt;value&gt;")</span>
            {Object.entries(tr.header_secrets).map(([h, k]) => (
              <div key={h} className="row">
                <code>{h}</code> → <code>{k}</code>
                <button
                  type="button"
                  className="btn sm ghost"
                  onClick={() => {
                    const next = { ...tr.header_secrets };
                    delete next[h];
                    setTr({ ...tr, header_secrets: next });
                  }}
                >
                  ×
                </button>
              </div>
            ))}
            <button type="button" className="btn sm ghost" style={{ alignSelf: 'flex-start' }} onClick={() => setTr({ ...tr, header_secrets: { ...tr.header_secrets, Authorization: secrets[0]?.key || 'TOKEN' } })}>
              + Authorization header
            </button>
          </div>
        </>
      )}
      <div className="section-title">SECRETS (WRITE-ONLY)</div>
      <p className="hint-text">Values are encrypted on the server and never shown again. Leave a stored secret empty to keep it.</p>
      {secrets.map((s, i) => (
        <div key={i} className="secret-row">
          <input value={s.key} disabled={s.stored} onChange={(e) => setSecrets((xs) => xs.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))} placeholder="KEY_NAME" aria-label="Secret name" />
          <input
            type="password"
            autoComplete="off"
            value={s.value}
            disabled={s.remove}
            onChange={(e) => setSecrets((xs) => xs.map((x, j) => (j === i ? { ...x, value: e.target.value } : x)))}
            placeholder={s.stored ? '•••••••• stored (type to replace)' : hint(s.key) ?? 'value'}
            aria-label={`Value of ${s.key}`}
          />
          <button type="button" className="icon-btn" style={{ background: 'var(--red)' }} aria-label={`Remove ${s.key}`} onClick={() => setSecrets((xs) => (s.stored ? xs.map((x, j) => (j === i ? { ...x, remove: !x.remove, value: '' } : x)) : xs.filter((_, j) => j !== i)))}>
            {s.remove ? '↺' : '✕'}
          </button>
        </div>
      ))}
      <button type="button" className="btn sm ghost" onClick={() => setSecrets((xs) => [...xs, { key: '', value: '', stored: false }])}>
        + Secret
      </button>
      <label className="field" style={{ marginTop: 10 }}>
        <span className="label">Allowed tools (comma separated, empty = all)</span>
        <input value={tools} onChange={(e) => setTools(e.target.value)} placeholder="list_issues, get_issue" />
      </label>
      <div className="row between" style={{ marginTop: 14 }}>
        <div>
          {existing && (
            <button
              className="btn sm red"
              onClick={async () => {
                if (!confirm(`Close the portal ${existing.name}? Agents lose its tools.`)) return;
                const ok = await attempt(() => api.deleteConnector(existing.id).then(() => true));
                if (ok) {
                  useBoard.getState().remove('connector', existing.id);
                  close();
                }
              }}
            >
              Close portal
            </button>
          )}
        </div>
        <div className="row">
          <button className="btn ghost" onClick={close}>
            Cancel
          </button>
          <button className="btn gold" disabled={busy || !name.trim()} onClick={() => void save()}>
            {existing ? 'Save' : '🌀 Open portal'}
          </button>
        </div>
      </div>
    </Modal>
  );
}

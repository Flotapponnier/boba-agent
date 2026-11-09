import { useMemo, useState, type CSSProperties, type FormEvent } from 'react';
import type { PermissionMode } from '../../api/types';
import { sfx } from '../../audio/audio';
import { MODELS, ROLE_PRESETS, SPRITES, SWATCHES, asset } from '../../lib/game';
import { createAgent, updateAgent } from '../../store/actions';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';
import { Sprite } from '../ui/Sprite';

const PROMPTS: Record<string, string> = {
  Dev: 'You are {name}, a careful full-stack developer. Work only in your worktree. Write a failing test first, keep the diff focused on the ticket, and open a clean PR with a clear summary.',
  Reviewer:
    'You are {name}, a strict but kind code reviewer. Read the whole diff, cite file:line for every finding with the failure scenario, check that tests cover the change, and never merge yourself.',
  SEO: 'You are {name}, an SEO specialist. Audit titles, meta descriptions, canonicals, Open Graph, sitemap and llms.txt. Fix what you can and list the rest as a table.',
  QA: 'You are {name}, a QA engineer. Reproduce the bug first, write end-to-end tests, and report exactly what you saw.',
  DevOps: 'You are {name}, a DevOps engineer. You own CI, containers and deployments. Prefer small, reversible changes and explain the rollback.',
  Architect: 'You are {name}, a software architect. Design before coding, write the trade-offs down, and measure performance before and after.',
  Designer: 'You are {name}, a product designer who codes. Respect the design tokens, keep things accessible, and show before/after.',
  Writer: 'You are {name}, a technical writer. Clear, short sentences. Examples first.',
};

const MODES: { id: PermissionMode; label: string; hint: string }[] = [
  { id: 'acceptEdits', label: 'Accept edits', hint: 'Edits files freely, asks before risky commands' },
  { id: 'default', label: 'Ask first', hint: 'Asks the team before every tool that changes things' },
  { id: 'plan', label: 'Read only', hint: 'Plan mode: reads and reasons, never writes (reviewers)' },
  { id: 'bypassPermissions', label: 'Bypass', hint: 'Admins only, server must run in a container' },
];

export function AgentEditor({ agentId }: { agentId?: string }) {
  const existing = useBoard((s) => (agentId ? s.agents[agentId] : undefined));
  const skills = useBoard((s) => s.skills);
  const role = useBoard((s) => s.me?.role);
  const show = useUi((u) => u.show);
  const open = useUi((u) => u.open);
  const [f, setF] = useState(() => ({
    name: existing?.name ?? '',
    role: existing?.role ?? 'Dev',
    sprite: existing?.sprite ?? 'boba',
    color: existing?.color ?? '#f39c12',
    tagline: existing?.tagline ?? '',
    system_prompt: existing?.system_prompt ?? '',
    skill_ids: existing?.skill_ids ?? [],
    connector_ids: existing?.connector_ids ?? [],
    board_tools: existing?.board_tools ?? false,
    model: existing?.model ?? null,
    permission_mode: existing?.permission_mode ?? ('acceptEdits' as PermissionMode),
    allowed_tools: existing?.allowed_tools ?? [],
    max_turns: existing?.max_turns ?? null,
  }));
  const [skillQ, setSkillQ] = useState('');
  const [toolIn, setToolIn] = useState('');
  const [busy, setBusy] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const close = () => show(null);
  const set = <K extends keyof typeof f>(k: K, v: (typeof f)[K]) => setF((x) => ({ ...x, [k]: v }));
  const isAdmin = role === 'admin' || role === 'owner';

  const skillList = useMemo(
    () =>
      Object.values(skills)
        .filter((s) => !skillQ || s.name.includes(skillQ.toLowerCase()) || s.description.toLowerCase().includes(skillQ.toLowerCase()))
        .sort((a, b) => Number(f.skill_ids.includes(b.id)) - Number(f.skill_ids.includes(a.id)) || a.name.localeCompare(b.name)),
    [skills, skillQ, f.skill_ids],
  );

  const applyRolePrompt = (r: string) => {
    const tpl = PROMPTS[r];
    if (!tpl) return;
    set('system_prompt', tpl.replace('{name}', f.name.trim() || 'a boba'));
    if (r === 'Reviewer') set('permission_mode', 'plan');
  };

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    const body = {
      ...f,
      name: f.name.trim(),
      role: f.role.trim() || 'Dev',
      tagline: f.tagline.trim(),
      model: f.model,
      max_turns: f.max_turns,
    };
    const a = existing ? await updateAgent(existing.id, body) : await createAgent(body);
    setBusy(false);
    if (a) {
      close();
      if (!existing) open({ kind: 'agent', id: a.id });
    }
  }

  return (
    <Modal title={existing ? `EDIT ${existing.name.toUpperCase()}` : 'RECRUIT A BOBA'} onClose={close} wide>
      <form onSubmit={submit} className="creator">
        <div className="creator-stage" style={{ '--ac': f.color } as CSSProperties}>
          <div className="pedestal">
            <Sprite sprite={f.sprite} color={f.color} size={130} body status="working" />
          </div>
          <h3>{f.name || 'Your new boba'}</h3>
          <span className="role" style={{ background: f.color }}>
            {f.role || 'Role'}
          </span>
          {f.tagline && <p className="tagline">“{f.tagline}”</p>}
          <div className="chips" style={{ justifyContent: 'center' }}>
            {f.skill_ids.map((id) => (
              <span key={id} className="chip on">
                {skills[id]?.name ?? id}
              </span>
            ))}
          </div>
          <p className="hint-text">{MODELS.find((m) => m.id === f.model)?.label ?? f.model}</p>
          <img src={asset('sprites/widget/love_letter.webp')} alt="" width={48} style={{ opacity: 0.85 }} />
        </div>

        <div>
          <span className="label">Look</span>
          <div className="sprite-grid" role="radiogroup" aria-label="Character" style={{ marginTop: 4 }}>
            {SPRITES.map((s) => (
              <button
                type="button"
                key={s.key}
                className="sprite-opt"
                aria-pressed={f.sprite === s.key}
                aria-label={s.label}
                title={s.label}
                onClick={() => {
                  set('sprite', s.key);
                  sfx('pearl', 0.3);
                }}
              >
                <img src={asset(s.body)} alt="" />
              </button>
            ))}
          </div>
          <div className="swatches" style={{ marginTop: 8 }} aria-label="Colour">
            {SWATCHES.map((c) => (
              <button type="button" key={c} className="swatch" style={{ background: c }} aria-pressed={f.color === c} aria-label={`Colour ${c}`} onClick={() => set('color', c)} />
            ))}
            <input type="color" value={f.color} onChange={(e) => set('color', e.target.value)} aria-label="Custom colour" />
          </div>

          <div className="field-row" style={{ marginTop: 12 }}>
            <label className="field">
              <span className="label">Name</span>
              <input required autoFocus maxLength={32} value={f.name} onChange={(e) => set('name', e.target.value)} placeholder="Boba Security" />
            </label>
            <label className="field">
              <span className="label">Role</span>
              <input
                list="role-presets"
                maxLength={20}
                value={f.role}
                onChange={(e) => {
                  set('role', e.target.value);
                  if (!f.system_prompt.trim()) applyRolePrompt(e.target.value);
                }}
              />
              <datalist id="role-presets">
                {ROLE_PRESETS.map((r) => (
                  <option key={r} value={r} />
                ))}
              </datalist>
            </label>
          </div>
          <label className="field">
            <span className="label">Tagline</span>
            <input maxLength={80} value={f.tagline} onChange={(e) => set('tagline', e.target.value)} placeholder="Ships it with tests, every time." />
          </label>
          <label className="field">
            <span className="label row between">
              <span>System prompt (appended to Claude Code's)</span>
              {PROMPTS[f.role] && (
                <button type="button" className="btn sm ghost" onClick={() => applyRolePrompt(f.role)}>
                  Use {f.role} template
                </button>
              )}
            </span>
            <textarea value={f.system_prompt} onChange={(e) => set('system_prompt', e.target.value)} placeholder="You are …" style={{ minHeight: 120 }} />
          </label>

          <div className="field">
            <span className="label">Skills ({f.skill_ids.length})</span>
            <input type="search" placeholder="Search skills…" value={skillQ} onChange={(e) => setSkillQ(e.target.value)} aria-label="Search skills" />
            <div className="skill-pick" role="group" aria-label="Skills">
              {skillList.map((s) => (
                <label key={s.id} className="skill-opt">
                  <input
                    type="checkbox"
                    checked={f.skill_ids.includes(s.id)}
                    onChange={(e) => set('skill_ids', e.target.checked ? [...f.skill_ids, s.id] : f.skill_ids.filter((x) => x !== s.id))}
                  />
                  <span>
                    {s.name}
                    <small>{s.description}</small>
                  </span>
                  <span className={`src ${s.source}`}>{s.source}</span>
                </label>
              ))}
              {skillList.length === 0 && <p className="muted" style={{ padding: 8 }}>No skill found.</p>}
            </div>
          </div>

          <div className="field-row" style={{ marginTop: 10 }}>
            <label className="field">
              <span className="label">Model</span>
              <select value={f.model ?? ''} onChange={(e) => set('model', e.target.value || null)}>
                {MODELS.map((m) => (
                  <option key={m.id ?? 'default'} value={m.id ?? ''}>
                    {m.label}
                  </option>
                ))}
                {f.model && !MODELS.some((m) => m.id === f.model) && <option value={f.model}>{f.model}</option>}
              </select>
            </label>
            <label className="field">
              <span className="label">Permissions</span>
              <select value={f.permission_mode} onChange={(e) => set('permission_mode', e.target.value as PermissionMode)}>
                {MODES.map((m) => (
                  <option key={m.id} value={m.id} disabled={m.id === 'bypassPermissions' && !isAdmin}>
                    {m.label}
                  </option>
                ))}
              </select>
              <span className="hint-text">{MODES.find((m) => m.id === f.permission_mode)?.hint}</span>
            </label>
          </div>

          <button type="button" className="btn sm ghost" style={{ marginTop: 10 }} onClick={() => setAdvanced((v) => !v)} aria-expanded={advanced}>
            {advanced ? '▾' : '▸'} Advanced
          </button>
          {advanced && (
            <div style={{ marginTop: 8 }}>
              <div className="field">
                <span className="label">Extra allowed tools</span>
                <div className="chips">
                  {f.allowed_tools.map((t) => (
                    <span key={t} className="chip x-able">
                      <code>{t}</code>
                      <button type="button" aria-label={`Remove ${t}`} onClick={() => set('allowed_tools', f.allowed_tools.filter((x) => x !== t))}>
                        ×
                      </button>
                    </span>
                  ))}
                </div>
                <div className="row">
                  <input
                    className="grow"
                    value={toolIn}
                    onChange={(e) => setToolIn(e.target.value)}
                    placeholder="Bash(git diff:*)"
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        if (toolIn.trim() && !f.allowed_tools.includes(toolIn.trim())) set('allowed_tools', [...f.allowed_tools, toolIn.trim()]);
                        setToolIn('');
                      }
                    }}
                  />
                </div>
              </div>
              <label className="field">
                <span className="label">Max turns (empty = no limit)</span>
                <input type="number" min={1} max={500} value={f.max_turns ?? ''} onChange={(e) => set('max_turns', e.target.value ? Number(e.target.value) : null)} />
              </label>
            </div>
          )}

          <div className="row end" style={{ marginTop: 16 }}>
            <button type="button" className="btn ghost" onClick={close}>
              Cancel
            </button>
            <button className="btn gold" disabled={busy || !f.name.trim()}>
              {existing ? 'Save' : '🎉 Recruit!'}
            </button>
          </div>
        </div>
      </form>
    </Modal>
  );
}

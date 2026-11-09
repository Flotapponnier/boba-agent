import { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { SkillDetail, SkillSource } from '../../api/types';
import { bodyUrl } from '../../lib/game';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { useAgentsList, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Markdown } from '../ui/Markdown';
import { Drawer } from '../ui/Overlay';
import { Icon } from '../ui/Icon';

const SOURCES: { key: SkillSource; label: string; hint: string }[] = [
  { key: 'boba', label: 'Boba', hint: 'Managed here, editable' },
  { key: 'project', label: 'Project', hint: '.claude/skills in a repo' },
  { key: 'user', label: 'User', hint: '~/.claude/skills on the runner' },
  { key: 'plugin', label: 'Plugin', hint: 'From installed plugins' },
];

function Editor({ initial, onDone }: { initial?: SkillDetail; onDone: (id?: string) => void }) {
  const [f, setF] = useState({ name: initial?.skill.name ?? '', description: initial?.skill.description ?? '', body: initial?.body ?? '# My skill\n\nWhen to use it, then the steps.\n' });
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    const s = await attempt(() => (initial ? api.updateSkill(initial.skill.id, f) : api.createSkill(f)));
    setBusy(false);
    if (s) {
      useBoard.getState().upsert({ entity: 'skill', data: s });
      toast(`Skill ${s.name} saved`, 'good');
      onDone(s.id);
    }
  };
  return (
    <div>
      <label className="field">
        <span className="label">Name</span>
        <input value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} disabled={!!initial} placeholder="kebab-case-name" />
      </label>
      <label className="field">
        <span className="label">Description (Claude reads this to decide when to use it)</span>
        <input value={f.description} onChange={(e) => setF({ ...f, description: e.target.value })} />
      </label>
      <label className="field">
        <span className="label">SKILL.md body</span>
        <textarea className="code" value={f.body} onChange={(e) => setF({ ...f, body: e.target.value })} style={{ minHeight: 280 }} />
      </label>
      <div className="row end" style={{ marginTop: 10 }}>
        <button className="btn ghost" onClick={() => onDone(initial?.skill.id)}>
          Cancel
        </button>
        <button className="btn gold" disabled={busy || !f.name.trim() || !f.description.trim()} onClick={() => void save()}>
          Save skill
        </button>
      </div>
    </div>
  );
}

export function SkillsDrawer({ initialId }: { initialId?: string }) {
  const skills = useBoard((s) => s.skills);
  const agents = useAgentsList();
  const close = useUi((u) => u.open);
  const canManage = useCan('manage_skills');
  const [sel, setSel] = useState<string | null>(initialId ?? null);
  const [detail, setDetail] = useState<SkillDetail | null>(null);
  const [mode, setMode] = useState<'view' | 'edit' | 'new'>('view');
  const [q, setQ] = useState('');

  useEffect(() => {
    if (!sel) {
      setDetail(null);
      return;
    }
    let alive = true;
    setDetail(null);
    api
      .skill(sel)
      .then((d) => alive && setDetail(d))
      .catch(() => alive && setDetail(null));
    return () => {
      alive = false;
    };
  }, [sel]);

  const grouped = useMemo(() => {
    const list = Object.values(skills).filter((s) => !q || s.name.includes(q.toLowerCase()) || s.description.toLowerCase().includes(q.toLowerCase()));
    return SOURCES.map((src) => ({ ...src, skills: list.filter((s) => s.source === src.key).sort((a, b) => a.name.localeCompare(b.name)) }));
  }, [skills, q]);

  const usedBy = (id: string) => agents.filter((a) => a.skill_ids.includes(id));

  return (
    <Drawer title="SKILL LIBRARY" onClose={() => close(null)}>
      <div className="sheet">
        {mode === 'new' ? (
          <Editor
            onDone={(id) => {
              setMode('view');
              if (id) setSel(id);
            }}
          />
        ) : sel ? (
          <div>
            <button className="btn sm ghost" onClick={() => { setSel(null); setMode('view'); }}>
              <Icon name="arrow-left" size={12} /> All skills
            </button>
            {!detail ? (
              <p className="muted" style={{ marginTop: 10 }}>
                Loading…
              </p>
            ) : mode === 'edit' ? (
              <div style={{ marginTop: 10 }}>
                <Editor initial={detail} onDone={() => { setMode('view'); setSel(null); setTimeout(() => setSel(detail.skill.id), 0); }} />
              </div>
            ) : (
              <div style={{ marginTop: 10 }}>
                <div className="row">
                  <h2 className="pixel-title grow" style={{ margin: 0 }}>
                    {detail.skill.name}
                  </h2>
                  <span className={`src ${detail.skill.source}`}>{detail.skill.source}</span>
                </div>
                <p className="hint-text">{detail.skill.description}</p>
                <p className="hint-text">
                  <code>{detail.skill.path}</code>
                </p>
                {usedBy(detail.skill.id).length > 0 && (
                  <div className="row" style={{ marginTop: 6 }}>
                    <span className="label">Used by</span>
                    {usedBy(detail.skill.id).map((a) => (
                      <span key={a.id} className="chip">
                        <img src={bodyUrl(a.sprite)} alt="" width={16} height={16} /> {a.name}
                      </span>
                    ))}
                  </div>
                )}
                <hr className="dash" />
                <Markdown text={detail.body} />
                {detail.files.length > 1 && (
                  <>
                    <div className="section-title">FILES</div>
                    <div className="chips">
                      {detail.files.map((f) => (
                        <code key={f}>{f}</code>
                      ))}
                    </div>
                  </>
                )}
                {canManage && detail.skill.editable && (
                  <div className="row end" style={{ marginTop: 12 }}>
                    <button className="btn gold sm" onClick={() => setMode('edit')}>
                      <Icon name="edit" size={14} /> Edit
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        ) : (
          <>
            <div className="row" style={{ marginBottom: 10 }}>
              <input type="search" className="grow" placeholder="Search skills…" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search skills" />
              {canManage && (
                <>
                  <button
                    className="btn sm"
                    onClick={async () => {
                      const list = await attempt(() => api.rescanSkills());
                      if (list) {
                        list.forEach((s) => useBoard.getState().upsert({ entity: 'skill', data: s }));
                        toast(`Found ${list.length} skills`, 'good');
                      }
                    }}
                    title="Re-scan skill folders on the runners"
                  >
                    ⟳ Rescan
                  </button>
                  <button className="btn gold sm" onClick={() => setMode('new')}>
                    + New skill
                  </button>
                </>
              )}
            </div>
            {grouped.map((g) =>
              g.skills.length ? (
                <div key={g.key}>
                  <div className="section-title">
                    <span className={`src ${g.key}`}>{g.label}</span> {g.hint}
                  </div>
                  <div className="skill-list">
                    {g.skills.map((s) => (
                      <button key={s.id} className="skill-card" onClick={() => setSel(s.id)}>
                        <b>{s.name}</b>
                        <Icon name={s.editable ? 'edit' : 'lock'} size={14} title={s.editable ? 'Editable' : 'Read-only'} />
                        <p>{s.description}</p>
                        {usedBy(s.id).length > 0 && (
                          <span className="users">
                            {usedBy(s.id).map((a) => (
                              <img key={a.id} src={bodyUrl(a.sprite)} alt={a.name} title={a.name} />
                            ))}
                          </span>
                        )}
                      </button>
                    ))}
                  </div>
                </div>
              ) : null,
            )}
          </>
        )}
      </div>
    </Drawer>
  );
}

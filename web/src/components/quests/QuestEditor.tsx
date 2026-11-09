import { useEffect, useMemo, useState, type FormEvent } from 'react';
import { api } from '../../api/client';
import type { JobOutput, SchedulePreview } from '../../api/types';
import { DAYS, describe, fromSchedule, localTz, nextRuns, timezones, toSchedule, validateCron, type Builder } from '../../lib/schedule';
import { attempt } from '../../store/actions';
import { toast } from '../../store/bus';
import { useAgentsList, useTabs } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';
import { Sprite } from '../ui/Sprite';
import { OUTPUT_LABEL } from './QuestBoard';
import { Icon } from '../ui/Icon';
import { glyphKey } from '../../lib/glyph';
import type { IconName } from '../../lib/icons';

const ICONS: IconName[] = ['quest', 'search', 'shield', 'siren', 'clipboard', 'chart', 'eraser', 'tools', 'megaphone', 'bug', 'globe', 'sword', 'mail', 'database', 'calendar', 'trophy'];
const MODES: { id: Builder['mode']; label: string }[] = [
  { id: 'manual', label: 'Manual' },
  { id: 'interval', label: 'Every N min' },
  { id: 'daily', label: 'Daily' },
  { id: 'weekly', label: 'Weekly' },
  { id: 'cron', label: 'Cron' },
];

function defaultFor(mode: Builder['mode'], prev: Builder): Builder {
  const tz = 'tz' in prev ? prev.tz : localTz();
  const time = 'time' in prev ? prev.time : '09:00';
  switch (mode) {
    case 'manual':
      return { mode };
    case 'interval':
      return { mode, minutes: 60 };
    case 'daily':
      return { mode, time, tz };
    case 'weekly':
      return { mode, days: [1], time, tz };
    case 'cron':
      return { mode, expr: prev.mode === 'cron' ? prev.expr : '0 9 * * 1-5', tz };
  }
}

/** Friendly schedule builder with a live "next 5 runs" preview from the server. */
function ScheduleBuilder({ value, onChange }: { value: Builder; onChange: (b: Builder) => void }) {
  const [preview, setPreview] = useState<SchedulePreview | null>(null);
  const zones = useMemo(timezones, []);
  const schedule = toSchedule(value);
  const key = JSON.stringify(schedule);
  useEffect(() => {
    if (schedule.type === 'manual') {
      setPreview({ valid: true, error: null, next: [], description: describe(schedule) });
      return;
    }
    const local = schedule.type === 'cron' ? validateCron(schedule.expr) : null;
    if (local) {
      setPreview({ valid: false, error: local, next: [], description: '' });
      return;
    }
    let alive = true;
    const id = setTimeout(() => {
      api
        .previewSchedule(schedule)
        .then((p) => alive && setPreview(p))
        .catch(() => alive && setPreview({ valid: true, error: null, next: nextRuns(schedule).map((d) => d.toISOString()), description: describe(schedule) }));
    }, 250);
    return () => {
      alive = false;
      clearTimeout(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return (
    <div className="sched">
      <div className="segmented" role="group" aria-label="Schedule type">
        {MODES.map((m) => (
          <button type="button" key={m.id} aria-pressed={value.mode === m.id} onClick={() => onChange(defaultFor(m.id, value))}>
            {m.label}
          </button>
        ))}
      </div>
      <div className="row" style={{ marginTop: 8 }}>
        {value.mode === 'interval' && (
          <label className="row">
            Every
            <input type="number" min={5} step={5} value={value.minutes} onChange={(e) => onChange({ ...value, minutes: Number(e.target.value) })} style={{ width: 90 }} aria-label="Minutes" />
            minutes
            <span className="chips">
              {[15, 60, 240, 1440].map((m) => (
                <button type="button" key={m} className={`chip ${value.minutes === m ? 'on' : ''}`} onClick={() => onChange({ ...value, minutes: m })}>
                  {m < 60 ? `${m}m` : m < 1440 ? `${m / 60}h` : '1 day'}
                </button>
              ))}
            </span>
          </label>
        )}
        {value.mode === 'weekly' && (
          <div className="chips" role="group" aria-label="Days">
            {DAYS.map((d, i) => (
              <button
                type="button"
                key={d}
                className={`chip ${value.days.includes(i) ? 'on' : ''}`}
                aria-pressed={value.days.includes(i)}
                onClick={() => onChange({ ...value, days: value.days.includes(i) ? value.days.filter((x) => x !== i) : [...value.days, i] })}
              >
                {d}
              </button>
            ))}
          </div>
        )}
        {(value.mode === 'daily' || value.mode === 'weekly') && (
          <label className="row">
            at <input type="time" value={value.time} onChange={(e) => onChange({ ...value, time: e.target.value || '09:00' })} style={{ width: 120 }} aria-label="Time" />
          </label>
        )}
        {value.mode === 'cron' && (
          <label className="grow">
            <input value={value.expr} onChange={(e) => onChange({ ...value, expr: e.target.value })} placeholder="m h dom mon dow" aria-label="Cron expression" style={{ fontFamily: 'ui-monospace, monospace', fontSize: 15 }} />
          </label>
        )}
        {'tz' in value && (
          <label className="row grow">
            <span className="label">Timezone</span>
            <select value={value.tz} onChange={(e) => onChange({ ...value, tz: e.target.value })} style={{ width: 'auto', maxWidth: 220 }}>
              {[...new Set([value.tz, ...zones])].map((z) => (
                <option key={z} value={z}>
                  {z}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      {preview && (
        <div className={`sched-preview ${preview.valid ? '' : 'bad'}`} aria-live="polite">
          {preview.valid ? (
            <>
              <b>{preview.description}</b>
              {preview.next.length > 0 && (
                <ol>
                  {preview.next.map((d) => (
                    <li key={d}>{new Date(d).toLocaleString([], { weekday: 'short', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}</li>
                  ))}
                </ol>
              )}
            </>
          ) : (
            <span>{preview.error}</span>
          )}
        </div>
      )}
    </div>
  );
}

export function QuestEditor({ jobId }: { jobId?: string }) {
  const existing = useBoard((s) => (jobId ? s.jobs[jobId] : undefined));
  const repos = useBoard((s) => s.repos);
  const agents = useAgentsList();
  const tabs = useTabs();
  const show = useUi((u) => u.show);
  const setSpot = useUi((u) => u.setSpotlight);
  const [f, setF] = useState(() => ({
    name: existing?.name ?? '',
    icon: glyphKey(existing?.icon) ?? 'quest',
    description: existing?.description ?? '',
    agent_id: existing?.agent_id ?? agents.find((a) => a.role.toLowerCase() !== 'reviewer')?.id ?? agents[0]?.id ?? '',
    prompt: existing?.prompt ?? '',
    output: existing?.output ?? ('report' as JobOutput),
    tab_id: existing?.tab_id ?? tabs[0]?.id ?? '',
    repo_id: existing?.repo_id ?? '',
    enabled: existing?.enabled ?? true,
    skip_if_running: existing?.skip_if_running ?? true,
  }));
  const [sched, setSched] = useState<Builder>(() => fromSchedule(existing?.schedule));
  const [busy, setBusy] = useState(false);
  const close = () => show(null);
  const agent = agents.find((a) => a.id === f.agent_id);
  const schedErr = sched.mode === 'cron' ? validateCron(sched.expr) : sched.mode === 'interval' && sched.minutes < 5 ? 'At least 5 minutes' : sched.mode === 'weekly' && sched.days.length === 0 ? 'Pick at least one day' : null;
  const needsRepo = f.output === 'pull_request' && !f.repo_id;

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    const body = {
      name: f.name.trim(),
      description: f.description.trim() || null,
      icon: f.icon,
      agent_id: f.agent_id,
      prompt: f.prompt.trim(),
      schedule: toSchedule(sched),
      output: f.output,
      enabled: f.enabled,
      repo_id: f.repo_id || null,
      tab_id: f.output === 'tickets' ? f.tab_id || null : null,
      skip_if_running: f.skip_if_running,
    };
    const j = await attempt(() => (existing ? api.updateJob(existing.id, body) : api.createJob(body)));
    setBusy(false);
    if (j) {
      useBoard.getState().upsert({ entity: 'job', data: j });
      toast(existing ? `Quest "${j.name}" updated` : `Quest "${j.name}" posted on the board`, 'gold', agent?.sprite);
      setSpot(j.id);
      setTimeout(() => useUi.getState().setSpotlight(null), 2500);
      close();
    }
  }

  return (
    <Modal title={existing ? 'EDIT QUEST' : 'POST A QUEST'} onClose={close} wide>
      <form onSubmit={submit}>
        <div className="field-row">
          <label className="field">
            <span className="label">Quest name</span>
            <input required autoFocus maxLength={60} value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} placeholder="Weekly SEO audit" />
          </label>
          <div className="field">
            <span className="label">Seal</span>
            <div className="swatches">
              {ICONS.map((i) => (
                <button key={i} type="button" className="swatch" style={{ background: f.icon === i ? '#fff3c4' : 'var(--paper-2)', display: 'grid', placeItems: 'center' }} aria-pressed={f.icon === i} aria-label={`Seal ${i}`} onClick={() => setF({ ...f, icon: i })}>
                  <Icon name={i} />
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="field" style={{ marginTop: 10 }}>
          <span className="label">Hero</span>
          <div className="hero-pick" role="radiogroup" aria-label="Agent">
            {agents.map((a) => (
              <button type="button" key={a.id} role="radio" aria-checked={f.agent_id === a.id} className={`hero ${f.agent_id === a.id ? 'on' : ''}`} onClick={() => setF({ ...f, agent_id: a.id })} title={`${a.name} · ${a.role}`}>
                <Sprite sprite={a.sprite} color={a.color} size={44} />
                <span>{a.name}</span>
              </button>
            ))}
          </div>
          {agent && f.output === 'tickets' && !agent.board_tools && <p className="form-error">{agent.name} has no Board tools, so it can't file tickets. Turn them on in its persona.</p>}
          {agent && agent.connector_ids.length === 0 && <p className="hint-text">{agent.name} has no portal (connector). It will only use the web and the repo.</p>}
        </div>

        <label className="field">
          <span className="label">The quest (the prompt for every run)</span>
          <textarea required value={f.prompt} onChange={(e) => setF({ ...f, prompt: e.target.value })} placeholder="Check Search Console for pages that lost clicks and file one ticket per fix." style={{ minHeight: 100 }} />
        </label>

        <div className="field">
          <span className="label">When</span>
          <ScheduleBuilder value={sched} onChange={setSched} />
        </div>

        <div className="field">
          <span className="label">Reward (what it produces)</span>
          <div className="output-pick" role="radiogroup" aria-label="Output">
            {(Object.keys(OUTPUT_LABEL) as JobOutput[]).map((o) => (
              <button type="button" key={o} role="radio" aria-checked={f.output === o} className={`out ${f.output === o ? 'on' : ''}`} onClick={() => setF({ ...f, output: o })}>
                <Icon name={OUTPUT_LABEL[o].icon} size={24} />
                {OUTPUT_LABEL[o].label}
              </button>
            ))}
          </div>
        </div>
        <div className="field-row" style={{ marginTop: 10 }}>
          {f.output === 'tickets' && (
            <label className="field">
              <span className="label">Tickets land in</span>
              <select value={f.tab_id} onChange={(e) => setF({ ...f, tab_id: e.target.value })}>
                {tabs.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.name}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="field">
            <span className="label">Repository{f.output === 'pull_request' ? ' (required)' : ''}</span>
            <select value={f.repo_id} onChange={(e) => setF({ ...f, repo_id: e.target.value })}>
              <option value="">None</option>
              {Object.values(repos).map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="row" style={{ marginTop: 10, gap: 18 }}>
          <label className="row">
            <input type="checkbox" checked={f.enabled} onChange={(e) => setF({ ...f, enabled: e.target.checked })} /> Active
          </label>
          <label className="row">
            <input type="checkbox" checked={f.skip_if_running} onChange={(e) => setF({ ...f, skip_if_running: e.target.checked })} /> Skip a tick if the last run is still going
          </label>
        </div>
        <div className="row end" style={{ marginTop: 14 }}>
          <button type="button" className="btn ghost" onClick={close}>
            Cancel
          </button>
          <button className="btn gold" disabled={busy || !f.name.trim() || !f.prompt.trim() || !f.agent_id || !!schedErr || needsRepo} title={schedErr ?? (needsRepo ? 'Pick a repository' : undefined)}>
            {existing ? 'Save quest' : 'Post it'}
          </button>
        </div>
      </form>
    </Modal>
  );
}

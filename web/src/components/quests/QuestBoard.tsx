import { memo, useEffect, useMemo, useState, type CSSProperties } from 'react';
import { api } from '../../api/client';
import type { Job, JobOutput, Run } from '../../api/types';
import { sfx } from '../../audio/audio';
import { timeAgo } from '../../lib/format';
import { countdown, describe } from '../../lib/schedule';
import { attempt } from '../../store/actions';
import { fxBus, toast } from '../../store/bus';
import { isActive, useCan } from '../../store/selectors';
import { useBoard } from '../../store/store';
import { useUi } from '../../store/ui';
import { UNAVAILABLE, useAvailable } from '../../store/features';
import { Markdown } from '../ui/Markdown';
import { Drawer } from '../ui/Overlay';
import { Sprite } from '../ui/Sprite';
import { RunView, STATUS_LABEL } from '../panels/RunView';
import { Discussion } from '../comments/Comments';

export const OUTPUT_LABEL: Record<JobOutput, { icon: string; label: string }> = {
  tickets: { icon: '📋', label: 'Files tickets' },
  report: { icon: '📜', label: 'Writes a report' },
  pull_request: { icon: '🔧', label: 'Opens a PR' },
};

function useNow(ms = 1000) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(id);
  }, [ms]);
  return now;
}

export async function runQuest(j: Job) {
  const r = await attempt(() => api.runJob(j.id));
  if (!r) return;
  useBoard.getState().upsert({ entity: 'run', data: r });
  sfx('done', 0.7);
  fxBus.emit({ fx: 'quest_started', job_id: j.id });
  toast(`Quest "${j.name}" accepted! ⚔️`, 'gold', useBoard.getState().agents[j.agent_id]?.sprite);
}

async function toggle(j: Job) {
  const res = await attempt(() => api.updateJob(j.id, { enabled: !j.enabled }));
  if (res) {
    useBoard.getState().upsert({ entity: 'job', data: res });
    toast(res.enabled ? `"${res.name}" is back on the board` : `"${res.name}" paused`, 'good');
  }
}

const QuestCard = memo(function QuestCard({ job: j, now }: { job: Job; now: number }) {
  const agent = useBoard((s) => s.agents[j.agent_id]);
  const run = useBoard((s) => (j.last_run_id ? s.runs[j.last_run_id] : undefined));
  const spot = useUi((u) => u.spotlight === j.id);
  const open = useUi((u) => u.open);
  const show = useUi((u) => u.show);
  const canManage = useCan('manage_automations');
  const canRun = useCan('run_agents');
  const avail = useAvailable('jobs');
  const running = isActive(run);
  const out = OUTPUT_LABEL[j.output];
  return (
    <article
      className={`quest ${j.enabled ? '' : 'paused'} ${running ? 'running' : ''} ${spot ? 'spotlight' : ''}`}
      data-quest={j.id}
      style={{ '--ac': agent?.color ?? '#f39c12' } as CSSProperties}
    >
      <div className="quest-seal" aria-hidden="true">
        {j.icon}
      </div>
      <button className="quest-main" onClick={() => open({ kind: 'quest', id: j.id })} aria-label={`Open quest ${j.name}`}>
        <h3>{j.name}</h3>
        <p className="quest-sched">⏳ {describe(j.schedule)}</p>
        <div className="quest-next">
          {running ? (
            <span className="live">● On the quest: {run?.activity ?? 'working…'}</span>
          ) : j.enabled && j.next_run_at ? (
            <>
              Next in <b>{countdown(j.next_run_at, now)}</b>
            </>
          ) : j.enabled ? (
            'Waiting for you to press Run'
          ) : (
            'Paused'
          )}
        </div>
        {running && run && (
          <div className="bar" style={{ '--c': agent?.color ?? '#f39c12' } as CSSProperties}>
            <i style={{ width: `${Math.max(4, run.progress * 100)}%` }} />
          </div>
        )}
        {j.last_result && !running && <p className="quest-result">“{j.last_result.replace(/[#*`]/g, '').replace(/\s+/g, ' ').slice(0, 140)}”</p>}
      </button>
      <div className="quest-foot">
        {agent && (
          <span className="worker" title={agent.name}>
            <Sprite sprite={agent.sprite} color={agent.color} status={running ? agent.status : 'idle'} size={34} />
            <span>{agent.name}</span>
          </span>
        )}
        <span className="chip" title={out.label}>
          {out.icon} {out.label}
        </span>
        <span className="grow" />
        {canManage && (
          <label className="switch" title={j.enabled ? 'Pause' : 'Resume'}>
            <input type="checkbox" checked={j.enabled} onChange={() => void toggle(j)} aria-label={`${j.enabled ? 'Pause' : 'Resume'} ${j.name}`} />
            <span />
          </label>
        )}
        {canManage && (
          <button className="icon-btn" onClick={() => show({ kind: 'quest-edit', id: j.id })} aria-label={`Edit ${j.name}`} title="Edit">
            ✎
          </button>
        )}
        {canRun && (
          <button className="btn gold sm" disabled={!avail || running || agent?.status !== 'idle'} title={!avail ? UNAVAILABLE : agent?.status !== 'idle' ? `${agent?.name ?? 'The agent'} is busy` : 'Run now'} onClick={() => void runQuest(j)}>
            ▶ Run
          </button>
        )}
      </div>
      <div className="quest-meta">
        {j.runs_count} run{j.runs_count === 1 ? '' : 's'}
        {j.last_run_at ? ` · last ${timeAgo(j.last_run_at, now)}` : ''}
      </div>
    </article>
  );
});

export function QuestBoard() {
  const jobs = useBoard((s) => s.jobs);
  const show = useUi((u) => u.show);
  const canManage = useCan('manage_automations');
  const avail = useAvailable('jobs');
  // Cheap probe so an older server shows a friendly state instead of failing on the first click.
  useEffect(() => {
    api.previewSchedule({ type: 'manual' }).catch(() => {});
  }, []);
  const now = useNow();
  const list = useMemo(() => Object.values(jobs).sort((a, b) => Number(b.enabled) - Number(a.enabled) || (a.next_run_at ?? 'z').localeCompare(b.next_run_at ?? 'z')), [jobs]);
  return (
    <section className="page wood quest-page" aria-label="Quest board">
      <div className="page-head">
        <div>
          <div className="sign" style={{ textAlign: 'left' }}>
            📜 QUEST BOARD
          </div>
          <p className="page-sub">Recurring missions for your bobas: audits, triage, reports. They run on schedule, even while you sleep.</p>
        </div>
        {canManage && (
          <button className="btn gold" disabled={!avail} title={avail ? undefined : UNAVAILABLE} onClick={() => show({ kind: 'quest-new' })}>
            + Post a quest
          </button>
        )}
      </div>
      {!avail && (
        <div className="sheet soon">
          🚧 The quest board is coming with the next server update. Scheduled quests will show up here.
        </div>
      )}
      <div className="cork">
        {list.map((j) => (
          <QuestCard key={j.id} job={j} now={now} />
        ))}
        {list.length === 0 && avail && (
          <div className="sheet empty-state" style={{ gridColumn: '1 / -1' }}>
            <p>No quest yet. Post one: “Every Monday at 9, Pinky audits Search Console and files tickets.”</p>
          </div>
        )}
      </div>
    </section>
  );
}

export function QuestPanel({ jobId }: { jobId: string }) {
  const j = useBoard((s) => s.jobs[jobId]);
  const agent = useBoard((s) => (j ? s.agents[j.agent_id] : undefined));
  const tab = useBoard((s) => (j?.tab_id ? s.tabs[j.tab_id] : undefined));
  const repo = useBoard((s) => (j?.repo_id ? s.repos[j.repo_id] : undefined));
  const runsMap = useBoard((s) => s.runs);
  const ui = useUi();
  const canManage = useCan('manage_automations');
  const canRun = useCan('run_agents');
  const [sel, setSel] = useState<string | null>(null);
  const now = useNow();

  useEffect(() => {
    api
      .jobRuns(jobId)
      .then((rs) => rs.forEach((r) => useBoard.getState().upsert({ entity: 'run', data: r })))
      .catch(() => {});
  }, [jobId]);
  const runs = useMemo(() => Object.values(runsMap).filter((r): r is Run => r.job_id === jobId).sort((a, b) => (a.created_at < b.created_at ? 1 : -1)), [runsMap, jobId]);
  if (!j) return null;
  const run = runs.find((r) => r.id === sel) ?? runs.find(isActive) ?? runs[0];

  return (
    <Drawer title={`${j.icon} ${j.name.toUpperCase()}`} onClose={() => ui.open(null)}>
      <div className="sheet">
        <dl className="kv">
          <dt>HERO</dt>
          <dd>{agent ? `${agent.name} (${agent.role})` : 'Missing agent'}</dd>
          <dt>WHEN</dt>
          <dd>{describe(j.schedule)}</dd>
          <dt>NEXT</dt>
          <dd>{j.enabled ? (j.next_run_at ? `${new Date(j.next_run_at).toLocaleString()} (in ${countdown(j.next_run_at, now)})` : 'Manual') : 'Paused'}</dd>
          <dt>REWARD</dt>
          <dd>
            {OUTPUT_LABEL[j.output].icon} {OUTPUT_LABEL[j.output].label}
            {tab ? ` in ${tab.name}` : ''}
            {repo ? ` · ${repo.name}` : ''}
          </dd>
          <dt>OVERLAP</dt>
          <dd>{j.skip_if_running ? 'Skips a tick while still running' : 'May overlap'}</dd>
        </dl>
        <div className="section-title">THE QUEST</div>
        <div className="prompt-box">{j.prompt}</div>
        <div className="row" style={{ marginTop: 10 }}>
          {canRun && (
            <button className="btn gold sm" onClick={() => void runQuest(j)} disabled={runs.some(isActive)}>
              ▶ Run now
            </button>
          )}
          {canManage && (
            <>
              <button className="btn sm" onClick={() => ui.show({ kind: 'quest-edit', id: j.id })}>
                ✎ Edit
              </button>
              <button
                className="btn sm red"
                onClick={async () => {
                  if (!confirm(`Tear down the quest "${j.name}"?`)) return;
                  const ok = await attempt(() => api.deleteJob(j.id).then(() => true));
                  if (ok) {
                    useBoard.getState().remove('job', j.id);
                    ui.open(null);
                  }
                }}
              >
                🗑 Delete
              </button>
            </>
          )}
        </div>
        {j.last_result && (
          <>
            <div className="section-title">LAST RESULT</div>
            <Markdown text={j.last_result} />
          </>
        )}
        <div className="section-title">RUNS ({j.runs_count})</div>
        {runs.length === 0 && <p className="muted">{j.runs_count ? 'Older runs are not loaded on this board. The next one will show up here live.' : 'No run on record yet.'}</p>}
        <div className="round-picker" style={{ flexWrap: 'wrap' }}>
          {runs.slice(0, 20).map((r) => (
            <button key={r.id} aria-pressed={run?.id === r.id} onClick={() => setSel(r.id)} title={STATUS_LABEL[r.status]}>
              {isActive(r) ? '●' : r.status === 'succeeded' ? '✓' : r.status === 'failed' ? '✗' : '·'} {timeAgo(r.created_at, now)}
            </button>
          ))}
        </div>
        {run && (
          <div style={{ marginTop: 8 }}>
            <RunView key={run.id} run={run} onSwitchRun={setSel} />
          </div>
        )}
        <div className="section-title">QUEST DISCUSSION</div>
        <Discussion jobId={j.id} />
      </div>
    </Drawer>
  );
}

import { useEffect, useRef, useState } from 'react';
import { api, getTransport, type GithubManifest } from '../../api/client';
import { toast } from '../../store/bus';
import { ApiFailure } from '../../api/transport';
import { sfx } from '../../audio/audio';
import { bodyUrl } from '../../lib/game';
import { errorText } from '../../store/actions';
import { useUi } from '../../store/ui';
import { Modal } from '../ui/Overlay';
import { Brand, Icon } from '../ui/Icon';

const SETUP_PATH = '/settings/github/setup';

const PERMISSION_LABEL: Record<string, string> = {
  contents: 'Code: clone, push branches',
  issues: 'Issues: import, comment, close',
  pull_requests: 'Pull requests: open, comment, review',
  checks: 'Checks: report run status',
  metadata: 'Repository metadata',
  workflows: 'GitHub Actions workflows',
};
const EVENT_LABEL: Record<string, string> = {
  issues: 'Issues opened / edited / closed',
  issue_comment: 'Comments',
  pull_request: 'Pull requests',
  pull_request_review: 'Reviews',
  push: 'Pushes',
  check_suite: 'Check suites',
};

/** Reload-safe deep link: /settings/github/setup?org=&name= reopens this page. */
export function githubSetupFromUrl(): { org?: string; name?: string } | null {
  if (location.pathname.replace(/\/$/, '') !== SETUP_PATH) return null;
  const q = new URLSearchParams(location.search);
  return { org: q.get('org') ?? undefined, name: q.get('name') ?? undefined };
}

/**
 * Game-styled confirm page for GitHub's App-manifest flow. The manifest (with a fresh single-use
 * state) is fetched when the page opens, and the button posts a real form to GitHub so the state
 * check and the callback stay server-side.
 */
export function GithubSetup({ org, name }: { org?: string; name?: string }) {
  const show = useUi((u) => u.show);
  const [m, setM] = useState<GithubManifest | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);

  useEffect(() => {
    const q = new URLSearchParams();
    if (org) q.set('org', org);
    if (name) q.set('name', name);
    history.replaceState(null, '', `${SETUP_PATH}${q.toString() ? `?${q}` : ''}`);
    let alive = true;
    api
      .githubManifest(org, name)
      .then((r) => alive && setM(r))
      .catch((e) => {
        if (!alive) return;
        // Older server: fall back to its own setup page.
        if (e instanceof ApiFailure && e.code === 'unavailable') location.href = api.githubSetupUrl(org, name);
        else setErr(errorText(e));
      });
    return () => {
      alive = false;
      if (location.pathname === SETUP_PATH) history.replaceState(null, '', '/' + location.hash);
    };
  }, [org, name]);

  const perms = Object.entries((m?.manifest.default_permissions as Record<string, string> | undefined) ?? {});
  const events = (m?.manifest.default_events as string[] | undefined) ?? [];
  const where = org ? `the ${org} organisation` : 'your GitHub account';

  return (
    <Modal title="CONNECT GITHUB" onClose={() => show(null)}>
      <div className="gh-setup-page">
        <div className="gh-hero">
          <img src={bodyUrl('boba')} alt="" />
          <span className="gh-plus">
            <Icon name="plus" size={24} />
          </span>
          <span className="gh-octo-big">
            <Brand name="github" size={56} mono />
          </span>
        </div>
        {err ? (
          <p className="form-error" role="alert">
            {err}
          </p>
        ) : !m ? (
          <p className="muted">Preparing your app…</p>
        ) : (
          <>
            <p>
              This creates a private GitHub App named <b>{m.name}</b> in {where}. GitHub asks you to confirm, then which repositories it may use. You come back here when it's done.
            </p>
            {m.warnings.map((w) => (
              <p key={w} className="gh-warning">
                <Icon name="warn" size={14} /> {w}
              </p>
            ))}
            {perms.length > 0 && (
              <>
                <div className="section-title">WHAT THE BOBAS CAN DO</div>
                <ul className="gh-perms">
                  {perms.map(([k, v]) => (
                    <li key={k}>
                      <span className={`perm-level ${v}`}>{v}</span> {PERMISSION_LABEL[k] ?? k.replace(/_/g, ' ')}
                    </li>
                  ))}
                </ul>
              </>
            )}
            {events.length > 0 && (
              <>
                <div className="section-title">WHAT GITHUB TELLS THE BOARD</div>
                <div className="chips">
                  {events.map((e) => (
                    <span key={e} className="chip">
                      {EVENT_LABEL[e] ?? e.replace(/_/g, ' ')}
                    </span>
                  ))}
                </div>
              </>
            )}
            <form
              ref={form}
              method="post"
              action={m.action_url}
              onSubmit={(e) => {
                sfx('done', 0.6);
                if (!getTransport().mock) return;
                // Demo: no real GitHub round trip.
                e.preventDefault();
                void import('../../mock/server').then(({ mockServer }) => {
                  mockServer.githubSetup(org || 'boba-labs');
                  toast('GitHub App created and installed (demo)', 'gold');
                  window.dispatchEvent(new Event('boba:github-changed'));
                  show(null);
                });
              }}
            >
              <input type="hidden" name="manifest" value={JSON.stringify(m.manifest)} />
              <div className="row end" style={{ marginTop: 16 }}>
                <button type="button" className="btn ghost" onClick={() => show(null)}>
                  Not now
                </button>
                <button className="btn gold" autoFocus>
                  <Brand name="github" size={14} mono /> Create the Boba Agent GitHub App
                </button>
              </div>
            </form>
          </>
        )}
      </div>
    </Modal>
  );
}

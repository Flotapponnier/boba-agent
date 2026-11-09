-- Boba Agent schema v1. Ids are UUID v7 text, timestamps RFC 3339 UTC text with a fixed
-- width (lexicographic order = chronological order). Lists are JSON text.

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    email         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    role          TEXT NOT NULL,
    avatar        TEXT NOT NULL,
    password_hash TEXT,
    invite_token_hash TEXT UNIQUE,
    invite_expires_at TEXT,
    created_at    TEXT NOT NULL
);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE repos (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    path           TEXT NOT NULL,
    remote_url     TEXT,
    default_branch TEXT NOT NULL,
    runner_id      TEXT,
    -- automation::RepoSync as JSON
    sync           TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE runners (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    max_concurrent_runs INTEGER NOT NULL,
    last_seen_at        TEXT
);

CREATE TABLE tabs (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    icon     TEXT NOT NULL,
    color    TEXT NOT NULL,
    position INTEGER NOT NULL,
    repo_id  TEXT REFERENCES repos(id) ON DELETE SET NULL
);

CREATE TABLE columns (
    id                     TEXT PRIMARY KEY,
    tab_id                 TEXT NOT NULL REFERENCES tabs(id) ON DELETE CASCADE,
    name                   TEXT NOT NULL,
    kind                   TEXT NOT NULL,
    color                  TEXT NOT NULL,
    position               INTEGER NOT NULL,
    auto_reviewer_agent_id TEXT,
    on_approve_column_id   TEXT,
    on_changes_column_id   TEXT
);
CREATE INDEX columns_tab ON columns(tab_id, position);

CREATE TABLE skills (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL,
    source      TEXT NOT NULL,
    path        TEXT NOT NULL UNIQUE,
    editable    INTEGER NOT NULL
);

CREATE TABLE agents (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    role            TEXT NOT NULL,
    sprite          TEXT NOT NULL,
    color           TEXT NOT NULL,
    tagline         TEXT NOT NULL,
    system_prompt   TEXT NOT NULL,
    model           TEXT,
    permission_mode TEXT NOT NULL,
    allowed_tools   TEXT NOT NULL,
    max_turns       INTEGER,
    xp              INTEGER NOT NULL DEFAULT 0,
    level           INTEGER NOT NULL DEFAULT 1,
    runs_completed  INTEGER NOT NULL DEFAULT 0,
    prs_approved    INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL,
    current_run_id  TEXT,
    board_tools     INTEGER NOT NULL DEFAULT 0,
    created_by      TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE agent_skills (
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (agent_id, skill_id)
);

-- Connectors (MCP servers) an agent may use. No FK: the connectors table ships separately.
CREATE TABLE agent_connectors (
    agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    connector_id TEXT NOT NULL,
    position     INTEGER NOT NULL,
    PRIMARY KEY (agent_id, connector_id)
);

CREATE TABLE tickets (
    id               TEXT PRIMARY KEY,
    number           INTEGER NOT NULL UNIQUE,
    tab_id           TEXT NOT NULL REFERENCES tabs(id) ON DELETE CASCADE,
    column_id        TEXT NOT NULL,
    position         REAL NOT NULL,
    title            TEXT NOT NULL,
    body             TEXT NOT NULL,
    tags             TEXT NOT NULL,
    priority         TEXT NOT NULL,
    color            TEXT,
    repo_id          TEXT,
    agent_id         TEXT,
    branch           TEXT,
    pr_url           TEXT,
    pr_number        INTEGER,
    review_feedback  TEXT,
    review_rounds    INTEGER NOT NULL DEFAULT 0,
    created_by       TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    -- automation::TicketSource as JSON
    source           TEXT NOT NULL DEFAULT '{"kind":"manual"}',
    -- server-internal: when the ticket entered its current column (review queue order)
    entered_column_at TEXT NOT NULL
);
CREATE INDEX tickets_column ON tickets(column_id, position);

CREATE TABLE runs (
    id            TEXT PRIMARY KEY,
    -- NULL for job / command runs
    ticket_id     TEXT REFERENCES tickets(id) ON DELETE CASCADE,
    job_id        TEXT,
    agent_id      TEXT NOT NULL,
    kind          TEXT NOT NULL,
    status        TEXT NOT NULL,
    runner_id     TEXT,
    branch        TEXT,
    worktree_path TEXT,
    session_id    TEXT,
    progress      REAL NOT NULL DEFAULT 0,
    activity      TEXT,
    summary       TEXT,
    verdict       TEXT,
    cost_usd      REAL NOT NULL DEFAULT 0,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    started_by    TEXT,
    created_at    TEXT NOT NULL,
    started_at    TEXT,
    ended_at      TEXT,
    error         TEXT,
    -- server-internal: the instructions / resume session the run was created with
    extra_instructions TEXT,
    resume_session_id  TEXT
);
CREATE INDEX runs_ticket ON runs(ticket_id, created_at);
CREATE INDEX runs_status ON runs(status);
CREATE INDEX runs_job ON runs(job_id, created_at);

CREATE TABLE run_logs (
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    seq    INTEGER NOT NULL,
    ts     TEXT NOT NULL,
    kind   TEXT NOT NULL,
    text   TEXT NOT NULL,
    data   TEXT,
    PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;

CREATE TABLE history (
    id         TEXT PRIMARY KEY,
    ts         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id   TEXT,
    ticket_id  TEXT,
    agent_id   TEXT,
    run_id     TEXT,
    tab_id     TEXT,
    message    TEXT NOT NULL,
    data       TEXT
);
CREATE INDEX history_ts ON history(ts DESC);
CREATE INDEX history_ticket ON history(ticket_id, ts DESC);
CREATE INDEX history_agent ON history(agent_id, ts DESC);
CREATE INDEX history_run ON history(run_id, ts DESC);
CREATE INDEX history_tab ON history(tab_id, ts DESC);

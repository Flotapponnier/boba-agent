-- Scheduled jobs: the Quest Board (automation::Job).
CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT NOT NULL,
    icon            TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    prompt          TEXT NOT NULL,
    schedule        TEXT NOT NULL,      -- JSON Schedule
    output          TEXT NOT NULL,
    enabled         INTEGER NOT NULL,
    repo_id         TEXT,
    tab_id          TEXT,
    skip_if_running INTEGER NOT NULL,
    last_run_id     TEXT,
    last_run_at     TEXT,
    next_run_at     TEXT,
    last_result     TEXT,
    runs_count      INTEGER NOT NULL DEFAULT 0,
    created_by      TEXT,
    created_at      TEXT NOT NULL
);
CREATE INDEX jobs_due ON jobs(enabled, next_run_at);

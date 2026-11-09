-- Command bar requests: who asked what (the plan goes back to that user only).
CREATE TABLE command_runs (
    run_id     TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    text       TEXT NOT NULL,
    voice      INTEGER NOT NULL,
    tab_id     TEXT,
    ticket_id  TEXT,
    agent_id   TEXT,
    plan       TEXT,                 -- the CommandPlan sent back (JSON)
    executed   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX command_runs_user ON command_runs(user_id, created_at);

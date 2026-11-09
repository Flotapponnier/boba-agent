-- Per-user inbox (team::Notification). Pushed over WS to its owner only.
CREATE TABLE notifications (
    id             TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL,
    message        TEXT NOT NULL,
    ticket_id      TEXT,
    comment_id     TEXT,
    run_id         TEXT,
    job_id         TEXT,
    actor_user_id  TEXT,
    actor_agent_id TEXT,
    read           INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL
);
CREATE INDEX notifications_user ON notifications(user_id, created_at DESC);
CREATE INDEX notifications_unread ON notifications(user_id, read);

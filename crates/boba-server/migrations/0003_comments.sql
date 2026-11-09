-- Comment threads (team::Comment): a root (parent_id NULL) + replies, on a ticket or a job.
CREATE TABLE comments (
    id                  TEXT PRIMARY KEY,
    ticket_id           TEXT REFERENCES tickets(id) ON DELETE CASCADE,
    job_id              TEXT,
    anchor              TEXT NOT NULL,             -- JSON CommentAnchor
    parent_id           TEXT REFERENCES comments(id) ON DELETE CASCADE,
    author_kind         TEXT NOT NULL,
    author_id           TEXT NOT NULL,
    body                TEXT NOT NULL,
    mentioned_user_ids  TEXT NOT NULL DEFAULT '[]',
    mentioned_agent_ids TEXT NOT NULL DEFAULT '[]',
    triggered_run_ids   TEXT NOT NULL DEFAULT '[]',
    reactions           TEXT NOT NULL DEFAULT '[]', -- JSON Reaction[]
    resolved            INTEGER NOT NULL DEFAULT 0,
    resolved_by         TEXT,
    created_at          TEXT NOT NULL,
    edited_at           TEXT,
    deleted             INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX comments_ticket ON comments(ticket_id, created_at);
CREATE INDEX comments_job ON comments(job_id, created_at);
CREATE INDEX comments_parent ON comments(parent_id);

-- A run started by an @mention answers in this thread (root comment id) when it finishes.
ALTER TABLE runs ADD COLUMN reply_comment_id TEXT;

-- Tickets imported from GitHub. (repo_id, kind, number) is unique: imports are idempotent.
CREATE TABLE github_links (
    repo_id   TEXT NOT NULL,
    kind      TEXT NOT NULL,          -- 'issue' | 'pr'
    number    INTEGER NOT NULL,
    ticket_id TEXT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    PRIMARY KEY (repo_id, kind, number)
);
CREATE INDEX github_links_ticket ON github_links(ticket_id);

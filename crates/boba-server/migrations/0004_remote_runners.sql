-- Remote runners ("connect your Claude Code"). The built-in runner has no row here.
ALTER TABLE runners ADD COLUMN token_hash TEXT;
ALTER TABLE runners ADD COLUMN scope TEXT NOT NULL DEFAULT 'shared';
ALTER TABLE runners ADD COLUMN owner_user_id TEXT;
ALTER TABLE runners ADD COLUMN auth TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE runners ADD COLUMN host TEXT;
ALTER TABLE runners ADD COLUMN claude_version TEXT;
ALTER TABLE runners ADD COLUMN created_by TEXT;
ALTER TABLE runners ADD COLUMN created_at TEXT;
CREATE UNIQUE INDEX runners_token ON runners(token_hash);

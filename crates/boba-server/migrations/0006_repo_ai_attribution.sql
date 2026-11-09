-- Whether agents' commits / PRs in this repo may carry an AI attribution line.
ALTER TABLE repos ADD COLUMN ai_attribution INTEGER NOT NULL DEFAULT 1;

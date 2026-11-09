-- The asker's IANA time zone (validated), for schedules in their plan.
ALTER TABLE command_runs ADD COLUMN timezone TEXT;

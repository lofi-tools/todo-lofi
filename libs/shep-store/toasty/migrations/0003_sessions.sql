-- The adapter's own session handle, so a restart asks the agent to resume the
-- thread it already has instead of silently starting a new one.
ALTER TABLE sessions ADD COLUMN adapter_session_id TEXT;

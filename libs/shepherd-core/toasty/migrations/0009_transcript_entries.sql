ALTER TABLE entries ADD COLUMN task_id TEXT REFERENCES tasks (id) ON DELETE CASCADE;
UPDATE entries SET task_id = (SELECT task_id FROM sessions WHERE sessions.id = entries.session_id);
CREATE UNIQUE INDEX idx_entries_task_id ON entries (task_id, id);
CREATE INDEX idx_entries_task_sequence ON entries (task_id, sequence);

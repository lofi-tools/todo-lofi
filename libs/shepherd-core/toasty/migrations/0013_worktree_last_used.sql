ALTER TABLE tasks ADD COLUMN worktree_last_used_at INTEGER NOT NULL DEFAULT 0;
UPDATE tasks SET worktree_last_used_at = updated_at;

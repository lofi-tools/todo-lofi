ALTER TABLE tasks ADD COLUMN model TEXT;
ALTER TABLE tasks ADD COLUMN mode TEXT;
ALTER TABLE tasks ADD COLUMN branch_error TEXT;

DELETE FROM prompt_queues
WHERE task_id IN (
    SELECT id FROM tasks
    WHERE state = 'draft' AND worktree_path IS NULL AND branch IS NULL
);

DELETE FROM tasks
WHERE state = 'draft' AND worktree_path IS NULL AND branch IS NULL;

UPDATE projects
SET last_task_id = NULL
WHERE last_task_id IS NOT NULL
  AND last_task_id NOT IN (SELECT id FROM tasks);
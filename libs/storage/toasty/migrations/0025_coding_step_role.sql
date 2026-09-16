-- Coding workflow subtasks and spec coverage
-- (docs/spec/coding-workflow-subtasks-spec.md §5.1): `role` gives an
-- engine-materialized run step an explicit identity so every subtask surface
-- can filter it out, and `spec_covered_at` records when the feature spec was
-- accepted as covering a subtask.
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "role" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "spec_covered_at" TEXT;
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS idx_tasks_role ON tasks("role");
-- #[toasty::breakpoint]
UPDATE tasks SET role = 'step' WHERE node_id IS NOT NULL AND workflow_run_id IS NOT NULL;

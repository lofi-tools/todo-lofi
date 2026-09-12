-- Coding workflow runs: a run can be attached to an existing feature task
-- (`root_task_id`), carries its git branch, and keeps that branch visible
-- after cancellation so it can be cleaned up deliberately. Tasks gain the
-- spec artifact (`spec` + `spec_path`), shown inline in the task details.
-- #[toasty::breakpoint]
ALTER TABLE workflow_runs ADD COLUMN "root_task_id" INTEGER;
-- #[toasty::breakpoint]
CREATE INDEX idx_workflow_runs_root_task ON workflow_runs("root_task_id");
-- #[toasty::breakpoint]
ALTER TABLE workflow_runs ADD COLUMN "branch" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE workflow_runs ADD COLUMN "base_branch" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE workflow_runs ADD COLUMN "branch_status" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "spec" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "spec_path" TEXT;

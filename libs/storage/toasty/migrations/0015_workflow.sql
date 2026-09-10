-- #[toasty::breakpoint]
CREATE TABLE workflow_recipes (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "slug" TEXT NOT NULL,
    "version" INTEGER NOT NULL,
    "recipe_json" TEXT NOT NULL,
    "created_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX idx_workflow_recipes_slug_version ON workflow_recipes("slug", "version");
-- #[toasty::breakpoint]
CREATE TABLE workflow_runs (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "recipe_id" INTEGER NOT NULL REFERENCES workflow_recipes("id"),
    "schedule_id" INTEGER REFERENCES repeat_task_templates("id"),
    "status" TEXT NOT NULL,
    "params" TEXT NOT NULL,
    "step_results" TEXT NOT NULL,
    "created_at" TEXT NOT NULL,
    "completed_at" TEXT
);
-- #[toasty::breakpoint]
CREATE INDEX idx_workflow_runs_recipe ON workflow_runs("recipe_id");
-- #[toasty::breakpoint]
CREATE INDEX idx_workflow_runs_status ON workflow_runs("status");
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "workflow_run_id" INTEGER;
-- #[toasty::breakpoint]
CREATE INDEX idx_tasks_workflow_run ON tasks("workflow_run_id");
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "node_id" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE repeat_task_templates ADD COLUMN "recipe_id" INTEGER;
-- Apps: the unified registry of content owners. `kind` is
-- 'recipe' (a workflow automation), 'integration' (e.g. todoist), or
-- 'builtin' (shipped content such as the demo seed data).
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS apps (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "kind" TEXT NOT NULL,
    "slug" TEXT NOT NULL UNIQUE,
    "label" TEXT NOT NULL,
    "description" TEXT,
    "enabled" INTEGER NOT NULL DEFAULT 0,
    "created_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
ALTER TABLE workflow_recipes ADD COLUMN "app_id" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE integrations ADD COLUMN "app_id" INTEGER;

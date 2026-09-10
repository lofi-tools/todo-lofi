-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "deleted_at" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "timezone" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "comments" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE repeat_task_templates ADD COLUMN "weekdays" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE repeat_task_templates ADD COLUMN "month_day" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE repeat_task_templates ADD COLUMN "strict" INTEGER NOT NULL DEFAULT 0;
-- #[toasty::breakpoint]
ALTER TABLE repeat_task_templates ADD COLUMN "timezone" TEXT;
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS integrations (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "provider" TEXT NOT NULL,
    "account_label" TEXT,
    "created_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS external_task_links (
    "integration_id" INTEGER NOT NULL,
    "external_id" TEXT NOT NULL,
    "task_id" INTEGER NOT NULL,
    "external_updated_at" TEXT,
    UNIQUE ("integration_id", "external_id")
);
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS external_tag_links (
    "integration_id" INTEGER NOT NULL,
    "external_id" TEXT NOT NULL,
    "tag_id" INTEGER NOT NULL,
    "source_kind" TEXT NOT NULL,
    "namespaced" INTEGER NOT NULL DEFAULT 0,
    UNIQUE ("integration_id", "external_id")
);
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS sync_state (
    "integration_id" INTEGER NOT NULL,
    "project_id" TEXT NOT NULL,
    "watermark" TEXT NOT NULL,
    PRIMARY KEY ("integration_id", "project_id")
);

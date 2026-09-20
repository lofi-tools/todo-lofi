-- Per-task extension data (docs/spec/coding-interview-readonly-session-spec.md
-- §8): a namespaced key/value store owned by an app/extension, so a feature no
-- longer needs a dedicated `tasks` column. `value` is JSON text.
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS task_extra (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "task_id" INTEGER NOT NULL,
    "namespace" TEXT NOT NULL,
    "key" TEXT NOT NULL,
    "value" TEXT NOT NULL,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX IF NOT EXISTS idx_task_extra_scope
    ON task_extra("task_id", "namespace", "key");
-- Backfill the coding specs before the columns go away. `spec_path` is not
-- migrated: the spec is database-only now (decision #17).
-- #[toasty::breakpoint]
INSERT INTO task_extra ("task_id", "namespace", "key", "value", "created_at", "updated_at")
    SELECT id, 'coding', 'spec', json_quote(spec),
           strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM tasks WHERE spec IS NOT NULL AND trim(spec) <> '';
-- #[toasty::breakpoint]
ALTER TABLE tasks DROP COLUMN "spec";
-- #[toasty::breakpoint]
ALTER TABLE tasks DROP COLUMN "spec_path";

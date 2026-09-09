CREATE TABLE "repeat_task_templates" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "interval_days" INTEGER NOT NULL,
    "created_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE TABLE "repeat_task_occurrences" (
    "template_id" INTEGER NOT NULL REFERENCES repeat_task_templates("id") ON DELETE CASCADE,
    "task_id" INTEGER NOT NULL REFERENCES tasks("id") ON DELETE CASCADE,
    "occurrence_index" INTEGER NOT NULL,
    PRIMARY KEY ("template_id", "task_id")
);
-- #[toasty::breakpoint]
CREATE INDEX idx_repeat_task_occurrences_task ON repeat_task_occurrences("task_id");
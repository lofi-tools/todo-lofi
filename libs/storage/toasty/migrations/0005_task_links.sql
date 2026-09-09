CREATE TABLE task_links (
    "task_id" INTEGER NOT NULL REFERENCES tasks("id") ON DELETE CASCADE,
    "other_id" INTEGER NOT NULL REFERENCES tasks("id") ON DELETE CASCADE,
    "kind" TEXT NOT NULL,
    PRIMARY KEY ("task_id", "other_id", "kind")
);
-- #[toasty::breakpoint]
CREATE INDEX idx_task_links_task ON task_links("task_id");
-- #[toasty::breakpoint]
CREATE INDEX idx_task_links_other ON task_links("other_id");
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "blocked_until" INTEGER;

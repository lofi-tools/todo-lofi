-- App bindings and per-row ownership. A binding is "(app, tag, role)":
-- `full_tag` preserves whole-tag ownership, `partial` is an app managing
-- only some sections/tasks inside a tag the user owns. Owner columns are
-- nullable on every content table: NULL means the user owns the row.
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS app_tag_bindings (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "app_id" INTEGER NOT NULL,
    "tag_id" INTEGER NOT NULL,
    "role" TEXT NOT NULL,
    "capture_new_tasks" INTEGER NOT NULL DEFAULT 0,
    "created_at" TEXT NOT NULL,
    UNIQUE ("app_id", "tag_id")
);
-- #[toasty::breakpoint]
ALTER TABLE tags ADD COLUMN "managed_by" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE tag_sections ADD COLUMN "managed_by" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE tag_sections ADD COLUMN "managed_capture" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "managed_by" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "managed_mode" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "managed_editable" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "user_modified" INTEGER NOT NULL DEFAULT 0;

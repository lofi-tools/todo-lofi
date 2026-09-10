-- Managed tags: a tag owned by a workflow recipe (created when its
-- automation is enabled, removed when disabled). `NULL` = ordinary tag.
-- #[toasty::breakpoint]
ALTER TABLE tags ADD COLUMN "managed_by_recipe_id" INTEGER;
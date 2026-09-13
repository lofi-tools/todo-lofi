-- `tags.managed_by_recipe_id` is retired. Whole-tag recipe ownership was
-- replaced by app bindings (`app_tag_bindings`, 0019), and the startup
-- backfill that converted legacy rows into partial bindings has already run
-- on every database opened by a build that contained it, so there is nothing
-- left to convert. Nothing reads the column any more either.
-- #[toasty::breakpoint]
ALTER TABLE tags DROP COLUMN "managed_by_recipe_id";

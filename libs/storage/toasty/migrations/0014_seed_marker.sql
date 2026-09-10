-- #[toasty::breakpoint]
ALTER TABLE tasks ADD COLUMN "is_seed" INTEGER NOT NULL DEFAULT 0;
-- #[toasty::breakpoint]
ALTER TABLE tags ADD COLUMN "is_seed" INTEGER NOT NULL DEFAULT 0;

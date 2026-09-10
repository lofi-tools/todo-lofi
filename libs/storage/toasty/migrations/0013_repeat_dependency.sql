-- #[toasty::breakpoint]
ALTER TABLE repeat_task_templates ADD COLUMN "blocked_by_template_id" INTEGER;

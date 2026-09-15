-- Per-project opt-out from the shared worktree build cache
-- (docs/spec/github-integration-spec.md §6.4). Off by default: every worktree
-- of a run builds into the repo's `target/`. On, each worktree builds into its
-- own `target/`, trading disk and time for builds that cannot invalidate each
-- other across branches.
-- #[toasty::breakpoint]
ALTER TABLE tag_settings ADD COLUMN "isolated_build_cache" INTEGER NOT NULL DEFAULT 0;

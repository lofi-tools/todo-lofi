-- GitHub integration (docs/spec/github-integration-spec.md): per-field merge
-- state for issue links, per-repo sync cursors, and the run-side rows for
-- worktrees and pull requests introduced by the PR step.
--
-- `external_task_links.field_state` holds the last remote values we synced
-- plus the local field-change stamps, because GitHub timestamps whole issues
-- rather than individual fields (§5.4).
-- #[toasty::breakpoint]
ALTER TABLE external_task_links ADD COLUMN "field_state" TEXT;
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS integration_sync_state (
    "integration_id" INTEGER NOT NULL,
    "external_id" TEXT NOT NULL,
    "etag" TEXT,
    "since" TEXT,
    "last_synced_at" TEXT,
    PRIMARY KEY ("integration_id", "external_id")
);
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS run_worktrees (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "run_id" INTEGER NOT NULL,
    "repo_dir" TEXT NOT NULL,
    "worktree_path" TEXT NOT NULL,
    "branch" TEXT NOT NULL,
    "base_branch" TEXT NOT NULL,
    "remote" TEXT NOT NULL,
    "created_at" TEXT NOT NULL,
    "removed_at" TEXT
);
-- #[toasty::breakpoint]
CREATE INDEX idx_run_worktrees_run ON run_worktrees("run_id");
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS run_pull_requests (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "run_id" INTEGER NOT NULL,
    "repo_dir" TEXT NOT NULL,
    "integration_id" INTEGER NOT NULL,
    "owner" TEXT NOT NULL,
    "repo" TEXT NOT NULL,
    "number" INTEGER NOT NULL,
    "url" TEXT NOT NULL,
    "head_branch" TEXT NOT NULL,
    "base_branch" TEXT NOT NULL,
    "draft" INTEGER NOT NULL DEFAULT 1,
    "state" TEXT NOT NULL,
    "merged_at" TEXT,
    "last_polled_at" TEXT,
    UNIQUE ("integration_id", "owner", "repo", "number")
);
-- #[toasty::breakpoint]
CREATE INDEX idx_run_pull_requests_run ON run_pull_requests("run_id");
-- #[toasty::breakpoint]
CREATE INDEX idx_run_pull_requests_state ON run_pull_requests("state");

-- Sync operation log (docs/spec/github-integration-spec.md §5.10): one row per
-- provider push the app attempted, delivered or not, with the reason a push
-- failed. It is the app's record of what it synced and when, and its failures
-- are what a replay pass retries once the cause is gone — a personal token
-- carrying the permission the first attempt lacked, say.
--
-- Append-only: a retry writes a new row rather than rewriting the old one, so a
-- failure stays visible after it has been worked around, and "how many times did
-- this keep failing" is a query rather than a counter. A *task* is behind when
-- its most recent row is a failure, which is how a replay picks its backlog.
--
-- Supersedes the label-only `pending_sync_ops` of 0027: those undelivered
-- pushes become the first rows of this log, and that table is retired.
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS sync_op_log (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "provider" TEXT NOT NULL,
    "task_id" INTEGER,
    "kind" TEXT NOT NULL,
    "payload" TEXT NOT NULL,
    "status" TEXT NOT NULL,
    "error" TEXT,
    "created_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS idx_sync_op_log_provider ON sync_op_log("provider", "id");
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS idx_sync_op_log_task ON sync_op_log("task_id");
-- #[toasty::breakpoint]
INSERT INTO sync_op_log ("provider", "task_id", "kind", "payload", "status", "error", "created_at")
    SELECT provider, task_id, kind, payload, 'failed', error, updated_at FROM pending_sync_ops;
-- #[toasty::breakpoint]
DROP TABLE IF EXISTS pending_sync_ops;

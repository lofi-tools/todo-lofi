-- Durable record of a push the app could not deliver (docs/spec/github-integration-spec.md
-- §5.5): a tag edit pushes its labels in the background, and a failure there has no
-- caller left to report to, so what the push wanted is kept here to be replayed
-- later — for example once the user has added a personal token that grants the
-- permission the first attempt lacked.
--
-- One row per (provider, integration, external object, kind): a later failure for
-- the same target replaces the payload and bumps `attempts`, so a broken target
-- accumulates one row rather than a queue of duplicates. `payload` is JSON in
-- whatever shape the kind needs (labels: `{"labels": ["bug"]}`).
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS pending_sync_ops (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "provider" TEXT NOT NULL,
    "integration_id" INTEGER NOT NULL,
    "external_id" TEXT NOT NULL,
    "task_id" INTEGER,
    "kind" TEXT NOT NULL,
    "payload" TEXT NOT NULL,
    "error" TEXT NOT NULL,
    "attempts" INTEGER NOT NULL DEFAULT 1,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL,
    UNIQUE ("provider", "integration_id", "external_id", "kind")
);
-- #[toasty::breakpoint]
CREATE INDEX IF NOT EXISTS idx_pending_sync_ops_integration
    ON pending_sync_ops("integration_id");

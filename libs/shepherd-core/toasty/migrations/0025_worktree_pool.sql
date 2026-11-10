CREATE TABLE worktree_pool (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL,
    path TEXT NOT NULL,
    state TEXT NOT NULL,
    task_id TEXT,
    leased_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX worktree_pool_project_path ON worktree_pool (project_id, path);
CREATE INDEX worktree_pool_project_state ON worktree_pool (project_id, state);
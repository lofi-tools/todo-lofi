CREATE TABLE worktree_deletions (
    task_id TEXT NOT NULL PRIMARY KEY,
    repository_root TEXT NOT NULL,
    original_path TEXT NOT NULL,
    staged_path TEXT NOT NULL
);

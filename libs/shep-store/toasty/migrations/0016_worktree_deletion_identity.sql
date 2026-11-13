ALTER TABLE worktree_deletions ADD COLUMN worktree_git_dir TEXT NOT NULL DEFAULT '';
ALTER TABLE worktree_deletions ADD COLUMN identity TEXT NOT NULL DEFAULT '';

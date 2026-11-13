CREATE TABLE projects (
    id TEXT NOT NULL PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    missing INTEGER NOT NULL DEFAULT 0,
    last_task_id TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_projects_created_at ON projects (created_at);

CREATE TABLE tasks (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    tier TEXT NOT NULL,
    proposed_worktree_path TEXT NOT NULL,
    proposed_branch TEXT NOT NULL,
    worktree_path TEXT,
    branch TEXT,
    base_branch TEXT,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_tasks_project_id ON tasks (project_id);
CREATE INDEX idx_tasks_state ON tasks (state);
CREATE INDEX idx_tasks_created_at ON tasks (created_at);

CREATE TABLE sessions (
    id TEXT NOT NULL PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    adapter TEXT NOT NULL,
    state TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    exit_code INTEGER,
    stderr TEXT
);
CREATE INDEX idx_sessions_task_id ON sessions (task_id);

CREATE TABLE entries (
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, sequence),
    UNIQUE (session_id, id)
);
CREATE INDEX idx_entries_session_sequence ON entries (session_id, sequence);

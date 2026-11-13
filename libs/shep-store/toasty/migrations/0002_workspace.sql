CREATE TABLE terminals (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks (id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    title TEXT NOT NULL,
    cwd TEXT NOT NULL,
    command TEXT NOT NULL,
    profile TEXT,
    created_at INTEGER NOT NULL,
    ended_at INTEGER,
    exit_code INTEGER,
    CHECK (scope IN ('project', 'task')),
    CHECK ((scope = 'project' AND task_id IS NULL) OR (scope = 'task' AND task_id IS NOT NULL))
);
CREATE INDEX idx_terminals_project_id ON terminals (project_id);
CREATE INDEX idx_terminals_task_id ON terminals (task_id);

CREATE TABLE terminal_layouts (
    project_id TEXT NOT NULL PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    layout_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE layouts (
    task_id TEXT NOT NULL PRIMARY KEY REFERENCES tasks (id) ON DELETE CASCADE,
    workspace_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

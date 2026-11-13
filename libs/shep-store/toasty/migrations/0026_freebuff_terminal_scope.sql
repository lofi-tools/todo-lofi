CREATE TABLE terminals_with_freebuff (
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
    CHECK (scope IN ('project', 'task', 'freebuff')),
    CHECK ((scope = 'project' AND task_id IS NULL)
        OR (scope = 'task' AND task_id IS NOT NULL)
        OR (scope = 'freebuff' AND task_id IS NULL))
);

INSERT INTO terminals_with_freebuff (
    id, project_id, task_id, scope, title, cwd, command, profile, created_at, ended_at, exit_code
)
SELECT id, project_id, task_id, scope, title, cwd, command, profile, created_at, ended_at, exit_code
FROM terminals;

DROP TABLE terminals;

ALTER TABLE terminals_with_freebuff RENAME TO terminals;

CREATE INDEX idx_terminals_project_id ON terminals (project_id);
CREATE INDEX idx_terminals_task_id ON terminals (task_id);

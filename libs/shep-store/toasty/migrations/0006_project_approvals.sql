CREATE TABLE IF NOT EXISTS project_approvals (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    grant_json TEXT NOT NULL,
    PRIMARY KEY (project_id, id)
);

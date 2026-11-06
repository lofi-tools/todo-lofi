CREATE TABLE attachments (
    task_id TEXT NOT NULL,
    entry_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    mime TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK (bytes >= 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, entry_id, position),
    FOREIGN KEY (task_id, entry_id) REFERENCES entries (task_id, id) ON DELETE CASCADE
);

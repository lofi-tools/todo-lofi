CREATE TABLE processes (
    id TEXT NOT NULL PRIMARY KEY,
    kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    pid INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    process_group INTEGER NOT NULL,
    session_id INTEGER NOT NULL,
    boot_id TEXT NOT NULL
);

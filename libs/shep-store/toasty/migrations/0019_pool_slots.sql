CREATE TABLE pool_slots (
    id TEXT NOT NULL PRIMARY KEY,
    agent TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    model_id TEXT NOT NULL,
    model_name TEXT NOT NULL,
    credential_ref TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL,
    enabled INTEGER NOT NULL,
    used INTEGER,
    quota_limit INTEGER,
    limit_unit TEXT NOT NULL DEFAULT '',
    reset_at INTEGER,
    cooldown_until INTEGER,
    last_used_at INTEGER,
    last_error TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX pool_slots_priority ON pool_slots (priority, created_at, id);

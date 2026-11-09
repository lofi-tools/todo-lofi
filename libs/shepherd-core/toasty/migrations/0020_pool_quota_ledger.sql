CREATE TABLE pool_quota_ledger (
    id TEXT NOT NULL PRIMARY KEY,
    slot_id TEXT NOT NULL REFERENCES pool_slots(id) ON DELETE CASCADE,
    observed_at INTEGER NOT NULL,
    source TEXT NOT NULL,
    used INTEGER,
    quota_limit INTEGER,
    limit_unit TEXT NOT NULL DEFAULT '',
    reset_at INTEGER,
    cooldown_until INTEGER,
    error TEXT NOT NULL DEFAULT ''
);

CREATE INDEX pool_quota_ledger_slot ON pool_quota_ledger (slot_id, observed_at, id);

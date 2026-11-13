ALTER TABLE pool_quota_ledger ADD COLUMN window_key TEXT NOT NULL DEFAULT '';

CREATE INDEX pool_quota_ledger_slot_window
    ON pool_quota_ledger (slot_id, window_key, observed_at, id);

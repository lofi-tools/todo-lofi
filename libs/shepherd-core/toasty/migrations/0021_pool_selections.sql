CREATE TABLE pool_selections (
    request_id TEXT NOT NULL PRIMARY KEY,
    slot_id TEXT REFERENCES pool_slots(id) ON DELETE SET NULL,
    selected_at INTEGER NOT NULL,
    released_at INTEGER,
    reason TEXT NOT NULL,
    skipped_json TEXT NOT NULL
);

CREATE UNIQUE INDEX pool_selections_active_slot
    ON pool_selections (slot_id)
    WHERE released_at IS NULL AND slot_id IS NOT NULL;

CREATE INDEX pool_selections_slot ON pool_selections (slot_id, selected_at);

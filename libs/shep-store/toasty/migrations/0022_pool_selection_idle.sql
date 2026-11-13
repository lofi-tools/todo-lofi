ALTER TABLE pool_selections ADD COLUMN idle_since INTEGER;

CREATE INDEX pool_selections_idle
    ON pool_selections (idle_since)
    WHERE released_at IS NULL AND idle_since IS NOT NULL;

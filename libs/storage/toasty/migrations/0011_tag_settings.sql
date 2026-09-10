-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS tag_settings (
    "tag_id" INTEGER NOT NULL PRIMARY KEY,
    "sync_integration_id" INTEGER,
    "sync_external_id" TEXT,
    "dirs" TEXT
);
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS tag_sections (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "tag_id" INTEGER NOT NULL,
    "name" TEXT NOT NULL,
    "position" INTEGER NOT NULL DEFAULT 0,
    UNIQUE ("tag_id", "name")
);

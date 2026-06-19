CREATE TABLE tags (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL UNIQUE
);

CREATE TABLE tag_implications (
    "implier_id" INTEGER NOT NULL REFERENCES tags("id") ON DELETE CASCADE,
    "implied_id" INTEGER NOT NULL REFERENCES tags("id") ON DELETE CASCADE,
    PRIMARY KEY ("implier_id", "implied_id")
);

CREATE INDEX idx_tag_implications_implier ON tag_implications("implier_id");
CREATE INDEX idx_tag_implications_implied ON tag_implications("implied_id");

CREATE TABLE direct_task_tags (
    "task_id" INTEGER NOT NULL REFERENCES tasks("id") ON DELETE CASCADE,
    "tag_id" INTEGER NOT NULL REFERENCES tags("id") ON DELETE CASCADE,
    PRIMARY KEY ("task_id", "tag_id")
);

CREATE TABLE tasks (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "title" TEXT NOT NULL,
    "description" TEXT,
    "branch_name" TEXT,
    "labels" TEXT,
    "blocked_by" TEXT,
    "deadline" INTEGER,
    "importance_factor" REAL NOT NULL,
    "urgency_factor" REAL NOT NULL,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL,
    "parent_id" INTEGER REFERENCES tasks("id")
);

CREATE TRIGGER protect_created_at
BEFORE UPDATE OF created_at ON tasks
BEGIN
    SELECT RAISE(ABORT, 'The created_at column cannot be updated');
END;

CREATE INDEX index_tasks_by_parent_id ON tasks("parent_id");

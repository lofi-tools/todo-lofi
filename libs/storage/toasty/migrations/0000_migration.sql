CREATE TABLE "tasks" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "title" TEXT NOT NULL,
    "description" TEXT,
    "branch_name" TEXT,
    "labels" TEXT,
    "blocked_by" TEXT,
    "importance_factor" REAL NOT NULL,
    "urgency_factor" REAL NOT NULL,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL
);

CREATE TRIGGER protect_created_at
BEFORE UPDATE OF created_at ON your_table
BEGIN
    SELECT RAISE(ABORT, 'The created_at column cannot be updated');
END;

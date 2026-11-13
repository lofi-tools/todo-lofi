CREATE TABLE prompt_queues (
    task_id TEXT NOT NULL PRIMARY KEY,
    prompts_json TEXT NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks (id) ON DELETE CASCADE
);

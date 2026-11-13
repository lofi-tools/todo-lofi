ALTER TABLE entries ADD COLUMN entry_text TEXT NOT NULL DEFAULT '';
ALTER TABLE entries ADD COLUMN task_title TEXT NOT NULL DEFAULT '';
ALTER TABLE entries ADD COLUMN project_name TEXT NOT NULL DEFAULT '';
UPDATE entries SET
    entry_text = CASE WHEN json_valid(payload) THEN COALESCE(
        json_extract(payload, '$.kind.data.text'),
        (SELECT group_concat(json_extract(value, '$.text'), ' ')
         FROM json_each(payload, '$.kind.data.rows')),
        TRIM(COALESCE(json_extract(payload, '$.kind.data.title'), '') || ' ' ||
             COALESCE(json_extract(payload, '$.kind.data.output'), ''))
    ) ELSE '' END,
    task_title = COALESCE((SELECT title FROM tasks WHERE tasks.id = entries.task_id), ''),
    project_name = COALESCE((SELECT projects.name FROM tasks JOIN projects ON projects.id = tasks.project_id
                            WHERE tasks.id = entries.task_id), '');
CREATE INDEX entries_search ON entries USING fts (entry_text, task_title, project_name);

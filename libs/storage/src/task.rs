use crate::TodoStore;
use derive_entity_id::EntityId;
use snafu::{OptionExt, ResultExt};
use toasty::Deferred;
use toasty::Embed;
use toasty::Model;
use toasty::schema::Model;
use toasty::stmt::IntoExpr;

// TODO after https://github.com/tokio-rs/toasty/issues/1040: use as key once embed keys work with parent/subtasks relationship
#[derive(EntityId, Embed, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
#[entity_id(prefix = "task")]
pub struct TaskId(u64);

#[derive(Debug, Clone, Model)]
pub struct Task {
    #[key]
    #[auto]
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub branch_name: Option<String>,
    pub labels: Option<toasty::Json<Vec<String>>>,
    pub deadline: Option<u64>,
    #[default(1.0)]
    pub importance_factor: f64,
    #[default(1.0)]
    pub urgency_factor: f64,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
    #[update(jiff::Timestamp::now())]
    pub updated_at: jiff::Timestamp,
    #[index]
    pub parent_id: Option<u64>,
    #[has_many(pair = parent)]
    pub subtasks: Deferred<Vec<Task>>,
    #[belongs_to(key = parent_id, references = id)]
    pub parent: Deferred<Option<Task>>,
}
impl Task {
    /// Compute priority score matching the SQL formula in `list_tasks_by_priority`.
    pub fn compute_priority_score(&self, now_secs: u64) -> f64 {
        let deadline_factor = match self.deadline {
            None => 1.0,
            Some(dl) => {
                let diff = dl as f64 - now_secs as f64;
                86400.0_f64 / diff.max(1.0)
            }
        };
        self.importance_factor * deadline_factor
    }
}

pub type TaskCreate = <Task as toasty::schema::Model>::Create;

#[derive(Debug, Clone)]
pub struct TaskWithMeta {
    pub task: Task,
    pub direct_tags: Vec<String>,
    pub inferred_tags: Vec<String>,
}
impl std::ops::Deref for TaskWithMeta {
    type Target = Task;
    fn deref(&self) -> &Self::Target {
        &self.task
    }
}
impl TaskWithMeta {
    pub fn priority_score(&self, now_secs: u64) -> f64 {
        self.importance_factor * self.deadline_factor(now_secs)
    }

    pub fn deadline_factor(&self, now_secs: u64) -> f64 {
        match self.deadline {
            None => 1.0,
            Some(dl) => {
                let diff = dl as f64 - now_secs as f64;
                86400.0 / diff.max(1.0)
            }
        }
    }
}

fn parse_task_from_row(record: &toasty::stmt::Value) -> crate::QueryResult<TaskWithMeta> {
    let toasty::stmt::Value::Record(record) = record else {
        unreachable!("raw SQL queries return record rows");
    };

    let id =
        record
            .first()
            .and_then(|v| v.to_i64())
            .context(crate::error::UnexpectedValueSnafu {
                message: "expected i64 for id",
            })? as u64;
    let title = record
        .get(1)
        .and_then(|v| v.as_str())
        .context(crate::error::UnexpectedValueSnafu {
            message: "expected string for title",
        })?
        .to_owned();
    let description = record.get(2).and_then(|v| v.as_str()).map(str::to_owned);
    let branch_name = record.get(3).and_then(|v| v.as_str()).map(str::to_owned);
    let labels = record
        .get(4)
        .and_then(|v| v.as_str())
        .map(|s| toasty::Json(serde_json::from_str(s).unwrap_or_default()));
    let deadline = record.get(6).and_then(|v| v.to_u64());
    let importance_factor = record.get(7).and_then(|v| v.to_f64()).unwrap_or(1.0);
    let urgency_factor = record.get(8).and_then(|v| v.to_f64()).unwrap_or(1.0);
    let created_at = record
        .get(9)
        .and_then(|v| v.as_str())
        .context(crate::error::UnexpectedValueSnafu {
            message: "expected string for created_at",
        })?
        .parse::<jiff::Timestamp>()?;
    let updated_at = record
        .get(10)
        .and_then(|v| v.as_str())
        .context(crate::error::UnexpectedValueSnafu {
            message: "expected string for updated_at",
        })?
        .parse::<jiff::Timestamp>()?;
    let parent_id = record.get(11).and_then(|v| v.to_i64()).map(|id| id as u64);

    let task = Task {
        id,
        title,
        description,
        branch_name,
        labels,
        deadline,
        importance_factor,
        urgency_factor,
        created_at,
        updated_at,
        parent_id,
        subtasks: Deferred::default(),
        parent: Deferred::default(),
    };

    Ok(TaskWithMeta {
        task,
        direct_tags: Vec::new(),
        inferred_tags: Vec::new(),
    })
}

impl TodoStore {
    #[fastrace::trace]
    pub async fn create_task(
        &mut self,
        create: <Task as Model>::Create,
    ) -> crate::QueryResult<Task> {
        let created = create
            .exec(&mut self.db)
            .await
            .context(crate::error::CreateTaskSnafu)?;
        Ok(created)
    }

    #[fastrace::trace]
    pub async fn update_task_by_id(
        &mut self,
        _id: u64,
        _update: impl IntoExpr<u64>,
    ) -> crate::QueryResult<()> {
        todo!()
    }

    #[fastrace::trace]
    pub async fn get_task(&mut self, id: u64) -> crate::QueryResult<Task> {
        let task = Task::get_by_id(&mut self.db, id)
            .await
            .context(crate::error::GetTaskSnafu { id })?;
        Ok(task)
    }

    #[fastrace::trace]
    pub async fn list_tasks(&mut self) -> crate::QueryResult<Vec<Task>> {
        let tasks = Task::all()
            .exec(&mut self.db)
            .await
            .context(crate::error::ListTasksByPrioritySnafu)?;
        Ok(tasks)
    }

    #[fastrace::trace]
    pub async fn delete_task(&mut self, id: u64) -> crate::QueryResult<()> {
        Task::delete_by_id(&mut self.db, id)
            .await
            .context(crate::error::DeleteTaskSnafu { id })?;
        Ok(())
    }

    #[fastrace::trace]
    pub async fn list_tasks_by_priority(&mut self) -> crate::QueryResult<Vec<TaskWithMeta>> {
        let rows = toasty::sql::query(
            r#"
            SELECT
                id, title, description, branch_name, labels, blocked_by,
                deadline, importance_factor, urgency_factor, created_at, updated_at,
                parent_id
            FROM tasks
            ORDER BY
                importance_factor * CASE
                    WHEN deadline IS NULL THEN 1.0
                    ELSE 86400.0 / MAX(1.0,
                        CAST(deadline AS REAL) - CAST(strftime('%s', 'now') AS REAL)
                    )
                END DESC
            "#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::F64,
            toasty::stmt::Type::F64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::ListTasksByPrioritySnafu)?;

        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            let mut task = parse_task_from_row(&row)?;
            self.load_direct_tags(&mut task).await?;
            tasks.push(task);
        }

        Ok(tasks)
    }

    #[fastrace::trace]
    pub async fn list_tasks_by_tag(
        &mut self,
        tag_id: u64,
    ) -> crate::QueryResult<Vec<TaskWithMeta>> {
        let mut tag_ids = vec![tag_id];
        let descendants = self.get_all_descendants(tag_id).await?;
        tag_ids.extend(descendants.into_iter().map(|t| t.id));

        let id_list: Vec<String> = tag_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let query = format!(
            r#"
            SELECT DISTINCT
                t.id, t.title, t.description, t.branch_name, t.labels, t.blocked_by,
                t.deadline, t.importance_factor, t.urgency_factor, t.created_at, t.updated_at,
                t.parent_id
            FROM tasks t
            JOIN direct_task_tags dtt ON dtt.task_id = t.id
            WHERE dtt.tag_id IN ({})
            ORDER BY
                t.importance_factor * CASE
                    WHEN t.deadline IS NULL THEN 1.0
                    ELSE 86400.0 / MAX(1.0,
                        CAST(t.deadline AS REAL) - CAST(strftime('%s', 'now') AS REAL)
                    )
                END DESC
            "#,
            placeholders.join(",")
        );

        let rows = toasty::sql::query(&query)
            .column_types([
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::F64,
                toasty::stmt::Type::F64,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
            ])
            .exec(&mut self.db)
            .await
            .context(crate::error::ListTasksByTagSnafu { tag_id })?;

        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            let mut task = parse_task_from_row(&row)?;
            self.load_all_tags(&mut task).await?;
            tasks.push(task);
        }

        Ok(tasks)
    }

    pub async fn load_direct_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        let tags = self.get_direct_task_tags(task.id).await?;
        task.direct_tags = tags.into_iter().map(|t| t.name).collect();
        Ok(())
    }

    pub async fn load_inferred_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        let tags = self.get_inferred_task_tags(task.id).await?;
        task.inferred_tags = tags.into_iter().map(|t| t.name).collect();
        Ok(())
    }

    pub async fn load_all_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        self.load_direct_tags(task).await?;
        self.load_inferred_tags(task).await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use jiff::Timestamp;

    use crate::prelude::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_create_get_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(
                Task::create()
                    .title("Task 1".to_string())
                    .description(Some("Do Task 1".to_string()))
                    .branch_name(Some("fix/task-1".to_string()))
                    .labels(toasty::Json(vec!["bug".to_string(), "backend".to_string()]))
                    // .blocked_by(toasty::Json(vec![BlockerRef {
                    //     id: Some("task_00".to_string()),
                    // }]))
                    .importance_factor(1.0)
                    .urgency_factor(1.0),
            )
            .await
            .unwrap();

        let retrieved = storage.get_task(task.id).await?;
        assert_eq!(retrieved.id, task.id);
        assert_eq!(retrieved.title, "Task 1");

        Ok(())
    }

    #[tokio::test]
    async fn test_update_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage.create_task(Task::create().title("Task 1")).await?;

        let query = Task::update_by_id(task.id)
            .title("Updated Task 1")
            .description(Some("Updated description".to_string()))
            .importance_factor(2.0);
        query.exec(&mut storage.db).await?;

        let reloaded = storage.get_task(task.id).await?;
        assert_eq!(reloaded.title, "Updated Task 1");
        assert_eq!(reloaded.importance_factor, 2.0);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        for i in 1..=5 {
            storage
                .create_task(
                    Task::create()
                        .title(format!("Task {}", i))
                        .description(None)
                        .branch_name(None)
                        .labels(Some(toasty::Json(vec![])))
                        .importance_factor(1.0)
                        .urgency_factor(1.0),
                )
                .await?;
        }

        let tasks = storage.list_tasks().await?;
        assert_eq!(tasks.len(), 5);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks_by_priority() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        storage
            .create_task(
                Task::create()
                    .title("Low priority".to_string())
                    .importance_factor(1.0)
                    .deadline(Some(now_unix + 7 * 86400)),
            )
            .await?;

        storage
            .create_task(
                Task::create()
                    .title("High priority".to_string())
                    .importance_factor(2.0)
                    .deadline(Some(now_unix + 3600)),
            )
            .await?;

        storage
            .create_task(
                Task::create()
                    .title("Urgent no deadline".to_string())
                    .importance_factor(5.0),
            )
            .await?;

        let tasks = storage.list_tasks_by_priority().await?;
        assert_eq!(tasks.len(), 3);

        assert_eq!(tasks[0].title, "High priority");
        assert_eq!(tasks[1].title, "Urgent no deadline");
        assert_eq!(tasks[2].title, "Low priority");

        Ok(())
    }

    #[tokio::test]
    #[ignore = "triggers still experimental on turso, unsupported by toasty driver"]
    async fn test_db_schema__update_created_at_should_fail() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage.create_task(Task::create().title("Task 1")).await?;

        let original_created_at = task.created_at;

        let mut updated = task.clone();
        updated.created_at = Timestamp::now().checked_add(Duration::from_secs(1))?;
        updated.title = "Updated".to_string();

        let result = Task::update_by_id(task.id)
            .created_at(updated.created_at)
            .exec(&mut storage.db)
            .await;

        dbg!(&result);
        assert!(result.is_err());

        let reloaded = storage.get_task(task.id).await?;
        assert_eq!(
            reloaded.created_at, original_created_at,
            "created_at should not be mutable"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_create_task_with_parent() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let parent = storage
            .create_task(Task::create().title("Parent task".to_string()))
            .await?;

        let child = storage
            .create_task(
                Task::create()
                    .title("Child task".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;

        assert_eq!(child.parent_id, Some(parent.id));

        let fetched = storage.get_task(child.id).await?;
        assert_eq!(fetched.parent_id, Some(parent.id));

        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_priority_score_over_time() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let start = tokio::time::Instant::now();
        let start_timestamp = start.elapsed().as_secs();

        let task = storage
            .create_task(
                Task::create()
                    .title("Deadline task".to_string())
                    .importance_factor(2.0)
                    .deadline(Some(start_timestamp + 3600)),
            )
            .await?;

        let score_early = task.compute_priority_score(start_timestamp);

        tokio::time::advance(Duration::from_secs(59 * 60)).await;
        let score_closer = task.compute_priority_score(start.elapsed().as_secs());
        assert!(score_closer > score_early);

        tokio::time::advance(Duration::from_secs(2 * 60)).await;
        let prio_score_after = task.compute_priority_score(start.elapsed().as_secs());
        let max_score = task.importance_factor * 86400.0;
        assert!(
            prio_score_after <= max_score,
            "past-deadline score {prio_score_after} should be <= {max_score}"
        );

        let no_deadline = storage
            .create_task(
                Task::create()
                    .title("No deadline".to_string())
                    .importance_factor(3.0),
            )
            .await?;
        assert_eq!(no_deadline.compute_priority_score(start_timestamp), 3.0);

        Ok(())
    }
}

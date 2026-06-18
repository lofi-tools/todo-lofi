use crate::TodoStore;
use derive_entity_id::EntityId;
use snafu::OptionExt;
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
    pub blocked_by: Option<toasty::Json<Vec<BlockerRef>>>,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    pub id: Option<String>,
}

impl Task {
    /// Compute priority score matching the SQL formula in `list_tasks_by_priority`.
    ///
    /// `now_secs` is unix timestamp in seconds. When the deadline has passed,
    /// the denominator is capped at 1.0, bounding the score at
    /// `importance_factor * 86400.0`.
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

impl TodoStore {
    #[fastrace::trace]
    pub async fn create_task(&mut self, create: <Task as Model>::Create) -> crate::Result<Task> {
        let created = create.exec(&mut self.db).await?;
        Ok(created)
    }

    #[fastrace::trace]
    pub async fn update_task_by_id(
        &mut self,
        _id: u64,
        _update: impl IntoExpr<u64>,
    ) -> crate::Result<()> {
        todo!()
    }

    #[fastrace::trace]
    pub async fn get_task(&mut self, id: u64) -> crate::Result<Task> {
        let task = Task::get_by_id(&mut self.db, id).await?;
        Ok(task)
    }

    #[fastrace::trace]
    pub async fn list_tasks(&mut self) -> crate::Result<Vec<Task>> {
        let tasks = Task::all().exec(&mut self.db).await?;
        Ok(tasks)
    }

    #[fastrace::trace]
    pub async fn list_tasks_by_priority(&mut self) -> crate::Result<Vec<Task>> {
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
        .await?;

        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(record) = row else {
                unreachable!("raw SQL queries return record rows");
            };

            let id = record.first().and_then(|v| v.to_i64()).context(
                crate::error::UnexpectedValueSnafu {
                    message: "expected i64 for id",
                },
            )? as u64;
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
            let blocked_by = record
                .get(5)
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

            tasks.push(Task {
                id,
                title,
                description,
                branch_name,
                labels,
                blocked_by,
                deadline,
                importance_factor,
                urgency_factor,
                created_at,
                updated_at,
                parent_id,
                subtasks: Deferred::default(),
                parent: Deferred::default(),
            });
        }

        Ok(tasks)
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use crate::prelude::*;
    use jiff::Timestamp;
    use std::time::Duration;

    #[tokio::test]
    async fn test_create_get_task() -> crate::error::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(
                Task::create()
                    .title("Task 1".to_string())
                    .description(Some("Do Task 1".to_string()))
                    .branch_name(Some("fix/task-1".to_string()))
                    .labels(toasty::Json(vec!["bug".to_string(), "backend".to_string()]))
                    .blocked_by(toasty::Json(vec![BlockerRef {
                        id: Some("task_00".to_string()),
                    }]))
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
    async fn test_update_task() -> crate::error::Result<()> {
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
    async fn test_list_tasks() -> crate::error::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        for i in 1..=5 {
            storage
                .create_task(
                    Task::create()
                        .title(format!("Task {}", i))
                        .description(None)
                        .branch_name(None)
                        .labels(Some(toasty::Json(vec![])))
                        .blocked_by(Some(toasty::Json(vec![])))
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
    async fn test_list_tasks_by_priority() -> crate::error::Result<()> {
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
    async fn test_db_schema__update_created_at_should_fail() -> crate::error::Result<()> {
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
    async fn test_create_task_with_parent() -> crate::error::Result<()> {
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
    async fn test_priority_score_over_time() -> crate::error::Result<()> {
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

use crate::TodoStore;
use snafu::OptionExt;
use toasty::Model;
use toasty::schema::Model;
use toasty::stmt::IntoExpr;

#[derive(Debug, Clone, Model)]
pub struct Task {
    #[key]
    #[auto]
    pub id: i64,
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    pub id: Option<String>,
}

impl TodoStore {
    // #[tracing::instrument(skip(self, create))]
    #[fastrace::trace]
    pub async fn create_task(&mut self, create: <Task as Model>::Create) -> crate::Result<Task> {
        // tracing::info!("Creating task");
        let created = create.exec(&mut self.db).await?;
        // tracing::info!(task_id = %created.id, "Task created");
        Ok(created)
    }

    #[fastrace::trace]
    pub async fn update_task_by_id(
        &mut self,
        _id: i64,
        _update: impl IntoExpr<i64>,
    ) -> crate::Result<()> {
        // tracing::info!(task_id = %id, "Updating task");
        todo!()
    }

    #[fastrace::trace]
    pub async fn get_task(&mut self, id: i64) -> crate::Result<Task> {
        // tracing::info!(task_id = %id, "Getting task");
        let task = Task::get_by_id(&mut self.db, id).await?;
        // tracing::info!(title = %task.title, "Task retrieved");
        Ok(task)
    }

    #[fastrace::trace]
    pub async fn list_tasks(&mut self) -> crate::Result<Vec<Task>> {
        // tracing::info!("Listing all tasks");
        let tasks = Task::all().exec(&mut self.db).await?;
        // tracing::info!(count = %tasks.len(), "Tasks listed");
        Ok(tasks)
    }

    #[fastrace::trace]
    pub async fn list_tasks_by_priority(&mut self) -> crate::Result<Vec<Task>> {
        let rows = toasty::sql::query(
            r#"
            SELECT
                id, title, description, branch_name, labels, blocked_by,
                deadline, importance_factor, urgency_factor, created_at, updated_at
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
            )?;
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

        let task = storage
            .create_task(Task::create().id(0).title("Task 1"))
            .await?;

        let query = Task::update_by_id(task.id)
            .title("Updated Task 1")
            .description(Some("Updated description".to_string()))
            .importance_factor(2.0);
        // let stmt = query.build_stmt().;
        // println!("{stmt}");
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
        // TODO assert error msg

        let reloaded = storage.get_task(task.id).await?;
        assert_eq!(
            reloaded.created_at, original_created_at,
            "created_at should not be mutable"
        );

        Ok(())
    }
}

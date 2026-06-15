use crate::TodoStore;
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
    #[tracing::instrument(skip(self, create))]
    pub async fn create_task(&mut self, create: <Task as Model>::Create) -> toasty::Result<Task> {
        tracing::info!("Creating task");
        let created = create.exec(&mut self.db).await?;
        tracing::info!(task_id = %created.id, "Task created");
        Ok(created)
    }

    // pub async fn update_task(
    //     &mut self,
    //     id: i64,
    //     update: <Task as Model>::UpdateQuery,
    // ) -> toasty::Result<()> {
    //     // let mut existing = Task::get_by_id(&mut self.db, id).await?;
    //     Task::update_by_id(id)
    //         .title(&task.title)
    //         .description(&task.description)
    //         .branch_name(&task.branch_name)
    //         .labels(&task.labels)
    //         .blocked_by(&task.blocked_by)
    //         .importance_factor(task.importance_factor)
    //         .urgency_factor(task.urgency_factor)
    //         .exec(&mut self.db)
    //         .await?;
    //     Ok(())
    // }
    #[tracing::instrument(skip(self, _update))]
    pub async fn update_task_by_id(
        &mut self,
        id: i64,
        _update: impl IntoExpr<i64>,
    ) -> toasty::Result<()> {
        tracing::info!(task_id = %id, "Updating task");
        todo!()
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_task(&mut self, id: i64) -> toasty::Result<Task> {
        tracing::info!(task_id = %id, "Getting task");
        let task = Task::get_by_id(&mut self.db, id).await?;
        tracing::info!(title = %task.title, "Task retrieved");
        Ok(task)
    }

    #[tracing::instrument(skip(self))]
    pub async fn list_tasks(&mut self) -> toasty::Result<Vec<Task>> {
        tracing::info!("Listing all tasks");
        let tasks = Task::all().exec(&mut self.db).await?;
        tracing::info!(count = %tasks.len(), "Tasks listed");
        Ok(tasks)
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;

    use crate::prelude::*;

    #[tokio::test]
    async fn test_create_get_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await.unwrap();

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
    async fn test_update_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(Task::create().id(0).title("Task 1"))
            .await?;

        // let updated = Task {
        //     id: task.id,
        //     title: "Updated Task 1".to_string(),
        //     description: Some("Updated description".to_string()),
        //     branch_name: None,
        //     labels: Some(toasty::Json(vec!["feature".to_string()])),
        //     blocked_by: Some(toasty::Json(vec![])),
        //     importance_factor: 2.0,
        //     urgency_factor: 1.5,
        //     created_at: task.created_at,
        //     updated_at: Timestamp::now(),
        // };

        // storage.update_task(task.id, &updated).await?;
        Task::update_by_id(task.id)
            .title("Updated Task 1")
            .description(Some("Updated description".to_string()))
            .importance_factor(2.0)
            .exec(&mut storage.db)
            .await?;

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

    // #[tokio::test]
    // async fn test_db_schema__update_created_at_should_fail() -> anyhow::Result<()> {
    //     let mut storage = TodoStore::for_test().await?;

    //     let task = storage.create_task(Task::create().title("Task 1")).await?;

    //     let original_created_at = task.created_at;

    //     let mut updated = task.clone();
    //     updated.created_at = Timestamp::now().checked_add(Duration::from_secs(1))?;
    //     updated.title = "Updated".to_string();

    //     // let result = storage.update_task(task.id, &updated).await;
    //     let result = Task::update_by_id(task.id)
    //         .created_at(updated.created_at)
    //         .exec(&mut storage.db)
    //         .await;

    //     dbg!(&result);
    //     assert!(result.is_err());

    //     // if result.is_ok() {
    //     //     let reloaded = storage.get_task(task.id).await?;
    //     //     dbg!(
    //     //         &original_created_at,
    //     //         &updated.created_at,
    //     //         &reloaded.created_at
    //     //     );
    //     //     assert_eq!(
    //     //         reloaded.created_at, original_created_at,
    //     //         "created_at should not be mutable"
    //     //     );
    //     // }

    //     Ok(())
    // }
}

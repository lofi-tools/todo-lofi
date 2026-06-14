use crate::TodoStore;
use toasty::Model;
use toasty::schema::Model;

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
    pub async fn create_task(
        &mut self,
        task_create: <Task as Model>::Create,
    ) -> toasty::Result<Task> {
        let created = task_create.exec(&mut self.db).await?;
        Ok(created)
    }

    pub async fn update_task(&mut self, id: i64, task: &Task) -> toasty::Result<()> {
        let mut existing = Task::get_by_id(&mut self.db, id).await?;
        existing
            .update()
            .title(&task.title)
            .description(&task.description)
            .branch_name(&task.branch_name)
            .labels(&task.labels)
            .blocked_by(&task.blocked_by)
            .importance_factor(task.importance_factor)
            .urgency_factor(task.urgency_factor)
            .exec(&mut self.db)
            .await?;
        Ok(())
    }

    pub async fn get_task(&mut self, id: i64) -> toasty::Result<Task> {
        Task::get_by_id(&mut self.db, id).await
    }

    pub async fn list_tasks(&mut self) -> toasty::Result<Vec<Task>> {
        Task::all().exec(&mut self.db).await
    }
}

#[cfg(test)]
mod tests {
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

        let updated = Task {
            id: task.id,
            title: "Updated Task 1".to_string(),
            description: Some("Updated description".to_string()),
            branch_name: None,
            labels: Some(toasty::Json(vec!["feature".to_string()])),
            blocked_by: Some(toasty::Json(vec![])),
            importance_factor: 2.0,
            urgency_factor: 1.5,
            created_at: task.created_at,
            updated_at: jiff::Timestamp::now(),
        };

        storage.update_task(task.id, &updated).await?;

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
                // .create_task(Task {
                //     id: 0,
                //     title: format!("Task {}", i),
                //     description: None,
                //     branch_name: None,
                //     labels: toasty::Json(vec![]),
                //     blocked_by: toasty::Json(vec![]),
                //     importance_factor: 1.0,
                //     urgency_factor: 1.0,
                //     created_at: jiff::Timestamp::now(),
                //     updated_at: jiff::Timestamp::now(),
                // })
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
}

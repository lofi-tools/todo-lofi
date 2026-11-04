//! Persistence boundary. The Turso implementation is added in the migration
//! slice; this trait keeps the core free of a database-specific API.

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::projects::Project;
use crate::tasks::Task;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError>;
    async fn save_project(&self, project: &Project) -> Result<(), StoreError>;
    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError>;
    async fn save_task(&self, task: &Task) -> Result<(), StoreError>;
}

/// A non-durable store used to test the boundary before Turso is wired in.
#[derive(Default)]
pub struct MemoryStore {
    projects: RwLock<BTreeMap<String, Project>>,
    tasks: RwLock<BTreeMap<String, Task>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        let projects = self.projects.read().await;
        Ok(projects.values().cloned().collect())
    }

    async fn save_project(&self, project: &Project) -> Result<(), StoreError> {
        let mut projects = self.projects.write().await;
        projects.insert(project.id.as_str().to_string(), project.clone());
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let tasks = self.tasks.read().await;
        Ok(tasks.values().cloned().collect())
    }

    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        tasks.insert(task.id.as_str().to_string(), task.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::projects::ProjectId;
    use crate::tasks::{TaskId, TaskState};

    #[tokio::test]
    async fn the_memory_store_round_trips_projects_and_tasks()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = MemoryStore::new();
        let project = Project::new(directory.path().to_path_buf());
        store.save_project(&project).await?;

        let projects = store.load_projects().await?;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);

        let task = Task::draft(
            ProjectId::new("project-1"),
            "Boundary",
            Path::new("/tmp/project"),
        );
        store.save_task(&task).await?;
        let tasks = store.load_tasks().await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
        assert_eq!(tasks[0].state, TaskState::Draft);
        assert_eq!(TaskId::new(tasks[0].id.as_str()), task.id);
        Ok(())
    }
}

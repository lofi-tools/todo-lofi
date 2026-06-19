use std::{
    collections::BTreeSet,
    sync::{Arc, RwLock},
};

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub completed: bool,
    pub tags: Vec<String>,
}

pub struct TaskStore {
    pub tasks: Arc<RwLock<Vec<Task>>>,
}
impl TaskStore {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(vec![
                Task {
                    id: "1".into(),
                    title: "Learn GPUI".into(),
                    completed: false,
                    tags: vec!["coding".into(), "gpui".into()],
                },
                Task {
                    id: "2".into(),
                    title: "Build a todo app".into(),
                    completed: true,
                    tags: vec!["coding".into(), "rust".into()],
                },
                Task {
                    id: "3".into(),
                    title: "Explore gpui-component".into(),
                    completed: false,
                    tags: vec!["gpui".into()],
                },
            ])),
        }
    }
    pub fn tasks(&self) -> anyhow::Result<Vec<Task>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        Ok(tasks.clone())
    }
    pub fn insert_task(&self, idx: usize, title: &str) -> anyhow::Result<()> {
        let task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            completed: false,
            tags: vec![],
        };

        let mut tasks = self
            .tasks
            .write()
            .map_err(|e| anyhow::anyhow!("Failed to lock tasks: {}", e))?;

        if idx > tasks.len() {
            anyhow::bail!("Index {} out of bounds, max index is {}", idx, tasks.len());
        }

        tasks.insert(idx, task);
        Ok(())
    }
    pub fn toggle_task(&self, id: &str) -> anyhow::Result<()> {
        let mut tasks = self
            .tasks
            .write()
            .map_err(|e| anyhow::anyhow!("Failed to lock tasks: {}", e))?;

        if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
            task.completed = !task.completed;
        }

        Ok(())
    }
    pub fn all_tags(&self) -> anyhow::Result<BTreeSet<String>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;
        let mut all_tags = BTreeSet::new();
        for task in tasks.iter() {
            for tag in &task.tags {
                all_tags.insert(tag.clone());
            }
        }
        Ok(all_tags)
    }
}

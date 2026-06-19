use std::{
    collections::BTreeSet,
    sync::{Arc, RwLock},
};

use storage::prelude::*;

#[derive(Debug, Clone)]
pub struct UiTask {
    pub id: u64,
    pub title: String,
    pub completed: bool,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub deadline: Option<u64>,
    pub importance_factor: f64,
    pub urgency_factor: f64,
}

pub struct TaskStore {
    pub tasks: Arc<RwLock<Vec<UiTask>>>,
    pub all_tags_cache: Arc<RwLock<BTreeSet<String>>>,
}

impl TaskStore {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(Vec::new())),
            all_tags_cache: Arc::new(RwLock::new(BTreeSet::new())),
        }
    }

    pub async fn load_from_storage(store: &mut TodoStore) -> anyhow::Result<Self> {
        store.seed().await?;

        let tasks = store.list_tasks().await?;
        let all_tags = store.list_tags().await?;

        let mut ui_tasks = Vec::new();
        let mut tag_set = BTreeSet::new();

        for tag in &all_tags {
            tag_set.insert(tag.name.clone());
        }

        for task in &tasks {
            let direct_tags = store.get_direct_task_tags(task.id).await?;
            let tag_names: Vec<String> = direct_tags.into_iter().map(|t| t.name).collect();

            ui_tasks.push(UiTask {
                id: task.id,
                title: task.title.clone(),
                completed: false,
                tags: tag_names,
                description: task.description.clone(),
                deadline: task.deadline,
                importance_factor: task.importance_factor,
                urgency_factor: task.urgency_factor,
            });
        }

        tracing::info!(tasks = ui_tasks.len(), "Loaded tasks from storage");

        Ok(Self {
            tasks: Arc::new(RwLock::new(ui_tasks)),
            all_tags_cache: Arc::new(RwLock::new(tag_set)),
        })
    }

    pub fn tasks(&self) -> anyhow::Result<Vec<UiTask>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        Ok(tasks.clone())
    }

    pub fn insert_task(&self, idx: usize, title: &str) -> anyhow::Result<()> {
        let task = UiTask {
            id: 0,
            title: title.into(),
            completed: false,
            tags: vec![],
            description: None,
            deadline: None,
            importance_factor: 1.0,
            urgency_factor: 1.0,
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

    pub fn toggle_task(&self, id: u64) -> anyhow::Result<()> {
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
        let tags = self
            .all_tags_cache
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        Ok(tags.clone())
    }
}

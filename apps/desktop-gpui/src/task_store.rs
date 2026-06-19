use std::{
    collections::HashMap,
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
    pub top_level_tags: Arc<RwLock<Vec<String>>>,
    pub tag_descendants: Arc<RwLock<HashMap<String, Vec<String>>>>,
    pub tag_children: Arc<RwLock<HashMap<String, Vec<String>>>>,
    pub tag_parents: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl TaskStore {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(Vec::new())),
            top_level_tags: Arc::new(RwLock::new(Vec::new())),
            tag_descendants: Arc::new(RwLock::new(HashMap::new())),
            tag_children: Arc::new(RwLock::new(HashMap::new())),
            tag_parents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn load_from_storage(store: &mut TodoStore) -> anyhow::Result<Self> {
        store.seed().await?;

        let tasks = store.list_tasks().await?;
        let all_tags = store.list_tags().await?;
        let top_level = store.get_top_level_tags().await?;

        let mut ui_tasks = Vec::new();
        let mut tag_descendants = HashMap::new();
        let mut tag_children = HashMap::new();
        let mut tag_parents = HashMap::new();
        let mut top_level_names = Vec::new();

        for tag in &all_tags {
            let children = store.get_children(tag.id).await?;
            let child_names: Vec<String> = children.into_iter().map(|t| t.name).collect();
            tag_children.insert(tag.name.clone(), child_names);
        }

        for tag in &all_tags {
            let descendants = store.get_all_descendants(tag.id).await?;
            let descendant_names: Vec<String> = descendants.into_iter().map(|t| t.name).collect();
            tag_descendants.insert(tag.name.clone(), descendant_names);
        }

        for tag in &all_tags {
            let parents = store.get_parents(tag.id).await?;
            let parent_names: Vec<String> = parents.into_iter().map(|t| t.name).collect();
            tag_parents.insert(tag.name.clone(), parent_names);
        }

        for tag in &top_level {
            top_level_names.push(tag.name.clone());
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

        tracing::info!(
            tasks = ui_tasks.len(),
            top_level_tags = top_level_names.len(),
            "Loaded tasks from storage"
        );

        Ok(Self {
            tasks: Arc::new(RwLock::new(ui_tasks)),
            top_level_tags: Arc::new(RwLock::new(top_level_names)),
            tag_descendants: Arc::new(RwLock::new(tag_descendants)),
            tag_children: Arc::new(RwLock::new(tag_children)),
            tag_parents: Arc::new(RwLock::new(tag_parents)),
        })
    }

    pub fn tasks(&self) -> anyhow::Result<Vec<UiTask>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        Ok(tasks.clone())
    }

    pub fn top_level_tags(&self) -> anyhow::Result<Vec<String>> {
        let tags = self
            .top_level_tags
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        Ok(tags.clone())
    }

    pub fn children_of(&self, tag: &str) -> anyhow::Result<Vec<String>> {
        let map = self
            .tag_children
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;
        Ok(map.get(tag).cloned().unwrap_or_default())
    }

    pub fn tasks_for_tag(&self, tag: &str) -> anyhow::Result<Vec<UiTask>> {
        let descendants = {
            let map = self
                .tag_descendants
                .read()
                .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;
            map.get(tag).cloned().unwrap_or_default()
        };

        let mut tags_to_match = descendants;
        tags_to_match.push(tag.to_string());

        let tasks = self.tasks()?;
        let filtered: Vec<UiTask> = tasks
            .into_iter()
            .filter(|t| t.tags.iter().any(|tag| tags_to_match.contains(tag)))
            .collect();

        Ok(filtered)
    }

    pub fn path_to(&self, tag: &str) -> anyhow::Result<Vec<String>> {
        let map = self
            .tag_parents
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        let mut current = tag.to_string();
        let mut chain = vec![current.clone()];

        loop {
            match map.get(&current) {
                Some(parents) if !parents.is_empty() => {
                    current = parents[0].clone();
                    chain.push(current.clone());
                }
                _ => break,
            }
        }

        chain.reverse();
        Ok(chain)
    }

    pub fn ancestors_of(&self, tag: &str) -> anyhow::Result<Vec<String>> {
        let map = self
            .tag_parents
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        if let Some(parents) = map.get(tag) {
            for parent in parents {
                if visited.insert(parent.clone()) {
                    queue.push_back(parent.clone());
                    result.push(parent.clone());
                }
            }
        }

        while let Some(current) = queue.pop_front() {
            if let Some(parents) = map.get(&current) {
                for parent in parents {
                    if visited.insert(parent.clone()) {
                        queue.push_back(parent.clone());
                        result.push(parent.clone());
                    }
                }
            }
        }

        Ok(result)
    }

    pub fn insert_task(&self, idx: usize, title: &str, tags: Vec<String>) -> anyhow::Result<()> {
        let task = UiTask {
            id: 0,
            title: title.into(),
            completed: false,
            tags,
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
}

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use storage::prelude::*;

pub struct TaskStore {
    pub tasks: Arc<RwLock<Vec<TaskWithMeta>>>,
    pub completed: Arc<RwLock<HashSet<u64>>>,
    pub top_level_tags: Arc<RwLock<Vec<String>>>,
    pub tag_descendants: Arc<RwLock<HashMap<String, Vec<String>>>>,
    pub tag_children: Arc<RwLock<HashMap<String, Vec<String>>>>,
    pub tag_parents: Arc<RwLock<HashMap<String, Vec<String>>>>,
    store: Arc<tokio::sync::Mutex<TodoStore>>,
}

impl TaskStore {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let rt = tokio::runtime::Handle::current();
        let store = rt.block_on(TodoStore::new(&StorageConfig {
            db_uri: "turso::memory:".to_string(),
        })).unwrap();
        Self {
            tasks: Arc::new(RwLock::new(Vec::new())),
            completed: Arc::new(RwLock::new(HashSet::new())),
            top_level_tags: Arc::new(RwLock::new(Vec::new())),
            tag_descendants: Arc::new(RwLock::new(HashMap::new())),
            tag_children: Arc::new(RwLock::new(HashMap::new())),
            tag_parents: Arc::new(RwLock::new(HashMap::new())),
            store: Arc::new(tokio::sync::Mutex::new(store)),
        }
    }

    pub async fn load_from_storage(mut store: TodoStore) -> anyhow::Result<Self> {
        store.seed().await?;

        let tasks = store.list_tasks_by_priority().await?;
        let all_tags = store.list_tags().await?;
        let top_level = store.get_top_level_tags().await?;

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

        tracing::info!(
            tasks = tasks.len(),
            top_level_tags = top_level_names.len(),
            "Loaded tasks from storage"
        );

        Ok(Self {
            tasks: Arc::new(RwLock::new(tasks)),
            completed: Arc::new(RwLock::new(HashSet::new())),
            top_level_tags: Arc::new(RwLock::new(top_level_names)),
            tag_descendants: Arc::new(RwLock::new(tag_descendants)),
            tag_children: Arc::new(RwLock::new(tag_children)),
            tag_parents: Arc::new(RwLock::new(tag_parents)),
            store: Arc::new(tokio::sync::Mutex::new(store)),
        })
    }

    pub fn tasks(&self) -> anyhow::Result<Vec<TaskWithMeta>> {
        let tasks = self
            .tasks
            .read()
            .map_err(|e| anyhow::anyhow!("Failed to read lock: {}", e))?;

        Ok(tasks.clone())
    }

    pub fn is_completed(&self, id: u64) -> bool {
        self.completed
            .read()
            .map(|c| c.contains(&id))
            .unwrap_or(false)
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

    pub fn tasks_for_tag(&self, tag: &str) -> anyhow::Result<Vec<TaskWithMeta>> {
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
        let filtered: Vec<TaskWithMeta> = tasks
            .into_iter()
            .filter(|t| t.direct_tags.iter().any(|tag| tags_to_match.contains(tag)))
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

    pub fn insert_task(&self, _idx: usize, title: &str, tags: Vec<String>) -> anyhow::Result<()> {
        let store = self.store.clone();
        let title = title.to_string();

        let new_task = {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async {
                let mut s = store.lock().await;

                let task = s
                    .create_task(Task::create().title(title))
                    .await?;

                for tag_name in &tags {
                    let _ = s.assign_tag_to_task(task.id, tag_name).await;
                }

                let mut meta = TaskWithMeta {
                    task,
                    direct_tags: Vec::new(),
                    inferred_tags: Vec::new(),
                };
                s.load_all_tags(&mut meta).await?;
                Ok::<_, anyhow::Error>(meta)
            })?
        };

        let mut tasks = self
            .tasks
            .write()
            .map_err(|e| anyhow::anyhow!("Failed to lock tasks: {}", e))?;
        tasks.push(new_task);

        Ok(())
    }

    pub fn toggle_task(&self, id: u64) -> anyhow::Result<()> {
        let mut completed = self
            .completed
            .write()
            .map_err(|e| anyhow::anyhow!("Failed to lock completed: {}", e))?;

        if completed.contains(&id) {
            completed.remove(&id);
        } else {
            completed.insert(id);
        }

        Ok(())
    }
}

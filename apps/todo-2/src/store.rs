use gpui::{AppContext, Task};
use std::sync::Arc;
use storage::prelude::*;
use storage::task::TaskCreate;

#[derive(Clone)]
pub struct Store(pub(crate) Arc<tokio::sync::Mutex<TodoStore>>);

impl Store {
    pub fn new(store: TodoStore) -> Self {
        Store(Arc::new(tokio::sync::Mutex::new(store)))
    }

    /// Create a task and return the task list for the view the caller is
    /// on. When a tag is selected the new task is assigned to it and the
    /// result is that tag's task list; otherwise all tasks are returned.
    pub fn insert_task(
        &self,
        create: TaskCreate,
        tag_name: Option<String>,
        cx: &impl AppContext,
    ) -> gpui::Task<anyhow::Result<Vec<storage::TaskWithMeta>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let task = s.create_task(create).await?;
            let tasks = match &tag_name {
                Some(tag_name) => {
                    s.assign_tag_to_task(task.id, tag_name).await?;
                    let tag_id = s
                        .get_tag_by_name(tag_name)
                        .await?
                        .map(|t| t.id)
                        .ok_or_else(|| anyhow::anyhow!("tag not found: {tag_name}"))?;
                    s.list_tasks_by_tag(tag_id).await.unwrap_or_default()
                }
                None => s.list_tasks_by_priority().await.unwrap_or_default(),
            };
            Ok(tasks)
        })
    }

    pub fn list_top_level_tags(&self, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let tags = s.get_top_level_tags().await.unwrap_or_default();
            Ok(tags)
        })
    }

    pub fn get_children(
        &self,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let children = s.get_children(tag_id).await.unwrap_or_default();
            Ok(children)
        })
    }

    pub fn toggle_task_done(
        &self,
        task_id: u64,
        done: bool,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            tracing::info!(task_id, done, "toggle_task_done: before update");
            let mut s = store.lock().await;
            s.update_task_done(task_id, done).await?;
            tracing::info!(task_id, done, "toggle_task_done: after update, ok");
            Ok(())
        })
    }

    pub fn rename_task(
        &self,
        task_id: u64,
        title: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.update_task_title(task_id, &title).await?;
            Ok(())
        })
    }

    pub fn set_task_description(
        &self,
        task_id: u64,
        description: Option<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.update_task_description(task_id, description).await?;
            Ok(())
        })
    }

    pub fn list_blockers(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_blockers(task_id).await?)
        })
    }

    pub fn blocker_candidates(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.blocker_candidates(task_id).await?)
        })
    }

    pub fn add_blocker(
        &self,
        task_id: u64,
        blocker_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.add_blocker(task_id, blocker_id).await?;
            Ok(s.list_blockers(task_id).await?)
        })
    }

    pub fn remove_blocker(
        &self,
        task_id: u64,
        blocker_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.remove_blocker(task_id, blocker_id).await?;
            Ok(s.list_blockers(task_id).await?)
        })
    }

    pub fn set_blocked_until(
        &self,
        task_id: u64,
        blocked_until: Option<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.update_blocked_until(task_id, blocked_until).await?;
            Ok(())
        })
    }

    pub fn list_tasks_by_tag_name_with_labels(
        &self,
        tag_name: &str,
        path: &[String],
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<(Vec<storage::TaskWithMeta>, Vec<String>)>> {
        let store = self.0.clone();
        let tag_name = tag_name.to_string();
        let path = path.to_vec();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let tag_id = s
                .get_tag_by_name(&tag_name)
                .await?
                .map(|t| t.id)
                .ok_or_else(|| anyhow::anyhow!("tag not found: {tag_name}"))?;
            let tasks = s.list_tasks_by_tag(tag_id).await?;
            let mut labels = Vec::with_capacity(path.len());
            for name in &path {
                let label = s
                    .get_tag_by_name(name)
                    .await?
                    .map(|t| t.label())
                    .unwrap_or_else(|| name.clone());
                labels.push(label);
            }
            Ok((tasks, labels))
        })
    }
}

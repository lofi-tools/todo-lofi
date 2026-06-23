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

    pub fn insert_task(
        &self,
        create: TaskCreate,
        cx: &impl AppContext,
    ) -> gpui::Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let _ = s.create_task(create).await;
            let tasks = s.list_tasks().await.unwrap_or_default();
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

    pub fn get_children(&self, tag_id: u64, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
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
            let mut s = store.lock().await;
            s.update_task_done(task_id, done).await?;
            Ok(())
        })
    }

    pub fn list_tasks_by_tag_name(
        &self,
        tag_name: &str,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        let tag_name = tag_name.to_string();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let tag_id = s
                .get_tag_by_name(&tag_name)
                .await?
                .map(|t| t.id)
                .ok_or_else(|| anyhow::anyhow!("tag not found: {tag_name}"))?;
            let tasks = s.list_tasks_by_tag(tag_id).await?;
            Ok(tasks.into_iter().map(|t| t.task).collect())
        })
    }
}

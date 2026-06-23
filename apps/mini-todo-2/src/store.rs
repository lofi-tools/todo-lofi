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
}

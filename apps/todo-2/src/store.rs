use gpui::{AppContext, Task};
use std::sync::Arc;
use storage::prelude::*;
use storage::task::TaskCreate;

#[derive(Clone)]
pub struct Store(pub(crate) Arc<tokio::sync::Mutex<TodoStore>>);

/// Push a field delta to every Todoist task linked to `task_id`. No links
/// (or no token) → no-op, so purely local tasks never touch the network.
/// Must run on the Tokio runtime. A push failure fails the whole edit so
/// the UI reports it; the local edit itself is already saved.
async fn push_patch(
    store: &mut TodoStore,
    task_id: u64,
    patch: storage::todoist::TaskPatch,
) -> anyhow::Result<()> {
    let todoist_ids: std::collections::HashSet<u64> = store
        .list_integrations()
        .await?
        .into_iter()
        .filter(|i| i.provider == "todoist")
        .map(|i| i.id)
        .collect();
    let links: Vec<storage::TaskLink> = store
        .task_links_for_task(task_id)
        .await?
        .into_iter()
        .filter(|link| todoist_ids.contains(&link.integration_id))
        .collect();
    if links.is_empty() {
        return Ok(());
    }
    let token = crate::todoist_auth::access_token().await?;
    for link in links {
        store
            .push_todoist_patch(&token, link.integration_id, &link.external_id, task_id, &patch)
            .await?;
    }
    Ok(())
}

impl Store {
    pub fn new(store: TodoStore) -> Self {
        Store(Arc::new(tokio::sync::Mutex::new(store)))
    }

    /// Create a task and return its id plus the task list for the view the
    /// caller is on. When a tag is selected the new task is assigned to it
    /// and the result is that tag's task list; otherwise all tasks are
    /// returned. The id lets the caller auto-select the new task.
    pub fn insert_task(
        &self,
        create: TaskCreate,
        tag_name: Option<String>,
        cx: &impl AppContext,
    ) -> gpui::Task<anyhow::Result<(u64, Vec<storage::TaskWithMeta>)>> {
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
            Ok((task.id, tasks))
        })
    }

    /// Create a task as a subtask of `parent_id` (no tag assignment; the
    /// subtask inherits whatever view the caller refreshes).
    pub fn insert_subtask(
        &self,
        parent_id: u64,
        title: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::Task>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.create_task(
                TaskCreate::default()
                    .title(title)
                    .parent_id(Some(parent_id)),
            )
            .await?)
        })
    }

    pub fn list_top_level_tags(&self, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {        let store = self.0.clone();
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
            push_patch(&mut s, task_id, storage::todoist::TaskPatch {
                done: Some(done),
                ..Default::default()
            })
            .await?;
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
            push_patch(&mut s, task_id, storage::todoist::TaskPatch {
                content: Some(title),
                ..Default::default()
            })
            .await?;
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
            s.update_task_description(task_id, description.clone())
                .await?;
            push_patch(&mut s, task_id, storage::todoist::TaskPatch {
                description: Some(description),
                ..Default::default()
            })
            .await?;
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

    pub fn get_task(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::Task>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.get_task(task_id).await?)
        })
    }

    pub fn list_subtasks(
        &self,
        parent_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_subtasks(parent_id).await?)
        })
    }

    /// Create a follow-up task blocked by `blocked_by` (its blocker).
    pub fn create_follow_up(
        &self,
        blocked_by: u64,
        title: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::Task>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.create_follow_up(blocked_by, title).await?)
        })
    }

    /// Direct subtasks of any of `task_ids`, keyed by parent id. Used by
    /// the task list to collapse subtask rows under their parent and to
    /// show subtask progress.
    pub fn subtasks_map(
        &self,
        task_ids: Vec<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<std::collections::HashMap<u64, Vec<TaskWithMeta>>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.subtasks_map(&task_ids).await?)
        })
    }

    /// All tasks blocked by any of `task_ids`, keyed by blocker id (used
    /// to nest blocked tasks under their blocker in the task list).
    pub fn blocking_map(
        &self,
        task_ids: Vec<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<std::collections::HashMap<u64, Vec<storage::TaskWithMeta>>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.blocking_map(&task_ids).await?)
        })
    }

    /// All blockers of any of `task_ids`, keyed by blocked task id.
    pub fn blockers_map(
        &self,
        task_ids: Vec<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<std::collections::HashMap<u64, Vec<storage::TaskWithMeta>>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.blockers_map(&task_ids).await?)
        })
    }

    /// Tasks blocked by `task_id` (the reverse of its blockers).
    pub fn list_blocking_tasks(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_blocking_tasks(task_id).await?)
        })
    }

    /// The repeat template that `task_id` is an occurrence of, if any.
    pub fn get_repeat(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<storage::RepeatTaskTemplate>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.repeat_template_for_task(task_id).await?)
        })
    }

    /// Create (or update) the repeat template linking `task_id` as the
    /// first occurrence.
    pub fn set_repeat(
        &self,
        task_id: u64,
        name: String,
        interval_days: u64,
        time_of_day: Option<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::RepeatTaskTemplate>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.set_repeat(task_id, name, interval_days, time_of_day).await?)
        })
    }

    /// Delete the repeat template that `task_id` is an occurrence of.
    pub fn remove_repeat(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.remove_repeat(task_id).await?)
        })
    }

    pub fn list_after(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_after_tasks(task_id).await?)
        })
    }

    pub fn after_candidates(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.after_candidates(task_id).await?)
        })
    }

    pub fn add_after(
        &self,
        task_id: u64,
        after_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.add_after_link(task_id, after_id).await?;
            Ok(s.list_after_tasks(task_id).await?)
        })
    }

    pub fn remove_after(
        &self,
        task_id: u64,
        after_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.remove_after_link(task_id, after_id).await?;
            Ok(s.list_after_tasks(task_id).await?)
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

    /// Reload a task with all metadata (tags, blocked flag) from the DB.
    pub fn reload_task(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::TaskWithMeta>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.get_task_with_meta(task_id).await?)
        })
    }

    pub fn get_task_meta(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::TaskWithMeta>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.get_task_with_meta(task_id).await?)
        })
    }

    pub fn list_tasks_by_tag_name_with_labels(        &self,
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

    pub fn list_integrations(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::Integration>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_integrations().await?)
        })
    }

    pub fn create_integration(
        &self,
        provider: String,
        account_label: Option<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::Integration>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.create_integration(&provider, account_label).await?)
        })
    }

    pub fn delete_integration(
        &self,
        integration_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.delete_integration(integration_id).await?)
        })
    }

    pub fn create_tag(&self, name: String, cx: &impl AppContext) -> Task<anyhow::Result<Tag>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.create_tag(&name).await?)
        })
    }

    pub fn tag_link_providers(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<std::collections::HashMap<u64, String>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.tag_link_providers().await?)
        })
    }

    /// Sync all linked Todoist projects of every Todoist integration.    /// Runs entirely on the Tokio runtime (network + DB).
    pub fn sync_todoist(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::SyncSummary>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let token = crate::todoist_auth::access_token().await?;
            let mut backend = store.lock().await;
            let ids: Vec<u64> = backend
                .list_integrations()
                .await?
                .into_iter()
                .filter(|i| i.provider == "todoist")
                .map(|i| i.id)
                .collect();
            let mut total = storage::SyncSummary::default();
            for id in ids {
                let summary = backend.sync_todoist_integration(&token, id).await?;
                total.projects += summary.projects;
                total.sections += summary.sections;
                total.tasks_upserted += summary.tasks_upserted;
                total.tasks_tombstoned += summary.tasks_tombstoned;
            }
            Ok(total)
        })
    }

    /// Section display order plus task-id → section map for a tag view.
    /// Empty when the tag has no sectioned tasks.
    pub fn task_section_groups(
        &self,
        tag_name: String,
        task_ids: Vec<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<(Vec<String>, std::collections::HashMap<u64, String>)>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let Some(tag) = s.get_tag_by_name(&tag_name).await? else {
                return Ok((Vec::new(), std::collections::HashMap::new()));
            };
            Ok(s.section_groups_for_tasks(tag.id, &task_ids).await?)
        })
    }
}

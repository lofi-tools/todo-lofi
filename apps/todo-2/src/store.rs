use gpui::{AppContext, Task};
use std::sync::Arc;
use storage::prelude::*;
use storage::task::TaskCreate;

use crate::coding_git;

#[derive(Clone)]
pub struct Store(pub(crate) Arc<tokio::sync::Mutex<TodoStore>>);

/// One try plus three retries for a transient GitHub failure (decision 22).
const GITHUB_SYNC_ATTEMPTS: u32 = 4;
/// Ceiling on the retry backoff, so a long rate-limit reset still surfaces.
const GITHUB_SYNC_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(300);

/// One worktree a run will use in one repo. Multi-repo projects get several,
/// each with its own base branch and remote (decision 21).
#[derive(Debug, Clone)]
struct WorktreePlan {
    repo_dir: std::path::PathBuf,
    worktree_path: std::path::PathBuf,
    branch: String,
    base_branch: String,
    remote: String,
}

/// Tell the apps that captured a freshly created task about it. Todoist
/// turns that into a remote item, so adding a task to a linked tag also
/// adds it to the user's Todoist project. Failures are logged rather than
/// returned: the local task already exists, and failing here would leave
/// the UI showing stale state instead of the task the user just typed.
async fn push_captured_task(store: &mut TodoStore, task_id: u64) -> anyhow::Result<()> {
    let has_todoist = store
        .list_integrations()
        .await?
        .iter()
        .any(|integration| integration.provider == "todoist");
    if !has_todoist {
        return Ok(());
    }
    let token = crate::todoist_auth::access_token().await?;
    store.push_todoist_new_task(&token, task_id).await?;
    Ok(())
}

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
                    // An app managing this tag may capture new tasks (e.g.
                    // push them to Todoist). The task itself stays the
                    // user's: capture never makes it read-only.
                    let captured = s.capture_task(task.id).await?;
                    if !captured.is_empty()
                        && let Err(error) = push_captured_task(&mut s, task.id).await
                    {
                        tracing::error!(
                            task_id = task.id,
                            %error,
                            "capture push failed; the task stays local"
                        );
                    }
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
            // A synced task keeps its local completion on the next pull and
            // pushes it as `state` (spec §5.5).
            s.stamp_local_issue_field(task_id, "state").await?;
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
            s.stamp_local_issue_field(task_id, "title").await?;
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
            s.stamp_local_issue_field(task_id, "body").await?;
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
        start_time_of_day: Option<u64>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::RepeatTaskTemplate>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s
                .set_repeat(task_id, name, interval_days, time_of_day, start_time_of_day)
                .await?)
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

    /// Tag view including tasks that start more than 2 days out (the
    /// "show all" toggle reveals them from the fetched rows).
    pub fn list_tasks_by_tag_name_with_labels_including_distant(
        &self,
        tag_name: &str,
        path: &[String],
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<(Vec<storage::TaskWithMeta>, Vec<String>)>> {
        self.list_tasks_by_tag_impl(tag_name, path, true, cx)
    }

    fn list_tasks_by_tag_impl(
        &self,
        tag_name: &str,
        path: &[String],
        include_distant: bool,
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
            let tasks = if include_distant {
                s.list_tasks_by_tag_including_distant(tag_id).await?
            } else {
                s.list_tasks_by_tag(tag_id).await?
            };
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

    /// Link a local tag to a remote (Todoist) project so re-syncs reuse it.
    /// Used by the collision dialog's "sync into the same tag" choice.
    pub fn link_tag(
        &self,
        integration_id: u64,
        external_id: String,
        tag_id: u64,
        source_kind: String,
        namespaced: bool,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s
                .link_tag(integration_id, &external_id, tag_id, &source_kind, namespaced)
                .await?)
        })
    }

    /// The Todoist integration, if connected.
    pub fn todoist_integration(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<storage::Integration>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s
                .list_integrations()
                .await?
                .into_iter()
                .find(|i| i.provider == "todoist"))
        })
    }

    /// Remote-project links of one integration with their local tag labels:
    /// `(external_id, source_kind, tag_id, tag_label)`.
    pub fn integration_tag_pairs(
        &self,
        integration_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<(String, String, u64, String)>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let mut pairs = Vec::new();
            for link in s.tag_links_for_integration(integration_id).await? {
                let label = s
                    .get_tag(link.tag_id)
                    .await
                    .map(|tag| tag.label())
                    .unwrap_or_else(|_| "(deleted tag)".to_string());
                pairs.push((link.external_id, link.source_kind, link.tag_id, label));
            }
            pairs.sort_by(|a, b| a.3.cmp(&b.3));
            Ok(pairs)
        })
    }

    /// Remove a remote-project ↔ local-tag pairing. Local tasks are kept.
    pub fn unlink_tag(
        &self,
        integration_id: u64,
        external_id: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.unlink_tag(integration_id, &external_id).await?)
        })
    }

    /// Remote Todoist projects for the stored account. Network I/O runs on
    /// the Tokio runtime via `Tokio::spawn_result`.
    pub fn todoist_remote_projects(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<(String, String)>>> {
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let projects = crate::todoist_auth::list_projects().await?;
            Ok(projects
                .into_iter()
                .map(|project| (project.id, project.name))
                .collect())
        })
    }

    /// Directories configured for a tag (`tag_settings.dirs`), used by the
    /// agent pane to resolve a project's launch directory.
    pub fn tag_dirs(&self, tag_id: u64, cx: &impl AppContext) -> Task<anyhow::Result<Vec<String>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.tag_settings(tag_id).await?.dirs)
        })
    }

    /// The whole tag hierarchy, fully expanded and already in render order:
    /// projects first, then plain tags, then sections, each alphabetically.
    /// The nav folds it to the selected path, while the tag settings pane
    /// shows the full tree.
    pub fn tag_tree_rows(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::TagTreeRow>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.tag_tree_rows().await?)
        })
    }

    /// The tags `tag_id` is placed under.
    pub fn tag_parents(&self, tag_id: u64, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.get_parents(tag_id).await?)
        })
    }

    /// Ids of every tag below `tag_id`, so a parent picker can leave out the
    /// choices the store would reject as a cycle.
    pub fn tag_descendant_ids(
        &self,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<u64>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.get_all_descendants(tag_id)
                .await?
                .into_iter()
                .map(|tag| tag.id)
                .collect())
        })
    }

    /// Replace the tags `child_id` is placed under: each label in
    /// `parent_names` is resolved to an existing tag or created, then the
    /// placement edges are diffed against the current parents.
    pub fn set_tag_parents(
        &self,
        child_id: u64,
        parent_names: Vec<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.set_tag_placements(child_id, &parent_names).await?)
        })
    }

    /// Replace a tag's directory list. Any tag may have dirs; that is what
    /// makes it directory-backed (folder icon, agent pane, and ineligibility
    /// for app bindings).
    pub fn set_tag_dirs(
        &self,
        tag_id: u64,
        dirs: Vec<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.set_tag_dirs(tag_id, dirs).await?)
        })
    }

    /// The sections of a tag, in display order.
    pub fn tag_sections(
        &self,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<TagSection>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.tag_sections(tag_id).await?)
        })
    }

    /// Create a section: the `tag_sections` row *and* its child tag, so it
    /// appears in the nav and can hold tasks.
    pub fn create_section(
        &self,
        tag_id: u64,
        name: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.create_section(tag_id, name).await?;
            Ok(())
        })
    }

    /// Remove a section and its child tag.
    pub fn remove_section(
        &self,
        tag_id: u64,
        section_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.remove_section(tag_id, section_id).await?)
        })
    }

    /// Move a section one slot up (`-1`) or down (`1`).
    pub fn move_section(
        &self,
        tag_id: u64,
        section_id: u64,
        delta: i64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.move_section(tag_id, section_id, delta).await?)
        })
    }

    /// The app bindings on a tag, paired with the owning app for display.
    pub fn bindings_for_tag(
        &self,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<(App, AppTagBinding)>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let bindings = s.bindings_for_tag(tag_id).await?;
            let apps = s.list_apps().await?;
            Ok(bindings
                .into_iter()
                .filter_map(|binding| {
                    apps.iter()
                        .find(|app| app.id == binding.app_id)
                        .cloned()
                        .map(|app| (app, binding))
                })
                .collect())
        })
    }

    /// Look up a tag by name (used to detect managed tags on selection).
    pub fn get_tag_by_name(
        &self,
        name: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<storage::Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.get_tag_by_name(&name).await?)
        })
    }

    /// Replace a task's direct tags (assigning new ones, unassigning removed).
    /// Tagging an existing task with a linked tag captures it, so the task
    /// shows up in Todoist too; an already-mirrored task is left alone.
    pub fn set_task_tags(
        &self,
        task_id: u64,
        tags: Vec<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.set_task_tags(task_id, &tags).await?;
            let captured = s.capture_task(task_id).await?;
            if !captured.is_empty()
                && let Err(error) = push_captured_task(&mut s, task_id).await
            {
                tracing::error!(
                    task_id,
                    %error,
                    "capture push failed; the task stays local"
                );
            }
            Ok(())
        })
    }

    /// All known tags, for the tag editor's fuzzy finder and misspelling check.
    pub fn list_tags(&self, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_tags().await?)
        })
    }

    /// The recipe that manages `tag_id`, if the tag is owned by an
    /// automation (drives the special panel in the Layout).
    pub fn managed_recipe_for_tag(
        &self,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<u64>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.managed_recipe_for_tag(tag_id).await?)
        })
    }

    /// Enable a managed-tag automation: create its tag (idempotent).
    pub fn enable_managed_recipe(
        &self,
        recipe_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::Tag>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.enable_managed_recipe(recipe_id).await?)
        })
    }

    /// Create a trip of the travel automation inside `tag_id`: replaces the
    /// app's own unfinished items there and spawns the new checklist into
    /// the tag's Pack / Before leaving sections.
    pub fn create_trip(
        &self,
        recipe_id: u64,
        tag_id: u64,
        name: String,
        days: String,
        activities: Vec<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::WorkflowRun>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.create_trip(recipe_id, tag_id, name, days, activities)
                .await?)
        })
    }

    /// ─── Apps ─────────────────────────────────────────────────────────────
    /// Every registered app with the tags it is bound to, for the Apps panel.
    pub fn list_apps(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<(App, Vec<(AppTagBinding, storage::Tag)>)>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let mut apps = Vec::new();
            for app in s.list_apps().await? {
                let mut bindings = Vec::new();
                for binding in s.bindings_for_app(app.id).await? {
                    let tag = s.get_tag(binding.tag_id).await?;
                    bindings.push((binding, tag));
                }
                apps.push((app, bindings));
            }
            Ok(apps)
        })
    }

    /// Attach an app to a tag and mark it installed. Recipe apps also get
    /// their sections provisioned in the tag, so the attachment works
    /// immediately (the tag gets the automation's panel).
    pub fn attach_app_to_tag(
        &self,
        app_id: u64,
        tag_id: u64,
        role: BindingRole,
        capture_new_tasks: bool,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.attach_app_to_tag(app_id, tag_id, role, capture_new_tasks)
                .await?;
            if let Some(recipe_id) = s.recipe_for_app(app_id).await? {
                s.ensure_recipe_sections(recipe_id, tag_id).await?;
            }
            s.set_app_enabled(app_id, true).await?;
            Ok(())
        })
    }

    /// Flip whether new tasks landing in a bound tag are captured by the app.
    pub fn set_binding_capture(
        &self,
        app_id: u64,
        tag_id: u64,
        capture: bool,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.set_binding_capture(app_id, tag_id, capture).await?;
            Ok(())
        })
    }

    /// The app registered for an integration, so the Integrations panel can
    /// offer that app's settings on its card.
    pub fn app_for_integration(
        &self,
        integration_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<App>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.app_for_integration(integration_id).await?)
        })
    }

    /// Detach one app from one tag: its sections there are downgraded (or
    /// removed when empty) and the binding goes; tasks keep their ownership.
    pub fn detach_app_from_tag(
        &self,
        app_id: u64,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.release_app_from_tag(app_id, tag_id).await?;
            Ok(())
        })
    }

    /// Remove an app everywhere. When `remove_owned_items`, its unfinished
    /// generated items are tombstoned; completed and user-modified items
    /// survive as ordinary tasks.
    pub fn remove_app(
        &self,
        app_id: u64,
        remove_owned_items: bool,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.disable_app(app_id, remove_owned_items).await?;
            Ok(())
        })
    }

    /// Recipe summaries (id, slug, name) for the run picker.
    pub fn list_recipe_metas(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::RecipeMeta>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_recipe_metas().await?)
        })
    }

    /// Start a run of `recipe_id` with default params.
    pub fn start_workflow_run(
        &self,
        recipe_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.create_run(recipe_id, serde_json::json!({}), None).await?;
            Ok(())
        })
    }

    /// Disable an automation: cancel every active run of its recipe.
    pub fn disable_automation(
        &self,
        recipe_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<usize>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.cancel_active_runs(recipe_id).await?)
        })
    }

    /// Active runs with their steps, for the run banner.
    pub fn list_active_run_views(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::RunView>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_active_run_views().await?)
        })
    }

    /// Complete a workflow step with a result (approve/reject, …).
    pub fn complete_workflow_step(
        &self,
        task_id: u64,
        result: serde_json::Value,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.complete_workflow_step(task_id, result).await?;
            Ok(())
        })
    }

    /// Fire an event wait (webhook/button): resolves matching "Await …"
    /// steps and continues the run.
    pub fn fire_workflow_event(
        &self,
        run_id: u64,
        event_name: String,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.trigger_event(run_id, &event_name).await?;
            Ok(())
        })
    }

    /// Cancel a run: tombstones its steps and marks the run cancelled.
    pub fn cancel_workflow_run(
        &self,
        run_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.cancel_run(run_id).await?;
            Ok(())
        })
    }

    /// Materialize repeat occurrences startable or due within the next
    /// 2 days. Idempotent; safe to call on startup and date rollovers.
    pub fn materialize_due_occurrences(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<usize>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Ok(s.materialize_daily_occurrences(now_secs).await?)
        })
    }    /// Runs entirely on the Tokio runtime (network + DB).
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

    /// ─── Coding workflow ──────────────────────────────────────────────────
    /// Start the `coding-task` run whose root is `task_id` (the feature task).
    /// Returns the new run id. Fails when the task or its project already has
    /// an active run.
    pub fn start_coding_run(&self, task_id: u64, cx: &impl AppContext) -> Task<anyhow::Result<u64>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let recipe_id = s
                .recipe_id_by_slug("coding-task")
                .await?
                .ok_or_else(|| anyhow::anyhow!("the coding-task recipe is missing"))?;
            let run = s
                .create_task_run(task_id, recipe_id, serde_json::json!({}))
                .await?;
            Ok(run.id)
        })
    }

    /// The newest coding run rooted at `task_id` with its steps, for the
    /// details panel stepper.
    pub fn coding_run_for_task(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<storage::RunView>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.coding_run_for_task(task_id).await?)
        })
    }

    /// Whether `task_id` sits in a directory-backed project: some ancestor
    /// task (itself included) carries a `project:` tag, or a tag with a
    /// configured directory, that exists on disk. Only such tasks get a
    /// coding run; travel and other non-directory tags never do.
    pub fn coding_directory(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<bool>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(Self::project_dir(&mut s, task_id).await?.is_some())
        })
    }

    /// Store the spec the interview produced (or one the user pasted) and
    /// complete that run's open interview step, which advances the run to the
    /// spec gate.
    pub fn save_coding_spec(
        &self,
        task_id: u64,
        spec: String,
        spec_path: Option<String>,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            s.save_task_spec(task_id, Some(spec), spec_path).await?;
            let Some(view) = s.coding_run_for_task(task_id).await? else {
                return Ok(());
            };
            if view.run.status != "active" {
                return Ok(());
            }
            s.append_run_note(view.run.id, "spec", "interview", "interview", "Spec saved")
                .await?;
            if let Some(step) = view.steps.iter().find(|step| {
                step.node.id == "interview" && !step.task.done
            }) {
                let step_id = step.task.id;
                s.complete_workflow_step(step_id, serde_json::json!({})).await?;
            }
            Ok(())
        })
    }

    /// Approve the spec gate: cut the feature branch (once — a later cycle
    /// reuses it) and let the run spawn the implement phase. Returns the branch
    /// the run is on.
    pub fn approve_coding_spec(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<String>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let view = s
                .coding_run_for_task(task_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no coding run for this task"))?;
            let Some(step) = view
                .steps
                .iter()
                .find(|step| step.node.id == "spec" && !step.task.done)
            else {
                anyhow::bail!("the spec is not waiting for approval");
            };
            let fallback = Self::default_branch_name(&view, task_id, &mut s).await?;
            let desired = Self::proposed_branch(&view).unwrap_or_else(|| fallback.clone());
            // A rejection sends the run back to the interview, but the branch
            // survives it: only the first approval creates the branch, later
            // cycles keep working on it.
            let branch = if view.run.branch_status.as_deref() == Some("active") {
                s.set_run_branch(view.run.id, &desired, view.run.base_branch.as_deref())
                    .await?;
                s.append_run_note(
                    view.run.id,
                    "annotation",
                    "spec",
                    "spec",
                    &format!("Continuing on {desired}"),
                )
                .await?;
                desired
            } else {
                let repos = Self::project_repo_dirs(&mut s, task_id).await?;
                if repos.is_empty() {
                    anyhow::bail!("set this project's directory first");
                }
                let run_id = view.run.id;
                let plans = tokio::task::spawn_blocking(move || {
                    Self::create_run_worktrees(&repos, &desired, &fallback, run_id)
                })
                .await??;
                let primary = plans
                    .first()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("this project has no git repository to work in"))?;
                for plan in &plans {
                    s.insert_run_worktree(&NewRunWorktree {
                        run_id,
                        repo_dir: plan.repo_dir.display().to_string(),
                        worktree_path: plan.worktree_path.display().to_string(),
                        branch: plan.branch.clone(),
                        base_branch: plan.base_branch.clone(),
                        remote: plan.remote.clone(),
                    })
                    .await?;
                }
                s.set_run_branch(run_id, &primary.branch, Some(&primary.base_branch))
                    .await?;
                let extra = match plans.len() {
                    0 | 1 => String::new(),
                    more => format!(" (+{} more repo(s))", more - 1),
                };
                s.append_run_note(
                    run_id,
                    "branch",
                    "spec",
                    "spec",
                    &format!(
                        "Working in {} on {} from {}{extra}",
                        primary.worktree_path.display(),
                        primary.branch,
                        primary.base_branch,
                    ),
                )
                .await?;
                primary.branch.clone()
            };
            s.complete_workflow_step(step.task.id, serde_json::json!({ "approved": true }))
                .await?;
            Ok(branch)
        })
    }

    /// Merge the run's feature branch into its base branch and complete the
    /// run, removing the run's worktrees while keeping its branches for the
    /// cleanup list (decision 28). On conflict the merge is aborted so the
    /// tree stays usable.
    pub fn merge_coding_branch(
        &self,
        run_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let run = s.get_run(run_id).await?;
            let branch = run
                .branch
                .clone()
                .ok_or_else(|| anyhow::anyhow!("this run has no branch to merge"))?;
            let base = run
                .base_branch
                .clone()
                .ok_or_else(|| anyhow::anyhow!("this run has no base branch"))?;
            let root = run
                .root_task_id
                .ok_or_else(|| anyhow::anyhow!("this run has no feature task"))?;
            let worktrees = s.run_worktrees(run_id).await?;
            if worktrees.is_empty() {
                // A run that predates worktrees still merges in the user's
                // checkout, guard and all.
                let dir = Self::project_dir(&mut s, root)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("set this project's directory first"))?;
                tokio::task::spawn_blocking(move || Self::merge_into_base(&dir, &base, &branch))
                    .await??;
            } else {
                let pending: Vec<&RunWorktree> = worktrees
                    .iter()
                    .filter(|worktree| worktree.removed_at.is_none())
                    .collect();
                let merges: Vec<(std::path::PathBuf, String, String)> = pending
                    .iter()
                    .map(|worktree| {
                        (
                            std::path::PathBuf::from(&worktree.repo_dir),
                            worktree.base_branch.clone(),
                            worktree.branch.clone(),
                        )
                    })
                    .collect();
                let removals: Vec<(std::path::PathBuf, std::path::PathBuf)> = pending
                    .iter()
                    .map(|worktree| {
                        (
                            std::path::PathBuf::from(&worktree.repo_dir),
                            std::path::PathBuf::from(&worktree.worktree_path),
                        )
                    })
                    .collect();
                tokio::task::spawn_blocking(move || {
                    for (dir, base, branch) in &merges {
                        Self::merge_into_base(dir, base, branch)?;
                    }
                    for (repo_dir, worktree_path) in &removals {
                        if worktree_path.is_dir() {
                            coding_git::worktree_remove(repo_dir, worktree_path)?;
                        } else {
                            // Deleted by hand: prune clears the stale admin
                            // entry instead of failing the merge.
                            coding_git::worktree_prune(repo_dir)?;
                        }
                    }
                    Ok::<(), anyhow::Error>(())
                })
                .await??;
                for worktree in pending {
                    s.mark_run_worktree_removed(worktree.id).await?;
                }
            }
            s.complete_coding_merge(run_id).await?;
            Ok(())
        })
    }

    /// Start a `coding-sub-interview` run rooted at a sub-task, so the model
    /// can be asked to clarify it without touching the parent run.
    pub fn request_sub_task_interview(
        &self,
        sub_task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let recipe_id = s
                .recipe_id_by_slug("coding-sub-interview")
                .await?
                .ok_or_else(|| anyhow::anyhow!("the coding-sub-interview recipe is missing"))?;
            s.create_task_run(sub_task_id, recipe_id, serde_json::json!({}))
                .await?;
            Ok(())
        })
    }

    /// Cancelled or completed coding runs that still hold a branch.
    pub fn list_branch_cleanup_runs(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Vec<storage::RunView>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.list_branch_cleanup_runs().await?)
        })
    }

    /// Delete a run's branch with `git branch -D` and detach it from the run.
    pub fn delete_coding_branch(
        &self,
        run_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<()>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let run = s.get_run(run_id).await?;
            let Some(branch) = run.branch.clone() else {
                return Ok(());
            };
            // The run's base branch is where a merge left us; a root task is
            // enough to resolve the directory either way.
            if let Some(root) = run.root_task_id
                && let Some(dir) = Self::project_dir(&mut s, root).await?
            {
                let dir_for_git = dir.clone();
                let branch_for_git = branch.clone();
                tokio::task::spawn_blocking(move || {
                    coding_git::delete_branch(&dir_for_git, &branch_for_git)
                })
                .await??;
            }
            s.clear_run_branch(run_id).await?;
            Ok(())
        })
    }

    /// Create one worktree per repo at spec approval (§6.2):
    /// `<repo>/worktrees/<branch-slug>`, excluded from git's view and cut from
    /// that repo's own base branch. The user's checkout is never switched,
    /// which is what retires the old dirty-tree refusal at this call site.
    fn create_run_worktrees(
        repos: &[std::path::PathBuf],
        desired: &str,
        fallback: &str,
        run_id: u64,
    ) -> anyhow::Result<Vec<WorktreePlan>> {
        let mut plans = Vec::new();
        for repo_dir in repos {
            if !coding_git::is_repo(repo_dir) {
                continue;
            }
            // Each repo can sit on a different branch, so each worktree
            // records its own base.
            let base_branch = coding_git::current_branch(repo_dir)?;
            let branch = if !coding_git::branch_exists(repo_dir, desired) {
                desired.to_string()
            } else if desired != fallback && !coding_git::branch_exists(repo_dir, fallback) {
                fallback.to_string()
            } else {
                anyhow::bail!(
                    "both {desired} and {fallback} already exist in {}; delete one first",
                    repo_dir.display()
                );
            };
            let slug = branch.replace('/', "-");
            let mut worktree_path = repo_dir.join("worktrees").join(&slug);
            if worktree_path.exists() {
                // Never reuse another run's checkout (spec §8).
                worktree_path = repo_dir.join("worktrees").join(format!("{slug}-{run_id}"));
            }
            if worktree_path.exists() {
                anyhow::bail!("{} already exists; remove it first", worktree_path.display());
            }
            coding_git::worktree_add(repo_dir, &worktree_path, &branch, &base_branch)?;
            let remote = coding_git::resolve_remote(repo_dir)
                .map(|remote| remote.name)
                .unwrap_or_default();
            plans.push(WorktreePlan {
                repo_dir: repo_dir.clone(),
                worktree_path,
                branch,
                base_branch,
                remote,
            });
        }
        if plans.is_empty() {
            anyhow::bail!("this project has no git repository to work in");
        }
        Ok(plans)
    }

    /// Switch to the base branch and merge the feature branch into it.
    fn merge_into_base(dir: &std::path::Path, base: &str, branch: &str) -> anyhow::Result<()> {
        if !coding_git::is_repo(dir) {
            anyhow::bail!("{} is not a git repository", dir.display());
        }
        let changed = coding_git::changed_paths(dir)?;
        if !changed.is_empty() {
            let listed: Vec<&str> = changed.iter().take(5).map(String::as_str).collect();
            anyhow::bail!(
                "commit or stash these changes before merging: {}",
                listed.join(", ")
            );
        }
        coding_git::switch_branch(dir, base)?;
        if let Err(error) = coding_git::merge_no_ff(dir, branch) {
            // Leave the tree clean so the user can resolve the conflict in the
            // agent pane and merge again.
            if let Err(abort) = coding_git::abort_merge(dir) {
                return Err(anyhow::anyhow!("{error} (and the merge could not be aborted: {abort})"));
            }
            return Err(error);
        }
        Ok(())
    }

    /// The agent's branch proposal, when it normalizes to something git can
    /// use.
    fn proposed_branch(view: &RunView) -> Option<String> {
        let proposed = view.run.branch.as_deref()?;
        let normalized = normalize_branch_name(proposed);
        (!normalized.is_empty()).then_some(normalized)
    }

    /// The branch name used when the agent proposed nothing usable:
    /// `feature/<run-id>-<title-slug>`.
    async fn default_branch_name(
        view: &RunView,
        task_id: u64,
        store: &mut TodoStore,
    ) -> anyhow::Result<String> {
        let title = store.get_task(task_id).await?.title;
        let slug = normalize_branch_name(&title).to_lowercase();
        let slug = slug.rsplit('/').next().unwrap_or_default().to_string();
        let slug = if slug.is_empty() { "task".to_string() } else { slug };
        Ok(format!("feature/{}-{slug}", view.run.id))
    }

    /// The directories the run may work in: the first ancestor of `task_id`
    /// (itself included) that carries a project tag with usable directories.
    /// A multi-directory project yields all of them, in settings order
    /// (decision 21: one worktree per repo involved in the run).
    async fn project_dirs(
        store: &mut TodoStore,
        task_id: u64,
    ) -> anyhow::Result<Vec<std::path::PathBuf>> {
        let mut current = Some(task_id);
        // Bounded so a corrupt parent chain cannot loop forever.
        for _ in 0..32 {
            let Some(id) = current else {
                return Ok(Vec::new());
            };
            let task = store.get_task(id).await?;
            let direct_tags = store.get_direct_task_tags(id).await?;
            for tag in &direct_tags {
                let dirs = store.tag_settings(tag.id).await?.dirs;
                if dirs.is_empty() {
                    // Legacy shape: the directory encoded in a `project:` name,
                    // used only while the tag has no directory settings.
                    if let Some(path) = tag.name.strip_prefix("project:") {
                        let path = std::path::PathBuf::from(path);
                        if path.is_dir() {
                            return Ok(vec![path]);
                        }
                    }
                    continue;
                }
                // Directory settings win over the name: they are the tag's
                // real (and editable) list of working directories.
                let existing: Vec<std::path::PathBuf> = dirs
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .filter(|dir| dir.is_dir())
                    .collect();
                if !existing.is_empty() {
                    return Ok(existing);
                }
            }
            current = task.parent_id;
        }
        Ok(Vec::new())
    }

    /// The directory the run's git commands run in: the first usable one.
    /// `None` means the user has to configure one.
    async fn project_dir(
        store: &mut TodoStore,
        task_id: u64,
    ) -> anyhow::Result<Option<std::path::PathBuf>> {
        Ok(Self::project_dirs(store, task_id).await?.into_iter().next())
    }

    /// The directories of the project that are git repositories, which is what
    /// a run gets worktrees in.
    async fn project_repo_dirs(
        store: &mut TodoStore,
        task_id: u64,
    ) -> anyhow::Result<Vec<std::path::PathBuf>> {
        let dirs = Self::project_dirs(store, task_id).await?;
        tokio::task::spawn_blocking(move || {
            dirs.into_iter().filter(|dir| coding_git::is_repo(dir)).collect()
        })
        .await
        .map_err(Into::into)
    }

    /// Section display order plus task-id → section map for a tag view.
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

    /// ——— GitHub sync —————————————————————————————————————
    /// Bind every project tag whose directory resolves a github.com remote
    /// and has no target yet, persisting the detection so it is stable and
    /// overridable (spec decision 5). Returns how many tags were bound.
    pub fn bind_detected_github_repos(
        &self,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<usize>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let ids: Vec<u64> = s
                .list_integrations()
                .await?
                .into_iter()
                .filter(|integration| integration.provider == "github")
                .map(|integration| integration.id)
                .collect();
            let mut bound = 0;
            for id in ids {
                bound += Self::bind_detected_repos(&mut s, id).await?;
            }
            Ok(bound)
        })
    }

    /// One sync pass over every bound repo. `full` ignores the incremental
    /// cursor, which is how the manual "Sync now" also notices deletions; the
    /// poller stays incremental.
    pub fn sync_github(
        &self,
        full: bool,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<storage::GithubSyncSummary>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let credentials = crate::github_auth::stored_credentials()
                .ok_or_else(|| anyhow::anyhow!("Connect GitHub first"))?;
            let mut s = store.lock().await;
            let ids: Vec<u64> = s
                .list_integrations()
                .await?
                .into_iter()
                .filter(|integration| integration.provider == "github")
                .map(|integration| integration.id)
                .collect();
            let client = storage::GithubHttpClient::new(credentials.token.clone());
            let mut total = storage::GithubSyncSummary::default();
            for id in ids {
                Self::bind_detected_repos(&mut s, id).await?;
                let summary = Self::sync_github_with_retry(&mut s, &client, id, full).await?;
                total.absorb(&summary);
            }
            Ok(total)
        })
    }

    /// The remote object a tag syncs with, shown by the tag settings panel
    /// beside the directory picker (spec §5.3).
    pub fn tag_sync_target(
        &self,
        tag_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<storage::SyncTarget>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.tag_settings(tag_id).await?.sync_target)
        })
    }

    /// The GitHub issue a task came from, for the source badge and the
    /// read-only metadata chips. `None` for purely local tasks.
    pub fn github_issue_for_task(
        &self,
        task_id: u64,
        cx: &impl AppContext,
    ) -> Task<anyhow::Result<Option<storage::TaskIssue>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            Ok(s.issue_link_for_task(task_id).await?)
        })
    }

    /// Whether anything is waiting on GitHub, which is what makes the poller
    /// demand-driven: an open pull request or a live coding run keeps it on
    /// the short interval, and an idle app backs off (decision 23).
    pub fn github_work_pending(&self, cx: &impl AppContext) -> Task<anyhow::Result<bool>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            if !s.open_run_pull_requests().await?.is_empty() {
                return Ok(true);
            }
            Ok(!s.list_active_run_views().await?.is_empty())
        })
    }

    /// Bind tags whose directories resolve a GitHub remote but carry no sync
    /// target yet. Remote resolution shells out to git, so it stays off the
    /// async runtime.
    async fn bind_detected_repos(
        store: &mut TodoStore,
        integration_id: u64,
    ) -> anyhow::Result<usize> {
        let mut bound = 0;
        for tag in store.list_tags().await? {
            let settings = store.tag_settings(tag.id).await?;
            if settings.sync_target.is_some() || settings.dirs.is_empty() {
                continue;
            }
            let dirs = settings.dirs.clone();
            let detected = tokio::task::spawn_blocking(move || {
                dirs.iter()
                    .map(std::path::PathBuf::from)
                    .filter(|dir| dir.is_dir())
                    .find_map(|dir| coding_git::resolve_remote(&dir))
            })
            .await?;
            let Some(remote) = detected else {
                continue;
            };
            store
                .bind_repo_tag(tag.id, integration_id, &remote.owner, &remote.repo)
                .await?;
            bound += 1;
        }
        Ok(bound)
    }

    /// Auto-retry transient failures (network, 5xx, rate limit) with backoff
    /// and surface permanent ones straight away (decision 22).
    async fn sync_github_with_retry(
        store: &mut TodoStore,
        client: &storage::GithubHttpClient,
        integration_id: u64,
        full: bool,
    ) -> anyhow::Result<storage::GithubSyncSummary> {
        let mut attempt = 0;
        loop {
            match store
                .sync_github_integration(client, integration_id, full)
                .await
            {
                Ok(summary) => return Ok(summary),
                Err(error) => {
                    let failure = error.downcast_ref::<storage::SyncFailure>();
                    let transient = failure.is_some_and(storage::SyncFailure::is_transient);
                    attempt += 1;
                    if !transient || attempt >= GITHUB_SYNC_ATTEMPTS {
                        return Err(error);
                    }
                    let wait = failure
                        .and_then(storage::SyncFailure::retry_after)
                        .unwrap_or_else(|| std::time::Duration::from_secs(1 << attempt));
                    tokio::time::sleep(wait.min(GITHUB_SYNC_MAX_BACKOFF)).await;
                }
            }
        }
    }
}

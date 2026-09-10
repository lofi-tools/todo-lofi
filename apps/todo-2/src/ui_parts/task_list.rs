use gpui::{
    AppContext, AsyncApp, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div, rgb,
};
use gpui_component::StyledExt;
use gpui_component::input::*;
use storage::TaskWithMeta;
use storage::task::TaskCreate;

use super::navbar::{NavBar, NavBarEvent};
use super::task_row::{TaskRow, TaskRowEvent};
use crate::store::Store;

#[derive(Clone)]
pub enum TaskListEvent {
    Selected(TaskWithMeta),
    Deselected,
    TitleCommitted { task_id: u64, title: String },
}

/// The display shape of one task-list row: the top-level task plus what it
/// blocks. Blocked tasks that are exclusively blocked by one visible
/// blocker do not get their own row — they render inside that blocker's
/// row instead (inline chain for a single one, a "blocks N" chip with an
/// expandable list when there are several).
pub struct RowSpec {
    pub task: TaskWithMeta,
    /// The single-task inline chain (arrow + grayed titles), when the task
    /// blocks exactly one other task.
    pub blocked: Vec<super::task_row::ChainNode>,
    /// Every task this task exclusively blocks, used for the "blocks N"
    /// chip and its expandable list.
    pub blocks: Vec<TaskWithMeta>,
    /// The task's direct subtasks, collapsed under the row: the first one
    /// renders inline right of the title, the rest behind the expandable
    /// "N/M" counter.
    pub subtasks: Vec<TaskWithMeta>,
}

pub struct TaskListView {
    task_views: Vec<Entity<TaskRow>>,
    /// The display shape of each visible row (top-level task + what it
    /// blocks), kept so done-toggles can update blocked flags locally
    /// before the delayed re-sort refetches.
    row_specs: Vec<RowSpec>,
    /// task_id -> its blockers (with meta). Used to decide which tasks are
    /// hidden into a blocker's chain and to unblock dependants locally.
    blockers_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    /// task_id -> tasks it blocks (with meta).
    blocking_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    /// task_id -> its direct subtasks (with meta), collapsed under the
    /// parent's row (inline first title + expandable "N/M" list).
    subtasks_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    /// The task whose subtask list is expanded, or None. Only one row's
    /// subtasks can be expanded at a time.
    expanded_subtask: Option<u64>,
    input: Entity<InputState>,
    store: Store,
    selected_path: Vec<String>,
    selected_labels: Vec<String>,
    selected: Option<TaskWithMeta>,
    /// Previously selected tasks, oldest first. The forward stack only ever
    /// grows via `go_back`, so "next" is meaningless until "prev" is used.
    back: Vec<TaskWithMeta>,
    forward: Vec<TaskWithMeta>,
    editing: bool,
    input_needs_clear: bool,
    /// While a completed task is jumping to the bottom of the list, clicks
    /// are disabled: from shortly before the jump until just after it.
    locked_until: Option<std::time::Instant>,
    _fetch_tasks: Option<gpui::Task<()>>,
    _input_subscription: Subscription,
    _nav_subscription: Subscription,
}

impl TaskListView {
    pub fn new(
        input: Entity<InputState>,
        store: Store,
        nav_bar: Entity<NavBar>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_clone = input.clone();
        let input_subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                if title.is_empty() {
                    return;
                }
                this.insert_task(title, cx);
            }
        });

        let nav_subscription =
            cx.subscribe(&nav_bar, move |this, _nav_bar, event, cx| match event {
                NavBarEvent::TagSelected(path) => {
                    this.selected_path = path.clone();
                    this.refresh(cx);
                }
                NavBarEvent::AllTasks => {
                    this.selected_path.clear();
                    this.selected_labels.clear();
                    this.refresh(cx);
                }
                NavBarEvent::OpenProjectPicker
                | NavBarEvent::OpenIntegrations
                | NavBarEvent::OpenSettings => {
                    // Picker/dialog open is handled by the Layout; the task
                    // list is unaffected.
                }
            });

        Self {
            task_views: Vec::new(),
            row_specs: Vec::new(),
            blockers_map: std::collections::HashMap::new(),
            blocking_map: std::collections::HashMap::new(),
            subtasks_map: std::collections::HashMap::new(),
            expanded_subtask: None,
            input,
            store,
            selected_path: Vec::new(),
            selected_labels: Vec::new(),
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            editing: false,
            input_needs_clear: false,
            locked_until: None,
            _fetch_tasks: None,
            _input_subscription: input_subscription,
            _nav_subscription: nav_subscription,
        }
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| row.set_selected(false, cx));
        }
    }

    /// Restore the row highlight to a task without emitting selection
    /// events (used when a pending details-panel navigation is cancelled).
    pub fn restore_selection(&mut self, selected: Option<TaskWithMeta>, cx: &mut Context<Self>) {
        let selected_id = selected.as_ref().map(|task| task.id);
        self.selected = selected;
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                row.set_selected(Some(row.task_id()) == selected_id, cx)
            });
        }
    }

    /// Select a task, recording history when `record` is set. All
    /// selection paths (row clicks, blocker navigation, history travel)
    /// funnel through here so the details panel stays in sync via the
    /// emitted event.
    fn select(&mut self, task: TaskWithMeta, record: bool, cx: &mut Context<Self>) {
        if record {
            if let Some(current) = self.selected.clone() {
                if current.id != task.id {
                    self.back.push(current);
                    self.forward.clear();
                }
            }
        }
        let selected_id = task.id;
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                if row.task_id() != selected_id {
                    row.cancel_edit(cx);
                }
                row.set_selected(row.task_id() == selected_id, cx);
            });
        }
        self.selected = Some(task.clone());
        self.editing = self
            .task_views
            .iter()
            .any(|row| row.read(cx).is_editing());
        cx.emit(TaskListEvent::Selected(task));
    }

    /// Select a task by id, fetching it when it is not in the current list
    /// (e.g. a blocker from another tag's view).
    pub fn select_task_by_id(&mut self, task_id: u64, cx: &mut Context<Self>) {
        if self.selected.as_ref().is_some_and(|task| task.id == task_id) {
            return;
        }
        if let Some(task) = self.task_views.iter().find_map(|row| {
            let data = row.read(cx).task_data();
            (data.id == task_id).then_some(data)
        }) {
            self.select(task, true, cx);
            return;
        }
        let fetch = self.store.get_task_meta(task_id, cx);
        cx.spawn(async move |this, cx| {
            match fetch.await {
                Ok(task) => {
                    this.update(cx, |this, cx| this.select(task, true, cx))
                        .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to fetch task {task_id}: {e}");
                }
            }
        })
        .detach();
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        let current = self.selected.clone();
        if let Some(previous) = self.back.pop() {
            if let Some(current) = current {
                self.forward.push(current);
            }
            self.select(previous, false, cx);
        }
    }

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        let current = self.selected.clone();
        if let Some(next) = self.forward.pop() {
            if let Some(current) = current {
                self.back.push(current);
            }
            self.select(next, false, cx);
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| row.cancel_edit(cx));
        }
        self.editing = false;
    }

    /// A task's done state changed. Completed tasks gray out immediately,
    /// stay in place for a moment, then the list re-sorts them to the
    /// bottom; reopened tasks jump back up right away. Tasks that were
    /// blocked by the toggled task flip their blocked flag right away.
    pub fn on_task_done_toggled(&mut self, task_id: u64, done: bool, cx: &mut Context<Self>) {
        // Update the local blocker copies so blocked flags can be
        // recomputed without a DB round-trip.
        for (_, blockers) in self.blockers_map.iter_mut() {
            for blocker in blockers.iter_mut() {
                if blocker.id == task_id {
                    blocker.task.done = done;
                }
            }
        }
        // Dependants of `task_id` are blocked iff any of their blockers is
        // still open.
        let mut changed: Vec<(u64, bool)> = Vec::new();
        if let Some(dependants) = self
            .blocking_map
            .get(&task_id)
            .cloned()
        {
            for dependant in dependants {
                let still_blocked = self
                    .blockers_map
                    .get(&dependant.id)
                    .is_some_and(|blockers| blockers.iter().any(|b| !b.done));
                changed.push((dependant.id, still_blocked));
            }
        }

        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                if row.task_id() == task_id {
                    row.set_done(done, cx);
                }
                // Keep the parent's N/M counter fresh when a subtask's
                // done state flips from the details panel.
                row.set_subtask_done(task_id, done, cx);
                for (id, blocked) in &changed {
                    row.set_chain_blocked(*id, *blocked, cx);
                }
            });
        }
        let delay = if done {
            // 10s in place, then jump to the bottom.
            std::time::Duration::from_secs(10)
        } else {
            std::time::Duration::ZERO
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                // Disable clicks from shortly before the jump until just
                // after it, so no click lands mid-animation.
                this.locked_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1300));
                for row in this.task_views.clone() {
                    row.update(cx, |row, cx| row.set_locked(true, cx));
                }
                this.refresh(cx);
            })
            .ok();
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1300))
                .await;
            this.update(cx, |this, cx| {
                this.locked_until = None;
                for row in this.task_views.clone() {
                    row.update(cx, |row, cx| row.set_locked(false, cx));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn is_locked(&self) -> bool {
        self.locked_until
            .is_some_and(|t| t > std::time::Instant::now())
    }

    pub fn set_task_title(&mut self, task_id: u64, title: String, cx: &mut Context<Self>) {
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                if row.task_id() == task_id {
                    row.set_title(title.clone(), cx);
                }
            });
        }
    }

    /// Refresh a row from DB-reloaded task data (blocked flag, tags, ...).
    /// Rows for tasks outside the current view are ignored.
    pub fn refresh_task_data(&mut self, task: &TaskWithMeta, cx: &mut Context<Self>) {
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                if row.task_id() == task.id {
                    row.set_task_data(task.clone(), cx);
                }
            });
        }
    }

    /// Build the blockers/blocking maps and subtasks map for `tasks`
    /// (used by the row computation below).
    async fn fetch_list_data(
        store: &Store,
        tasks: &[TaskWithMeta],
        cx: &mut AsyncApp,
    ) -> (
        std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    ) {
        let ids: Vec<u64> = tasks.iter().map(|t| t.id).collect();
        let blockers = store.blockers_map(ids.clone(), cx).await.unwrap_or_default();
        let blocking = store.blocking_map(ids.clone(), cx).await.unwrap_or_default();
        let subtasks = store.subtasks_map(ids, cx).await.unwrap_or_default();
        (blockers, blocking, subtasks)
    }

    /// Reload the current view (all tasks or the selected tag's tasks) from
    /// the DB, e.g. after a subtask was created from the details panel.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let selected_path = self.selected_path.clone();
        let selected_labels = self.selected_labels.clone();
        let fetch = if let Some(last) = selected_path.last().cloned() {
            let path = selected_path.clone();
            cx.spawn(async move |this, cx| {
                let (tasks, labels) =
                    match store.list_tasks_by_tag_name_with_labels(&last, &path, cx).await {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!("Failed to fetch tasks by tag: {e}");
                            return;
                        }
                    };
                let (blockers_map, blocking_map, subtasks) =
                    Self::fetch_list_data(&store, &tasks, cx).await;
                this.update(cx, |this, cx| {
                    this.selected_labels = labels;
                    this.set_tasks_with_path(
                        tasks,
                        &path,
                        &this.selected_labels.clone(),
                        blockers_map,
                        blocking_map,
                        subtasks,
                        cx,
                    );
                    this._fetch_tasks = None;
                    cx.notify();
                })
                .ok();
            })
        } else {
            cx.spawn(async move |this, cx| {
                let tasks = {
                    let mut s = store.0.lock().await;
                    s.list_tasks_by_priority().await.unwrap_or_default()
                };
                let (blockers_map, blocking_map, subtasks) =
                    Self::fetch_list_data(&store, &tasks, cx).await;
                this.update(cx, |this, cx| {
                    this.set_tasks_with_path(
                        tasks,
                        &selected_path,
                        &selected_labels,
                        blockers_map,
                        blocking_map,
                        subtasks,
                        cx,
                    );
                    this._fetch_tasks = None;
                    cx.notify();
                })
                .ok();
            })
        };
        self._fetch_tasks = Some(fetch);
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        // When a tag is selected the new task belongs to it, so it is
        // tagged and the view stays on that tag's task list.
        let tag_name = self.selected_path.last().cloned();
        let store = self.store.clone();
        let create_task = store.insert_task(TaskCreate::default().title(title), tag_name, cx);

        self._fetch_tasks = Some(cx.spawn(async move |this, cx| {
            let (new_task_id, new_tasks) = match create_task.await {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("Failed to insert task: {e}");
                    return;
                }
            };
            let (blockers_map, blocking_map, subtasks) =
                Self::fetch_list_data(&store, &new_tasks, cx).await;
            // Select the fresh task so its details are one keypress/click
            // away without hunting for it in the list (works even when the
            // task nests inside another row's blocker chain).
            let created = new_tasks
                .iter()
                .find(|task| task.id == new_task_id)
                .cloned();
            this.update(cx, |this, cx| {
                this.set_tasks_with_path(
                    new_tasks,
                    &this.selected_path.clone(),
                    &this.selected_labels.clone(),
                    blockers_map,
                    blocking_map,
                    subtasks,
                    cx,
                );
                if let Some(task) = created {
                    this.select(task, true, cx);
                }
                this.input_needs_clear = true;
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_tasks_with_path(
        &mut self,
        tasks: Vec<TaskWithMeta>,
        selected_path: &[String],
        selected_labels: &[String],
        blockers_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        blocking_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        subtasks: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        cx: &mut Context<Self>,
    ) {
        self.editing = false;
        self.blockers_map = blockers_map;
        self.blocking_map = blocking_map;
        self.subtasks_map = subtasks;
        let row_specs = Self::compute_row_specs(
            &tasks,
            &self.blockers_map,
            &self.blocking_map,
            &self.subtasks_map,
        );
        self.row_specs = row_specs;
        let selected_task_id = self.selected.as_ref().map(|task| task.id);
        self.task_views = self
            .row_specs
            .iter()
            .map(|spec| {
                let is_selected = Some(spec.task.id) == selected_task_id;
                let subtasks_expanded = self.expanded_subtask == Some(spec.task.id);
                let row = cx.new(|cx| {
                    TaskRow::new(
                        spec.task.clone(),
                        super::task_row::RowBlocking {
                            blocked: spec.blocked.clone(),
                            blocks: spec.blocks.clone(),
                        },
                        spec.subtasks.clone(),
                        self.store.clone(),
                        selected_path.to_vec(),
                        selected_labels.to_vec(),
                        is_selected,
                        subtasks_expanded,
                        cx,
                    )
                });
                cx.subscribe(&row, |this, _row, event, cx| match event {
                    TaskRowEvent::Selected(task) => {
                        this.select(task.clone(), true, cx);
                    }
                    TaskRowEvent::EditStarted => {
                        this.editing = true;
                    }
                    TaskRowEvent::EditEnded => {
                        this.editing = false;
                    }
                    TaskRowEvent::TitleCommitted { task_id, title } => {
                        this.editing = false;
                        cx.emit(TaskListEvent::TitleCommitted {
                            task_id: *task_id,
                            title: title.clone(),
                        });
                    }
                    TaskRowEvent::DoneToggled { task_id, done } => {
                        this.on_task_done_toggled(*task_id, *done, cx);
                    }
                    TaskRowEvent::SubtasksToggled { task_id } => {
                        this.toggle_subtask_expansion(*task_id, cx);
                    }
                })
                .detach();
                row
            })
            .collect();
    }

    /// Toggle which row's subtask list is expanded. Only one row is
    /// expanded at a time: expanding another row collapses the previous
    /// one, and clicking the expanded row's counter collapses it.
    pub fn toggle_subtask_expansion(&mut self, task_id: u64, cx: &mut Context<Self>) {
        self.expanded_subtask = if self.expanded_subtask == Some(task_id) {
            None
        } else {
            Some(task_id)
        };
        let expanded = self.expanded_subtask;
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                row.set_subtasks_expanded(Some(row.task_id()) == expanded, cx);
            });
        }
    }

    pub fn set_tasks(&mut self, tasks: Vec<TaskWithMeta>, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let selected_path = self.selected_path.clone();
        let selected_labels = self.selected_labels.clone();
        self._fetch_tasks = Some(cx.spawn(async move |this, cx| {
            let (blockers_map, blocking_map, subtasks) =
                Self::fetch_list_data(&store, &tasks, cx).await;
            this.update(cx, |this, cx| {
                this.set_tasks_with_path(
                    tasks,
                    &selected_path,
                    &selected_labels,
                    blockers_map,
                    blocking_map,
                    subtasks,
                    cx,
                );
                this._fetch_tasks = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Compute the visible rows from the task list plus the blocker and
    /// subtask relationships: a task blocked by exactly one visible
    /// blocker, or whose parent is a visible task, does not get its own
    /// row — it nests inside that blocker's chain or the parent's row.
    pub fn compute_row_specs(
        tasks: &[TaskWithMeta],
        blockers_map: &std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        blocking_map: &std::collections::HashMap<u64, Vec<TaskWithMeta>>,
        subtasks_map: &std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    ) -> Vec<RowSpec> {
        let visible_ids: std::collections::HashSet<u64> =
            tasks.iter().map(|t| t.id).collect();
        // A task is hidden when its parent is a task in this view (its
        // subtask row collapses under the parent) or when exactly one of
        // its blockers is a task in this view (blocker chain collapse).
        let hidden: std::collections::HashSet<u64> = tasks
            .iter()
            .filter(|task| {
                let is_subtask = task
                    .parent_id
                    .is_some_and(|parent_id| visible_ids.contains(&parent_id));
                let is_exclusively_blocked = blockers_map
                    .get(&task.id)
                    .is_some_and(|blockers| {
                        blockers
                            .iter()
                            .filter(|b| visible_ids.contains(&b.id))
                            .count()
                            == 1
                    });
                is_subtask || is_exclusively_blocked
            })
            .map(|task| task.id)
            .collect();

        fn chain_of(
            task_id: u64,
            blocking_map: &std::collections::HashMap<u64, Vec<TaskWithMeta>>,
            hidden: &std::collections::HashSet<u64>,
            width_so_far: usize,
        ) -> Vec<super::task_row::ChainNode> {
            // Elide chains that would render too wide: stop nesting once
            // the combined titles would exceed roughly half the panel.
            const MAX_WIDTH: usize = 60;
            let Some(blocked) = blocking_map.get(&task_id) else {
                return Vec::new();
            };
            let exclusive: Vec<&TaskWithMeta> =
                blocked.iter().filter(|t| hidden.contains(&t.id)).collect();
            if exclusive.len() != 1 {
                return Vec::new();
            }
            let next = exclusive[0];
            let next_width = width_so_far + next.title.len();
            if next_width > MAX_WIDTH {
                return Vec::new();
            }
            let mut node = super::task_row::ChainNode {
                task: next.clone(),
                nested: Vec::new(),
            };
            node.nested = chain_of(next.id, blocking_map, hidden, next_width);
            vec![node]
        }

        tasks
            .iter()
            .filter(|task| !hidden.contains(&task.id))
            .map(|task| {
                let exclusively_blocked: Vec<TaskWithMeta> = blocking_map
                    .get(&task.id)
                    .map(|blocked| {
                        blocked
                            .iter()
                            .filter(|t| hidden.contains(&t.id))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let chain = if exclusively_blocked.len() == 1 {
                    chain_of(task.id, blocking_map, &hidden, task.title.len())
                } else {
                    Vec::new()
                };
                RowSpec {
                    task: task.clone(),
                    blocked: chain,
                    blocks: exclusively_blocked,
                    subtasks: subtasks_map.get(&task.id).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::task::Task;

    fn meta(id: u64, title: &str) -> TaskWithMeta {
        TaskWithMeta {
            task: Task {
                id,
                title: title.to_string(),
                description: None,
                branch_name: None,
                labels: None,
                deadline: None,
                blocked_until: None,
                importance_factor: 1.0,
                urgency_factor: 1.0,
                done: false,
                completed_at: None,
                created_at: jiff::Timestamp::now(),
                updated_at: jiff::Timestamp::now(),
                parent_id: None,
                source_task_id: None,
                deleted_at: None,
                timezone: None,
                comments: None,
                subtasks: storage::prelude::Deferred::default(),
                parent: storage::prelude::Deferred::default(),
            },
            direct_tags: Vec::new(),
            inherited_tags: Vec::new(),
            inferred_tags: Vec::new(),
            leaf_tags: Vec::new(),
            blocked: false,
        }
    }

    #[test]
    fn test_compute_row_specs_collapses_exclusive_blocker() {
        let a = meta(1, "a");
        let b = meta(2, "b");
        let c = meta(3, "c");
        let tasks = vec![a.clone(), b.clone(), c.clone()];

        let mut blockers_map = std::collections::HashMap::new();
        let mut blocking_map = std::collections::HashMap::new();
        // a blocks b and c.
        blockers_map.insert(b.id, vec![a.clone()]);
        blockers_map.insert(c.id, vec![a.clone()]);
        blocking_map.insert(a.id, vec![b.clone(), c.clone()]);

        let subtasks_map = std::collections::HashMap::new();
        let specs =
            TaskListView::compute_row_specs(&tasks, &blockers_map, &blocking_map, &subtasks_map);
        // Only a is a visible row; b and c are collapsed under it.
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].task.id, 1);
        // Two exclusive dependants: no inline chain, but a blocks-N list.
        assert!(specs[0].blocked.is_empty());
        assert_eq!(specs[0].blocks.len(), 2);
    }

    #[test]
    fn test_compute_row_specs_hides_subtasks_under_visible_parent() {
        let parent = meta(1, "parent");
        let child = meta(2, "child");
        let mut child_with_parent = child.clone();
        child_with_parent.task.parent_id = Some(parent.id);
        // A subtask whose parent is not in the view stays a visible row.
        let orphan = meta(3, "orphan subtask");
        let tasks = vec![parent.clone(), child_with_parent.clone(), orphan.clone()];

        let blockers_map = std::collections::HashMap::new();
        let blocking_map = std::collections::HashMap::new();
        let mut subtasks_map = std::collections::HashMap::new();
        subtasks_map.insert(parent.id, vec![child_with_parent.clone()]);

        let specs = TaskListView::compute_row_specs(
            &tasks,
            &blockers_map,
            &blocking_map,
            &subtasks_map,
        );
        assert_eq!(specs.len(), 2);
        // The child has no row of its own and nests under the parent.
        assert!(specs.iter().all(|spec| spec.task.id != child.id));
        let parent_spec = specs.iter().find(|spec| spec.task.id == parent.id).unwrap();
        assert_eq!(parent_spec.subtasks.len(), 1);
        assert_eq!(parent_spec.subtasks[0].id, child.id);
        // The orphan (parent not in view) keeps its own row.
        assert!(specs.iter().any(|spec| spec.task.id == orphan.id));
    }

    #[tokio::test]
    async fn test_seed_data_rows_nest_write_migration_tests_under_crud() -> anyhow::Result<()> {
        // Drive the real seed data through the row computation, so the
        // nesting matches what the app displays.
        let mut store = storage::TodoStore::new(&storage::StorageConfig {
            db_uri: "turso::memory:".to_string(),
        })
        .await?;
        store.seed().await?;

        let tasks = store.list_tasks_by_priority().await?;
        let ids: Vec<u64> = tasks.iter().map(|t| t.id).collect();
        let blockers_map = store.blockers_map(&ids).await?;
        let blocking_map = store.blocking_map(&ids).await?;
        let subtasks_map = store.subtasks_map(&ids).await?;
        let specs =
            TaskListView::compute_row_specs(&tasks, &blockers_map, &blocking_map, &subtasks_map);

        // "Write migration tests" is blocked by exactly one visible task
        // ("Implement task CRUD"), so it is hidden from its own row and
        // nested after the CRUD row's inline arrow.
        let crud = specs
            .iter()
            .find(|spec| spec.task.title == "Implement task CRUD")
            .expect("CRUD task should be a visible row");
        assert!(
            specs.iter().all(|spec| spec.task.title != "Write migration tests"),
            "Write migration tests must not have its own row"
        );
        assert_eq!(crud.blocks.len(), 2, "CRUD blocks two tasks -> blocks-2 chip");

        // "Migrate database schema" is a subtask of CRUD: no own row, and
        // it nests under CRUD as its (only) collapsed subtask.
        assert!(
            specs
                .iter()
                .all(|spec| spec.task.title != "Migrate database schema"),
            "subtask must not have its own row"
        );
        assert_eq!(crud.subtasks.len(), 1);
        assert_eq!(crud.subtasks[0].title, "Migrate database schema");

        // "Code review PRs" blocks exactly one task ("Deploy to
        // production"), so the deploy task nests in its inline chain.
        let review = specs
            .iter()
            .find(|spec| spec.task.title == "Code review PRs")
            .expect("review task should be a visible row");
        assert_eq!(review.blocked.len(), 1);
        assert_eq!(review.blocked[0].task.title, "Deploy to production");
        // Single dependant: no blocks-N chip (needs more than one).
        assert_eq!(review.blocks.len(), 1);
        assert!(
            specs.iter().all(|spec| spec.task.title != "Deploy to production"),
            "Deploy to production must not have its own row"
        );

        Ok(())
    }

    #[test]
    fn test_compute_row_specs_inline_chain() {
        let a = meta(1, "a");
        let b = meta(2, "b");
        let tasks = vec![a.clone(), b.clone()];

        let mut blockers_map = std::collections::HashMap::new();
        let mut blocking_map = std::collections::HashMap::new();
        // a blocks b exactly.
        blockers_map.insert(b.id, vec![a.clone()]);
        blocking_map.insert(a.id, vec![b.clone()]);

        let subtasks_map = std::collections::HashMap::new();
        let specs =
            TaskListView::compute_row_specs(&tasks, &blockers_map, &blocking_map, &subtasks_map);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].blocked.len(), 1);
        assert_eq!(specs[0].blocked[0].task.id, 2);
        // Single dependant: it is in `blocks` too, but the blocks-N chip
        // only renders when there is more than one.
        assert_eq!(specs[0].blocks.len(), 1);
    }
}

impl EventEmitter<TaskListEvent> for TaskListView {}

impl Render for TaskListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.input_needs_clear {
            self.input_needs_clear = false;
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        let heading = self
            .selected_path
            .last()
            .cloned()
            .unwrap_or_else(|| "Tasks".to_string());

        div()
            .id("task-list")
            .flex_1()
            .v_flex()
            .p_8()
            .gap_4()
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.is_locked() {
                    cx.emit(TaskListEvent::Deselected);
                }
            }))
            .child(
                div()
                    .text_2xl()
                    .font_bold()
                    .text_color(rgb(0xe5e5e5))
                    .child(heading),
            )
            .child(Input::new(&self.input))
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .gap_2()
                    .children(self.task_views.iter().cloned()),
            )
    }
}

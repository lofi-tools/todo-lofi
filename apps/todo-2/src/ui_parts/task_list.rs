use gpui::{
    AnyElement, AppContext, AsyncApp, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div, list,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::scroll::Scrollbar;
use gpui_component::{IconName, Sizable, Size};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::*;
use storage::TaskWithMeta;
use storage::task::TaskCreate;

use super::navbar::{NavBar, NavBarEvent, NavDestination};
use super::task_row::{RowBlocking, TaskRow, TaskRowEvent};
use crate::store::Store;
use crate::theme::HAIRLINE;
use std::rc::Rc;

#[derive(Clone)]
pub enum TaskListEvent {
    Selected(TaskWithMeta),
    Deselected,
    TitleCommitted { task_id: u64, title: String },
    /// The empty-list action button was clicked (managed tags).
    EmptyActionRequested,
    /// The gear beside the tag title was clicked: open tag settings for the
    /// selected tag (the name is the tag's unique name, not its label).
    OpenTagSettings(String),
}

/// The display shape of one task-list row: the top-level task plus what it
/// blocks. Blocked tasks that are exclusively blocked by one visible
/// blocker do not get their own row — they render inside that blocker's
/// row instead (inline chain for a single one, a "blocks N" chip with an
/// expandable list when there are several).
#[derive(Clone)]
pub struct RowSpec {
    pub task: TaskWithMeta,
    /// The single-task inline chain ("then" + grayed titles), when the
    /// task blocks exactly one other task.
    pub blocked: Vec<super::task_row::ChainNode>,
    /// Every task this task exclusively blocks, used for the "blocks N"
    /// chip and its expandable list.
    pub blocks: Vec<TaskWithMeta>,
    /// The task's direct subtasks, collapsed under the row: the first one
    /// renders inline right of the title, the rest behind the expandable
    /// "N/M" counter.
    pub subtasks: Vec<TaskWithMeta>,
}

/// What one row shows about the tasks its task blocks: the inline chain and
/// the "blocks N" chip.
fn row_blocking(spec: &RowSpec) -> RowBlocking {
    RowBlocking {
        blocked: spec.blocked.clone(),
        blocks: spec.blocks.clone(),
    }
}

/// An inline insert input open in an interstitial gap: the input plus
/// the neighbouring row task ids that position the new task.
struct PendingInsert {
    above_id: Option<u64>,
    below_id: Option<u64>,
    input: Entity<InputState>,
    _subscription: Subscription,
}

pub struct TaskListView {
    task_views: Vec<Entity<TaskRow>>,
    /// The display shape of each visible row (top-level task + what it
    /// blocks), kept so done-toggles can update blocked flags locally
    /// before the delayed re-sort refetches.
    row_specs: Vec<RowSpec>,
    /// The view's tasks in list order, kept so a done-toggle can re-derive
    /// the visible rows without a database round-trip. A just-ticked task
    /// keeps its old done state here on purpose: its row sits in place
    /// until the completed task's delayed jump, so a re-derivation must not
    /// move it to the completed group early.
    tasks: Vec<TaskWithMeta>,
    /// task_id -> its blockers (with meta). Used to decide which tasks are
    /// hidden into a blocker's chain and to unblock dependants locally.
    blockers_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    /// task_id -> tasks it blocks (with meta).
    blocking_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    /// task_id -> its direct subtasks (with meta), collapsed under the
    /// parent's row (inline first title + expandable "N/M" list).
    subtasks_map: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    /// Section display order for the selected tag (Todoist-style headers),
    /// plus task-id → section name. Empty in All-tasks view and for tags
    /// without sectioned tasks: rows render flat.
    section_order: Vec<String>,
    task_section: std::collections::HashMap<u64, String>,
    /// Reveal tasks starting more than 2 days out (hidden by default so
    /// far-future occurrences don't flood the list).
    show_all: bool,
    /// The task whose subtask list is expanded, or None. Only one row's
    /// subtasks can be expanded at a time.
    expanded_subtask: Option<u64>,
    /// The list body's items in display order (section headers, insert
    /// gaps, task rows), shared with the list element's render closure.
    entries: Rc<Vec<ListEntry>>,
    /// Which item each row sits at, so a row that changed its own height
    /// can be measured again by index.
    entry_index: std::collections::HashMap<u64, usize>,
    /// Rows that reported a height change since the last frame. Collected
    /// here because the report arrives per row, and the list rebuilds its
    /// height index on every remeasure: one tick can touch several rows.
    measure_rows: std::collections::HashSet<u64>,
    /// gpui's virtualized list state (the zed pattern): it caches every
    /// item's measured height and lays out and paints only the items on
    /// screen, with some overdraw on either side. Scrolling therefore
    /// costs the same for ten tasks and for ten thousand. It lives on the
    /// view because the state has to outlive the element using it.
    list_state: ListState,
    /// When the list is empty, show this labeled button where the rows
    /// would be (managed tags e.g. the travel checklists panel). Clicking
    /// it emits `EmptyActionRequested`.
    empty_action_label: Option<String>,
    /// Optional action at the end of the title row: the travel panel,
    /// whose "+ New trip" button renders there. `None` in the plain
    /// tag/project view, which has no title-row action.
    travel_panel: Option<Entity<super::travel::TravelPanel>>,
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
    /// An inline insert input open in an interstitial gap, if any: the ids
    /// of the rows above/below the gap (`None` at the list edges).
    inserting: Option<PendingInsert>,
    /// While a completed task is jumping to the bottom of the list, clicks
    /// are disabled: from shortly before the jump until just after it.
    locked_until: Option<std::time::Instant>,
    _fetch_tasks: Option<gpui::Task<()>>,
    _fetch_sections: Option<gpui::Task<()>>,
    /// Periodic time-based re-sort: priority scores decay as deadlines
    /// approach, so the list re-fetches in score order every 60s.
    /// Cancelled automatically when the view drops.
    _reorder_timer: gpui::Task<()>,
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
                NavBarEvent::Navigated(destination) => match destination {
                    NavDestination::Tag(path) => {
                        if this.selected_path == *path {
                            return;
                        }
                        this.selected_path = path.clone();
                        this.refresh(cx);
                    }
                    NavDestination::AllTasks => {
                        if this.selected_path.is_empty() {
                            return;
                        }
                        this.selected_path.clear();
                        this.selected_labels.clear();
                        this.refresh(cx);
                    }
                    // A menu panel replaces the list on screen; its own
                    // selection is kept for the navigation back to it.
                    NavDestination::Integrations
                    | NavDestination::Automations
                    | NavDestination::Settings => {}
                },
                NavBarEvent::OpenProjectPicker | NavBarEvent::OpenTagSettings(_) => {
                    // Picker/popover open is handled by the Layout; the task
                    // list is unaffected.
                }
            });

        let timer_store = store.clone();
        Self {
            task_views: Vec::new(),
            row_specs: Vec::new(),
            tasks: Vec::new(),
            blockers_map: std::collections::HashMap::new(),
            blocking_map: std::collections::HashMap::new(),
            subtasks_map: std::collections::HashMap::new(),
            section_order: Vec::new(),
            task_section: std::collections::HashMap::new(),
            show_all: false,
            _fetch_sections: None,
            expanded_subtask: None,
            entries: Rc::new(Vec::new()),
            entry_index: std::collections::HashMap::new(),
            measure_rows: std::collections::HashSet::new(),
            // The overdraw keeps a screenful of rows measured beyond each
            // edge, so fast wheel scrolling never runs into unmeasured
            // (and therefore unrendered) rows.
            list_state: ListState::new(0, ListAlignment::Top, px(400.)),
            empty_action_label: None,
            travel_panel: None,
            input,
            store,
            selected_path: Vec::new(),
            selected_labels: Vec::new(),
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            editing: false,
            input_needs_clear: false,
            inserting: None,
            locked_until: None,
            _fetch_tasks: None,
            _reorder_timer: {
                cx.spawn(async move |this, cx| {
                    // Startup already materialized; the timer covers
                    // date rollovers while the app stays open.
                    let mut last_day = today_key();
                    loop {
                        cx.background_executor()
                            .timer(std::time::Duration::from_secs(60))
                            .await;
                        if today_key() != last_day {
                            last_day = today_key();
                            let materialize = timer_store.materialize_due_occurrences(cx);
                            if let Err(e) = materialize.await {
                                tracing::error!("Daily materialize failed: {e}");
                            }
                        }
                        let shifted = this
                            .update(cx, |this, cx| {
                                // Never yank rows mid-edit or mid-animation;
                                // skip this cycle instead.
                                if this.editing || this.is_locked() {
                                    return false;
                                }
                                this.set_locked(true, cx);
                                this.refresh(cx);
                                true
                            })
                            .unwrap_or(false);
                        if !shifted {
                            // View dropped: end the loop. Otherwise this was
                            // a skipped cycle; wait out the next minute.
                            if this.upgrade().is_none() {
                                break;
                            }
                            continue;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(1300))
                            .await;
                        if this
                            .update(cx, |this, cx| this.set_locked(false, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
            },
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
        self.editing || self.inserting.is_some()
    }

    pub fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| row.cancel_edit(cx));
        }
        self.editing = false;
        if self.inserting.take().is_some() {
            cx.notify();
        }
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
        // The dependants' flags on the list's own copies of the tasks, and
        // on the "what I block" copies that feed a row's chain and blocks
        // chip: re-deriving the rows below reads those instead of the
        // database. The toggled task keeps its old done state here — its
        // row is told directly below, and the layout must not move it to
        // the completed group until the delayed reload.
        for task in self.tasks.iter_mut() {
            if let Some((_, blocked)) = changed.iter().find(|(id, _)| *id == task.id) {
                task.blocked = *blocked;
            }
        }
        for blocked_task in self.blocking_map.values_mut().flat_map(|map| map.iter_mut()) {
            if let Some((_, blocked)) = changed.iter().find(|(id, _)| *id == blocked_task.id) {
                blocked_task.blocked = *blocked;
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
                    // A dependant with its own row stops being grayed out
                    // (and its checkbox becomes tickable) right away.
                    if row.task_id() == *id {
                        row.set_blocked(*blocked, cx);
                    }
                }
            });
        }
        // A tick can free an exclusively blocked dependant, which then gets
        // its own row (and its checkbox) right away rather than only once
        // the completed task's delayed reload lands.
        self.sync_visible_rows(cx);
        let delay = if done {
            // 10s in place, then jump to the bottom.
            std::time::Duration::from_secs(10)
        } else {
            std::time::Duration::ZERO
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                this.set_locked(true, cx);
                this.refresh(cx);
            })
            .ok();
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1300))
                .await;
            this.update(cx, |this, cx| {
                this.set_locked(false, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn is_locked(&self) -> bool {
        self.locked_until
            .is_some_and(|t| t > std::time::Instant::now())
    }

    /// Disable (or re-enable) clicks on the task list from shortly before
    /// a row shift until just after it, so no click lands mid-animation.
    /// Shared by the done-toggle jump and the periodic time-based re-sort.
    fn set_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        self.locked_until = locked.then(|| {
            std::time::Instant::now() + std::time::Duration::from_millis(1300)
        });
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| row.set_locked(locked, cx));
        }
        cx.notify();
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

    /// Fetch a tag's tasks plus display labels, tolerating a tag that is
    /// being created concurrently: enabling an automation then immediately
    /// opening its tag can lose the spawn-order race to the enable
    /// transaction, so the first lookup runs before the tag row commits.
    /// When the tag is missing, wait a beat and resolve once more before
    /// giving up; any other error fails immediately.
    async fn fetch_tagged_with_retry(
        store: &Store,
        tag_name: &str,
        path: &[String],
        cx: &mut AsyncApp,
    ) -> anyhow::Result<(Vec<TaskWithMeta>, Vec<String>)> {
        match store
            .list_tasks_by_tag_name_with_labels_including_distant(tag_name, path, cx)
            .await
        {
            Ok(result) => Ok(result),
            Err(first) => {
                let missing = store
                    .get_tag_by_name(tag_name.to_string(), cx)
                    .await
                    .ok()
                    .flatten()
                    .is_none();
                if !missing {
                    return Err(first);
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(350))
                    .await;
                store
                    .list_tasks_by_tag_name_with_labels_including_distant(tag_name, path, cx)
                    .await
            }
        }
    }

    /// Toggle revealing far-future tasks (for editing them early).
    fn toggle_show_all(&mut self, cx: &mut Context<Self>) {
        self.show_all = !self.show_all;
        cx.notify();
    }

    /// Load section grouping for the selected tag (Todoist-style headers).
    /// No-op in the All-tasks view. Runs after the rows are set so every
    /// visible task id is known.
    fn load_sections(&mut self, cx: &mut Context<Self>) {
        let Some(tag_name) = self.selected_path.last().cloned() else {
            self.section_order.clear();
            self.task_section.clear();
            return;
        };
        let task_ids: Vec<u64> = self.row_specs.iter().map(|spec| spec.task.id).collect();
        if task_ids.is_empty() {
            self.section_order.clear();
            self.task_section.clear();
            return;
        }
        let fetch = self.store.task_section_groups(tag_name, task_ids, cx);
        self._fetch_sections = Some(cx.spawn(async move |this, cx| {
            match fetch.await {
                Ok((order, map)) => {
                    this.update(cx, |this, cx| {
                        this.section_order = order;
                        this.task_section = map;
                        this._fetch_sections = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to load sections: {e}");
                }
            }
        }));
    }

    /// Reload the current view (all tasks or the selected tag's tasks) from
    /// the DB, e.g. after a subtask was created from the details panel.
    /// Set the empty-list action label (e.g. "+ New trip" for managed
    /// tags); `None` restores the plain empty body.
    pub fn set_empty_action(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        self.empty_action_label = label;
        cx.notify();
    }

    /// Set the title-row travel panel (its "+ New trip" button renders
    /// at the end of the title row); `None` restores the plain title.
    pub fn set_travel_panel(
        &mut self,
        panel: Option<Entity<super::travel::TravelPanel>>,
        cx: &mut Context<Self>,
    ) {
        self.travel_panel = panel;
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let selected_path = self.selected_path.clone();
        let selected_labels = self.selected_labels.clone();
        // Drop stale section headers while the new list loads.
        self.section_order.clear();
        self.task_section.clear();
        let fetch = if let Some(last) = selected_path.last().cloned() {
            let path = selected_path.clone();
            cx.spawn(async move |this, cx| {
                let (tasks, labels) =
                    match Self::fetch_tagged_with_retry(&store, &last, &path, cx).await {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!("Failed to fetch tasks by tag: {e}");
                            return;
                        }
                    };
                let (blockers_map, blocking_map, subtasks) =
                    Self::fetch_list_data(&store, &tasks, cx).await;
                this.update(cx, |this, cx| {
                    // Drop the result if the user navigated elsewhere while
                    // the (possibly retried) fetch was in flight.
                    if this.selected_path != path {
                        return;
                    }
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
                    this.load_sections(cx);
                    this._fetch_tasks = None;
                    cx.notify();
                })
                .ok();
            })
        } else {
            cx.spawn(async move |this, cx| {
                let tasks = {
                    let mut s = store.0.lock().await;
                    s.list_tasks_by_priority_including_distant()
                        .await
                        .unwrap_or_default()
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
        self.insert_task_with_factors(title, 1.0, 1.0, cx);
    }

    /// Open an inline input in the gap between two rows (`None` at the
    /// list edges). Enter commits it via `commit_insert`; Esc cancels it
    /// via `cancel_editing`.
    pub fn begin_insert(
        &mut self,
        above_id: Option<u64>,
        below_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("New task", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                this.commit_insert(cx);
            }
        });
        self.inserting = Some(PendingInsert {
            above_id,
            below_id,
            input: input.clone(),
            _subscription: subscription,
        });
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Commit the inline gap input: insert the titled task with factors
    /// placing it between the gap's neighbours, like `insert_task`.
    fn commit_insert(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.inserting.take() else {
            return;
        };
        let title = pending.input.read(cx).text().to_string();
        let title = title.trim().to_string();
        cx.notify();
        if title.is_empty() {
            return;
        }
        let spec = |id: Option<u64>| {
            id.and_then(|id| self.row_specs.iter().find(|spec| spec.task.id == id))
        };
        let (importance, urgency) = storage::factors_between(
            spec(pending.above_id).map(|spec| &spec.task.task),
            spec(pending.below_id).map(|spec| &spec.task.task),
        );
        self.insert_task_with_factors(title, importance, urgency, cx);
    }

    fn insert_task_with_factors(
        &mut self,
        title: String,
        importance: f64,
        urgency: f64,
        cx: &mut Context<Self>,
    ) {
        // When a tag is selected the new task belongs to it, so it is
        // tagged and the view stays on that tag's task list.
        let tag_name = self.selected_path.last().cloned();
        let store = self.store.clone();
        let create_task = store.insert_task(
            TaskCreate::default()
                .title(title)
                .importance_factor(importance)
                .urgency_factor(urgency),
            tag_name,
            cx,
        );

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
                this.load_sections(cx);
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
        // A reload reorders rows, invalidating any open gap position.
        self.inserting = None;
        self.blockers_map = blockers_map;
        self.blocking_map = blocking_map;
        self.subtasks_map = subtasks;
        self.tasks = tasks;
        let row_specs = Self::compute_row_specs(
            &self.tasks,
            &self.blockers_map,
            &self.blocking_map,
            &self.subtasks_map,
        );
        self.row_specs = row_specs;
        let views: Vec<Entity<TaskRow>> = self
            .row_specs
            .iter()
            .map(|spec| self.build_row(spec, selected_path, selected_labels, cx))
            .collect();
        self.task_views = views;
    }

    /// Build the row (and subscribe to its events) for one spec. Rows are
    /// kept across a local re-derivation of the list, so this is only
    /// called for a spec that has no row yet.
    fn build_row(
        &self,
        spec: &RowSpec,
        selected_path: &[String],
        selected_labels: &[String],
        cx: &mut Context<Self>,
    ) -> Entity<TaskRow> {
        let is_selected = Some(spec.task.id) == self.selected.as_ref().map(|task| task.id);
        let subtasks_expanded = self.expanded_subtask == Some(spec.task.id);
        let row = cx.new(|cx| {
            TaskRow::new(
                spec.task.clone(),
                row_blocking(spec),
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
            TaskRowEvent::LayoutChanged { task_id } => {
                this.measure_rows.insert(*task_id);
                cx.notify();
            }
        })
        .detach();
        row
    }

    /// Re-measure the rows that changed their own content height (an
    /// expanded subtask or blocks list, inline editing, reloaded metadata).
    /// The list caches measured heights, so those rows' cached heights are
    /// dropped and the frame that paints the new content measures them
    /// again. One call for all of them: the list rebuilds its height index
    /// per call, and a single tick can move several rows at once.
    fn flush_row_measurements(&mut self) {
        if self.measure_rows.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.measure_rows);
        let mut first = usize::MAX;
        let mut last = 0;
        for task_id in pending {
            if let Some(index) = self.entry_index.get(&task_id).copied() {
                first = first.min(index);
                last = last.max(index + 1);
            }
        }
        if first < last {
            self.list_state.remeasure_items(first..last);
        }
    }

    /// Re-derive the visible rows from the tasks already in hand, keeping
    /// the rows that stay put (an open editor, an expanded list) and only
    /// building the ones that just appeared. A done-toggle that frees an
    /// exclusively blocked dependant calls this so the next task in a chain
    /// shows its row — and its checkbox — at once, instead of only once the
    /// completed task's delayed jump to the bottom reloads the list.
    fn sync_visible_rows(&mut self, cx: &mut Context<Self>) {
        let row_specs = Self::compute_row_specs(
            &self.tasks,
            &self.blockers_map,
            &self.blocking_map,
            &self.subtasks_map,
        );
        let same_rows = row_specs
            .iter()
            .map(|spec| spec.task.id)
            .eq(self.row_specs.iter().map(|spec| spec.task.id));
        if same_rows {
            return;
        }
        self.row_specs = row_specs;
        let mut views: Vec<Entity<TaskRow>> = Vec::with_capacity(self.row_specs.len());
        for spec in &self.row_specs {
            let existing = self
                .task_views
                .iter()
                .find(|row| row.read(cx).task_id() == spec.task.id)
                .cloned();
            match existing {
                // A row that survives the change keeps its state, but sheds
                // the tasks that no longer nest in it.
                Some(row) => {
                    row.update(cx, |row, cx| row.set_blocking(row_blocking(spec), cx));
                    views.push(row);
                }
                None => {
                    let row = self.build_row(spec, &self.selected_path, &self.selected_labels, cx);
                    views.push(row);
                }
            }
        }
        self.task_views = views;
        cx.notify();
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
    /// subtask relationships: a task blocked by exactly one visible *open*
    /// blocker, or whose parent is a visible task, does not get its own
    /// row — it nests inside that blocker's chain or the parent's row.
    /// Tasks whose only visible blockers are done get their own full row
    /// (e.g. after the 10s tick animation, the unlocked task replaces the
    /// completed one instead of staying nested in it).
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
        // its *open* blockers is a task in this view (blocker chain
        // collapse). Done blockers never collapse: the unlocked task
        // stands on its own.
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
                            .filter(|b| !b.done && visible_ids.contains(&b.id))
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
                workflow_run_id: None,
                node_id: None,
                spec: None,
                spec_path: None,
                role: None,
                spec_covered_at: None,
                subtasks: storage::prelude::Deferred::default(),
                parent: storage::prelude::Deferred::default(),
            },
            direct_tags: Vec::new(),
            inherited_tags: Vec::new(),
            inferred_tags: Vec::new(),
            leaf_tags: Vec::new(),
            blocked: false,
            managed_by: None,
            managed_label: None,
            managed_mode: None,
            managed_editable: false,
            user_modified: false,
        }
    }

    fn header(top: &str) -> ListEntry {
        ListEntry::Header {
            top: top.to_string(),
            sub: None,
            divided: false,
            distant: None,
            show_all: false,
        }
    }

    fn gap(above: u64, below: u64) -> ListEntry {
        ListEntry::Gap {
            above: Some(above),
            below: Some(below),
            input: None,
        }
    }

    /// The body draws the items the viewport reaches and takes its geometry
    /// from the list state, instead of eagerly laying out the whole run: a
    /// hundred items draw and scroll without panicking, and the scroll
    /// position is the list's own item offset.
    #[gpui::test]
    fn test_virtualized_body_draws_and_scrolls(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        struct ListBody {
            entries: Rc<Vec<ListEntry>>,
            list_state: ListState,
        }
        impl Render for ListBody {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().w(px(400.)).h(px(200.)).child(
                    ScrollableTaskList::new(
                        self.entries.clone(),
                        self.list_state.clone(),
                        |entry, _window, _cx| match entry {
                            ListEntry::Header { top, .. } => {
                                div().child(top.clone()).into_any_element()
                            }
                            ListEntry::Gap { above, .. } => div()
                                .id(format!("gap-{}", above.unwrap_or(0)))
                                .h(px(16.))
                                .into_any_element(),
                            ListEntry::Row { task_id, .. } => div()
                                .h(px(52.))
                                .child(format!("row {task_id}"))
                                .into_any_element(),
                            ListEntry::BottomPad => div().h(px(24.)).into_any_element(),
                        },
                    )
                    .render_body(),
                )
            }
        }
        // Alternating headers and insert gaps: two item kinds, more of them
        // than the 200px body can show.
        let entries: Rc<Vec<ListEntry>> = Rc::new(
            (0..100)
                .map(|index| {
                    if index % 2 == 0 {
                        header(&format!("group {index}"))
                    } else {
                        gap(index, index + 1)
                    }
                })
                .collect(),
        );
        let list_state = ListState::new(entries.len(), ListAlignment::Top, px(200.));
        let (_view, cx) = cx.add_window_view(|_, _| ListBody {
            entries: entries.clone(),
            list_state: list_state.clone(),
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(list_state.item_count(), entries.len());
        assert_eq!(list_state.logical_scroll_top().item_ix, 0);
        list_state.scroll_by(px(600.));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(list_state.logical_scroll_top().item_ix > 0);
    }

    #[test]
    fn test_unchanged_prefix_only_remeasures_the_changed_tail() {
        let old = vec![
            header("Pack"),
            gap(1, 2),
            gap(2, 3),
            header("Completed"),
        ];
        // A reload that reorders the tail re-measures from the first
        // difference on; the items above it keep their cached heights.
        let new = vec![
            header("Pack"),
            gap(1, 2),
            gap(2, 4),
            header("Completed"),
        ];
        assert_eq!(unchanged_prefix(&old, &new), 2);
        // An identical run re-measures nothing at all.
        assert_eq!(unchanged_prefix(&old, &old.clone()), old.len());
        // A shorter list stops at its own end.
        assert_eq!(unchanged_prefix(&old, &old[..2]), 2);
        assert_eq!(unchanged_prefix(&[], &old), 0);
    }

    #[test]
    fn test_sectioned_order_groups() {
        let section_of: std::collections::HashMap<usize, String> =
            [(1, "B".to_string()), (2, "A".to_string()), (4, "B".to_string())]
                .into_iter()
                .collect();
        let order = vec!["A".to_string(), "B".to_string()];
        let indices: Vec<usize> = (0..5).collect();
        let chunks = sectioned_order(&indices, &section_of, &order);
        assert_eq!(chunks.len(), 3);
        assert!(matches!(&chunks[0], RowChunk::Rows(rows) if rows == &[0, 3]));
        assert!(matches!(&chunks[1], RowChunk::Section(name, rows) if name == "A" && rows == &[2]));
        assert!(matches!(&chunks[2], RowChunk::Section(name, rows) if name == "B" && rows == &[1, 4]));
    }

    #[test]
    fn test_split_subsection_one_level_only() {
        assert_eq!(split_subsection("Pack"), ("Pack", None));
        assert_eq!(
            split_subsection("Pack / food"),
            ("Pack", Some("food".to_string()))
        );
        assert_eq!(
            split_subsection("Pack/food"),
            ("Pack", Some("food".to_string()))
        );
        // A second slash never nests deeper: the remainder stays whole.
        assert_eq!(
            split_subsection("Pack / a / b"),
            ("Pack", Some("a / b".to_string()))
        );
        assert_eq!(split_subsection("Pack /"), ("Pack /", None));
    }

    #[test]
    fn test_sectioned_order_nests_subsections() {
        let section_of: std::collections::HashMap<usize, String> = [
            (0, "Pack".to_string()),
            (1, "Pack / stay".to_string()),
            (2, "Before leaving".to_string()),
            (3, "Pack / food".to_string()),
        ]
        .into_iter()
        .collect();
        let order = vec![
            "Pack".to_string(),
            "Pack / stay".to_string(),
            "Pack / food".to_string(),
            "Before leaving".to_string(),
        ];
        let indices: Vec<usize> = (0..4).collect();
        let chunks = sectioned_order(&indices, &section_of, &order);
        assert_eq!(chunks.len(), 4);
        assert!(matches!(&chunks[0], RowChunk::Section(name, rows) if name == "Pack" && rows == &[0]));
        assert!(matches!(&chunks[1], RowChunk::Section(name, rows) if name == "Pack / stay" && rows == &[1]));
        assert!(matches!(&chunks[2], RowChunk::Section(name, rows) if name == "Pack / food" && rows == &[3]));
        assert!(matches!(&chunks[3], RowChunk::Section(name, rows) if name == "Before leaving" && rows == &[2]));
    }

    #[test]
    fn test_sectioned_order_flat_without_sections() {
        let indices: Vec<usize> = (0..2).collect();
        let chunks = sectioned_order(&indices, &std::collections::HashMap::new(), &[]);
        assert!(matches!(&chunks[..], [RowChunk::Rows(rows)] if rows == &[0, 1]));
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
    fn test_compute_row_specs_done_blocker_frees_dependant() {
        let mut a = meta(1, "blocker");
        a.task.done = true;
        let b = meta(2, "unlocked");
        let tasks = vec![a.clone(), b.clone()];

        let mut blockers_map = std::collections::HashMap::new();
        let mut blocking_map = std::collections::HashMap::new();
        blockers_map.insert(b.id, vec![a.clone()]);
        blocking_map.insert(a.id, vec![b.clone()]);

        let subtasks_map = std::collections::HashMap::new();
        let specs =
            TaskListView::compute_row_specs(&tasks, &blockers_map, &blocking_map, &subtasks_map);
        // Both stand on their own: the done blocker keeps a plain row with
        // no chain, and the unlocked task gets a full row (own checkbox).
        assert_eq!(specs.len(), 2);
        let done_spec = specs.iter().find(|spec| spec.task.id == a.id).unwrap();
        assert!(done_spec.blocked.is_empty());
        assert!(done_spec.blocks.is_empty());
        let free_spec = specs.iter().find(|spec| spec.task.id == b.id).unwrap();
        assert!(!free_spec.task.blocked);
    }

    #[test]
    fn test_done_toggle_hands_the_chain_over_to_the_next_task() {
        let a = meta(1, "a");
        let mut b = meta(2, "b");
        b.blocked = true;
        let tasks = vec![a.clone(), b.clone()];

        let mut blockers_map = std::collections::HashMap::new();
        let mut blocking_map = std::collections::HashMap::new();
        // a blocks b exactly: b collapses into a's chain.
        blockers_map.insert(b.id, vec![a.clone()]);
        blocking_map.insert(a.id, vec![b.clone()]);

        let subtasks_map = std::collections::HashMap::new();
        let specs =
            TaskListView::compute_row_specs(&tasks, &blockers_map, &blocking_map, &subtasks_map);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].blocked[0].task.id, b.id);

        // Ticking `a` patches the local copies the way `on_task_done_toggled`
        // does before re-deriving the rows: `b` gets its own row right after
        // `a` with its blocked flag cleared, and `a` hands the chain over.
        blockers_map.get_mut(&b.id).unwrap()[0].task.done = true;
        let mut tasks = tasks;
        tasks[1].blocked = false;
        let specs =
            TaskListView::compute_row_specs(&tasks, &blockers_map, &blocking_map, &subtasks_map);
        let ids: Vec<u64> = specs.iter().map(|spec| spec.task.id).collect();
        assert_eq!(ids, vec![a.id, b.id]);
        assert!(specs[0].blocked.is_empty());
        assert!(specs[0].blocks.is_empty());
        assert!(!specs[1].task.blocked);
        // The ticked task still reads pending in the layout: its row greys
        // out in place, and only the delayed reload moves it to the bottom.
        assert!(!specs[0].task.done);
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

/// Split a section name into its top section and optional sub-section.
/// Only one level is supported: the split happens at the first slash,
/// so "Pack / food" becomes ("Pack", Some("food")) while "Pack" becomes
/// ("Pack", None). A sub name containing another slash is kept whole —
/// it never nests deeper.
pub fn split_subsection(name: &str) -> (&str, Option<String>) {
    match name.split_once('/') {
        Some((top, sub)) => {
            let sub = sub.trim().to_string();
            if sub.is_empty() {
                (name.trim(), None)
            } else {
                (top.trim(), Some(sub))
            }
        }
        None => (name.trim(), None),
    }
}

/// One item of the task-list body. The body is a flat run of these —
/// section headers, the insert gaps between rows, and the rows themselves
/// — handed to gpui's `list`, which measures each item once and only lays
/// out and paints the ones on screen. Items are cheap to clone and to
/// compare, so the view can build a fresh run every frame and have the
/// list re-measure only the part that actually changed.
#[derive(Clone, PartialEq)]
pub enum ListEntry {
    /// A section header ("Upcoming", "Completed", a tag's section, ...).
    Header {
        /// The top section name.
        top: String,
        /// One level of sub-section, rendered grayed out as "Top / sub".
        sub: Option<String>,
        /// Divider above the header, separating the trailing groups.
        divided: bool,
        /// How many far-future tasks the "show all" toggle would reveal;
        /// `None` when there is nothing to reveal and so no toggle.
        distant: Option<usize>,
        /// Whether those tasks are currently revealed (the toggle's label).
        show_all: bool,
    },
    /// The insert strip between two rows (`None` at the list's edges): a
    /// hover-revealed + on a line, or the inline insert input when this gap
    /// is being filled.
    Gap {
        above: Option<u64>,
        below: Option<u64>,
        input: Option<Entity<InputState>>,
    },
    /// A task row, rendering its own view.
    Row {
        task_id: u64,
        view: Entity<TaskRow>,
    },
    /// Bottom breathing room inside the scroll area: in-flow, so it only
    /// appears when scrolled to the very bottom and never covers a task.
    BottomPad,
}

/// Height hinted for items the list has not measured yet: about what a row
/// plus its insert gap averages, so the scrollable range and the scrollbar
/// thumb are in the right ballpark from the first frame instead of growing
/// as the user scrolls. Measuring only ever replaces the hint with the real
/// height, and erring high only overshoots the thumb, never the reachable
/// end of the list.
const ITEM_HEIGHT_HINT: f32 = 40.0;

/// Reusable virtualized task-list body: the flat item run, the list state
/// the view owns (it has to outlive the element), and how to render one
/// item. Used by the main tag/project view and by the travel checklist view
/// (the same `TaskListView` instance under the travel header).
pub struct ScrollableTaskList {
    entries: Rc<Vec<ListEntry>>,
    list_state: ListState,
    render_item: Box<dyn Fn(&ListEntry, &mut Window, &mut gpui::App) -> AnyElement + 'static>,
}

impl ScrollableTaskList {
    pub fn new(
        entries: Rc<Vec<ListEntry>>,
        list_state: ListState,
        render_item: impl Fn(&ListEntry, &mut Window, &mut gpui::App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            entries,
            list_state,
            render_item: Box::new(render_item),
        }
    }

    pub fn render_body(self) -> AnyElement {
        let Self {
            entries,
            list_state,
            render_item,
        } = self;
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                list(list_state.clone(), move |index, window, cx| {
                    match entries.get(index) {
                        Some(entry) => render_item(entry, window, cx),
                        None => div().into_any_element(),
                    }
                })
                .size_full(),
            )
            // The list paints no scrollbar of its own; this one drives the
            // same state the wheel does, so dragging and the wheel agree.
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&list_state)
                        .id("task-list-scrollbar")
                        .viewport_from_layout(),
                ),
            )
            .into_any_element()
    }
}

/// How many items at the front of the list `old` run are identical to the
/// front of the new one. Everything from there on has to be measured again;
/// before it, cached heights (and the scroll anchor's item index) hold.
fn unchanged_prefix(old: &[ListEntry], new: &[ListEntry]) -> usize {
    old.iter()
        .zip(new.iter())
        .take_while(|(old, new)| old == new)
        .count()
}

/// Render one item of the task-list body. Headers and gaps are rebuilt from
/// their data on demand (they carry live event handlers, so they cannot be
/// shared between frames); rows render their own view.
fn list_entry(entry: &ListEntry, view: &WeakEntity<TaskListView>) -> AnyElement {
    match entry {
        ListEntry::Header {
            top,
            sub,
            divided,
            distant,
            show_all,
        } => list_header(top, sub.as_deref(), *divided, *distant, *show_all, view).into_any_element(),
        ListEntry::Gap {
            above,
            below,
            input,
        } => list_gap(*above, *below, input.clone(), view).into_any_element(),
        // The row spans the full width, like it did as a flex child; the
        // negative vertical margin cancels the gap strip's own height so
        // rows pack tight and only the padding shows.
        ListEntry::Row { view: row, .. } => div()
            .w_full()
            .my(px(-4.))
            .child(row.clone())
            .into_any_element(),
        // Plain breathing room: no hover affordance, no input.
        ListEntry::BottomPad => div().w_full().h(px(24.)).into_any_element(),
    }
}

/// One section header, with the "show all" toggle at its end when
/// far-future tasks are hidden.
fn list_header(
    top: &str,
    sub: Option<&str>,
    divided: bool,
    distant: Option<usize>,
    show_all: bool,
    view: &WeakEntity<TaskListView>,
) -> impl IntoElement {
    let mut header = if divided {
        div()
            // Full-width so the divider above spans the list and the
            // toggle sits at the far right (list items shrink-wrap by
            // default, which used to clip both to the title's width).
            .w_full()
            // Upcoming breathes more above the divider, less below it so
            // its rows sit close under the label.
            .when(top == "Upcoming", |this| this.mt(px(96.)).mb(px(-8.)))
            .when(top != "Upcoming", |this| this.mt_4())
            .pt_2()
            .border_t_1()
            .border_color(rgb(0x333333))
            .h_flex()
            .items_center()
            .justify_between()
    } else {
        div()
            .w_full()
            .h_flex()
            .items_center()
            .justify_between()
            .mt_2()
    };
    // A sub-section renders as "Top / sub" with the slash and sub name
    // grayed out next to the top section.
    let title = match sub {
        Some(sub) => div()
            .h_flex()
            .items_baseline()
            .gap_1()
            .text_sm()
            .font_semibold()
            .child(div().text_color(rgb(0xa3a3a3)).child(top.to_string()))
            .child(div().text_color(rgb(0x525252)).child(format!("/ {sub}")))
            .into_any_element(),
        None => div()
            .text_sm()
            .font_semibold()
            .text_color(rgb(0xa3a3a3))
            .child(top.to_string())
            .into_any_element(),
    };
    header = header.child(title);
    if let Some(distant) = distant {
        let view = view.clone();
        header = header.child(
            Button::new("show-all-tasks")
                .ghost()
                .compact()
                // Sized and toned like the "Upcoming" header beside it, so
                // the toggle reads as part of that label rather than a call
                // to action. "Later" names what it reveals: tasks starting
                // more than 2 days out, which stay hidden otherwise.
                .with_size(Size::Small)
                .text_color(rgb(0xa3a3a3))
                .label(if show_all {
                    "Show less".to_string()
                } else {
                    format!("Show later ({distant})")
                })
                .tooltip(if show_all {
                    "Only show tasks starting within the next 2 days"
                } else {
                    "Also show tasks starting more than 2 days from now"
                })
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.toggle_show_all(cx)).ok();
                }),
        );
    }
    header
}

/// One interstitial insert row: a hover-revealed + on a horizontal line,
/// or the inline insert input when this gap is being filled. The strip is
/// explicitly zero-height with visibly overflowing content, so adjacent
/// row headers touch each other: the inner content keeps its 24px hitbox
/// (centered on the boundary by its negative margin) and carries the
/// hover reveal, since the zero-height strip itself is not hoverable.
fn list_gap(
    above: Option<u64>,
    below: Option<u64>,
    input: Option<Entity<InputState>>,
    view: &WeakEntity<TaskListView>,
) -> impl IntoElement {
    let key = format!(
        "insert-gap-{}-{}",
        above.unwrap_or(0),
        below.unwrap_or(0)
    );
    if let Some(input) = input {
        // Align with row titles/tags: row px_3 (12) + checkbox (22) +
        // title gap_3 (12) = 46px; right side matches the row px_3.
        return div()
            .id(key)
            .py_2()
            .pl(px(46.))
            .pr_3()
            .child(Input::new(&input).small().focus_bordered(false))
            .into_any_element();
    }
    let view = view.clone();
    div()
        .id(key.clone())
        .w_full()
        .h(px(0.))
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .my_neg_3()
                .opacity(0.0)
                .hover(|style| style.opacity(1.0))
                .child(
                    div()
                        .id(format!("{key}-plus"))
                        .h_6()
                        .w_6()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_lg()
                        .text_color(rgb(0xa3a3a3))
                        .cursor_pointer()
                        .child("+")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            view.update(cx, |this, cx| {
                                this.begin_insert(above, below, window, cx)
                            })
                            .ok();
                        }),
                )
                .child(div().h_px().flex_1().bg(rgb(0x333333))),
        )
        .into_any_element()
}

impl Render for TaskListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.input_needs_clear {
            self.input_needs_clear = false;
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        // Prefer the fetched display label (e.g. a managed tag's
        // "Travel checklists") over the raw tag name.
        let heading = self
            .selected_labels
            .last()
            .cloned()
            .or_else(|| self.selected_path.last().cloned())
            .unwrap_or_else(|| "Tasks".to_string());
        // The gear opens tag settings by unique name; `All Tasks` has no tag.
        let selected_tag_name = self.selected_path.last().cloned();

        div()
            .id("task-list")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .v_flex()
            // No bottom padding: it shows the window background through and
            // merges with the footer into a gray bar. The trailing insert
            // gap already gives the scrolled-to-bottom content its air.
            .px_8()
            .pt_8()
            .pb_0()
            .gap_4()
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.is_locked() {
                    cx.emit(TaskListEvent::Deselected);
                }
            }))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_2xl()
                            .font_bold()
                            .text_color(rgb(0xe5e5e5))
                            .child(heading),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .when_some(
                                self.travel_panel.clone(),
                                |this, panel: Entity<super::travel::TravelPanel>| {
                                    this.child(panel.update(cx, |panel, cx| {
                                        panel.new_trip_button(cx)
                                    }))
                                },
                            )
                            // Tag settings live behind the gear: what the tag is
                            // placed under, its directories, sections, and apps.
                            .when_some(selected_tag_name, |this, tag_name| {
                                this.child(
                                    Button::new("tag-settings")
                                        .ghost()
                                        .compact()
                                        .icon(IconName::Settings)
                                        .tooltip("Tag settings")
                                        .on_click(cx.listener(move |_this, _, _, cx| {
                                            cx.emit(TaskListEvent::OpenTagSettings(
                                                tag_name.clone(),
                                            ));
                                        })),
                                )
                            }),
                    ),
            )
            // Shrink below the placeholder's intrinsic width when the right
            // pane squeezes this panel, instead of overflowing it.
            .child(Input::new(&self.input).min_w_0())
            .child(self.list_body(window, cx))
    }
}

impl TaskListView {
    /// The list body: the rows via the reusable virtualized task-list
    /// component, or the empty-list action button centered where the rows
    /// would be (managed tags only).
    fn list_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Hand this frame's items to the list first: it keeps the scroll
        // position and re-measures only the tail that changed. Then the
        // rows whose own content changed height, whose item indices are
        // fresh after that sync.
        self.sync_list_state(self.list_entries());
        self.flush_row_measurements();
        // The row data (tasks + dependents + subtasks) is the source of
        // truth for emptiness; the views mirror it one-to-one.
        let empty = self.row_specs.is_empty();
        let empty_action = self.empty_action_label.clone();
        if empty
            && let Some(label) = empty_action
        {
            return div()
                .flex_1()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa3a3a3))
                        .child("Nothing here yet"),
                )
                .child(
                    Button::new("empty-list-action")
                        .ghost()
                        .compact()
                        .with_size(gpui_component::Size::Small)
                        .border_1()
                        .border_color(rgb(HAIRLINE))
                        .text_color(rgb(0xa3a3a3))
                        .label(label)
                        .on_click(cx.listener(|_this, _, _, cx| {
                            cx.emit(TaskListEvent::EmptyActionRequested);
                        })),
                )
                .into_any_element();
        }
        let view = cx.entity().downgrade();
        ScrollableTaskList::new(
            self.entries.clone(),
            self.list_state.clone(),
            move |entry, _window, _cx| list_entry(entry, &view),
        )
        .render_body()
    }

    /// Hand the freshly built items to the virtualized list. Only the items
    /// from the first difference onwards are re-measured, so a reload keeps
    /// both the scroll position and every unchanged item's measured height
    /// (and, with it, the row views' own state).
    fn sync_list_state(&mut self, entries: Vec<ListEntry>) {
        let entries = Rc::new(entries);
        if *entries == *self.entries {
            return;
        }
        // Hold the reader's place: a reload reorders the rows around the
        // one at the top of the viewport (completed tasks move down, the
        // periodic re-sort by score, a task inserted above), and the splice
        // below resets the scroll to the first changed item. Following the
        // top row by task id keeps the viewport on it instead.
        let anchor = self.scroll_anchor();
        let unchanged = unchanged_prefix(&self.entries, &entries);
        self.list_state
            .splice(unchanged..self.entries.len(), entries.len() - unchanged);
        self.list_state = self
            .list_state
            .clone()
            .with_uniform_item_height(px(ITEM_HEIGHT_HINT));
        self.entry_index = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                ListEntry::Row { task_id, .. } => Some((*task_id, index)),
                _ => None,
            })
            .collect();
        self.entries = entries;
        if let Some((task_id, offset)) = anchor
            && let Some(index) = self.entries.iter().position(|entry| {
                matches!(entry, ListEntry::Row { task_id: id, .. } if *id == task_id)
            })
        {
            self.list_state.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: offset,
            });
        }
    }

    /// The row at the top of the viewport as a task id, plus how far into
    /// it the viewport starts. `None` when the top item is not a row (a
    /// header or an insert gap), where the splice's own adjustment is as
    /// good as any.
    fn scroll_anchor(&self) -> Option<(u64, gpui::Pixels)> {
        let scroll_top = self.list_state.logical_scroll_top();
        match self.entries.get(scroll_top.item_ix) {
            Some(ListEntry::Row { task_id, .. }) => Some((*task_id, scroll_top.offset_in_item)),
            _ => None,
        }
    }
    /// The list body's items: flat by default, grouped under section
    /// headers for sectioned tags (Todoist-style), with not-yet-doable
    /// tasks last under an "Upcoming" header and completed ones under
    /// "Completed". Tasks starting more than 2 days out only render when
    /// "show all" is on. Every row outside Upcoming is preceded by its
    /// insert gap, the normal section is closed by one trailing gap, and
    /// then the list is closed by one trailing gap (unless it ends inside
    /// Upcoming, which takes no inserts).
    fn list_entries(&self) -> Vec<ListEntry> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (done_pairs, rest_pairs): (Vec<_>, Vec<_>) = self
            .row_specs
            .iter()
            .enumerate()
            .partition(|(_, spec)| spec.task.done);
        let done: Vec<usize> = done_pairs.into_iter().map(|(i, _)| i).collect();
        let (upcoming_pairs, current_pairs): (Vec<_>, Vec<_>) = rest_pairs
            .into_iter()
            .partition(|(_, spec)| spec.task.blocked_until.is_some_and(|until| until > now_secs));
        let (near_upcoming, distant): (Vec<usize>, Vec<usize>) = upcoming_pairs
            .into_iter()
            .map(|(i, _)| i)
            .partition(|&i| {
                self.row_specs[i]
                    .task
                    .blocked_until
                    .is_some_and(|until| until <= now_secs + DISTANT_SECS)
            });
        let mut upcoming = near_upcoming;
        let distant_count = distant.len();
        if self.show_all {
            upcoming.extend(distant);
        }
        let current: Vec<usize> = current_pairs.into_iter().map(|(i, _)| i).collect();
        // No sections here: doable rows flat, then Upcoming, then Completed.
        if self.selected_path.is_empty() || self.section_order.is_empty() {
            let mut chunks = Vec::new();
            if !current.is_empty() {
                chunks.push(RowChunk::Rows(current));
            }
            if !upcoming.is_empty() {
                chunks.push(RowChunk::Upcoming(upcoming));
            }
            if !done.is_empty() {
                chunks.push(RowChunk::Completed(done));
            }
            return self.entries_for_chunks(&chunks, distant_count);
        }
        let section_of: std::collections::HashMap<usize, String> = current
            .iter()
            .filter_map(|&index| {
                self.task_section
                    .get(&self.row_specs[index].task.id)
                    .map(|name| (index, name.clone()))
            })
            .collect();
        let mut chunks = sectioned_order(&current, &section_of, &self.section_order);
        if !upcoming.is_empty() {
            chunks.push(RowChunk::Upcoming(upcoming));
        }
        if !done.is_empty() {
            chunks.push(RowChunk::Completed(done));
        }
        self.entries_for_chunks(&chunks, distant_count)
    }

    /// Turn row chunks into list items: the chunk's header (if it has one)
    /// followed by each of its rows behind an insert gap. Gaps are placed
    /// against the whole display order, so the gap above a section's first
    /// row still sits between it and the section header.
    ///
    /// The Upcoming chunk is a query, not a place: no insert gap is
    /// rendered above (or between) its rows — new tasks are always added
    /// in the normal section and move to Upcoming on their own once their
    /// properties make them not immediately doable. Instead the normal
    /// section gets one insert gap after its last row, ahead of the
    /// Upcoming header.
    fn entries_for_chunks(&self, chunks: &[RowChunk], distant: usize) -> Vec<ListEntry> {
        let order: Vec<u64> = chunks
            .iter()
            .flat_map(|chunk| {
                chunk
                    .indices()
                    .iter()
                    .map(|index| self.row_specs[*index].task.id)
            })
            .collect();
        let upcoming_ids: std::collections::HashSet<u64> = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                RowChunk::Upcoming(indices) => Some(indices),
                _ => None,
            })
            .flatten()
            .map(|index| self.row_specs[*index].task.id)
            .collect();
        let mut entries = Vec::new();
        let mut position = 0;
        for (chunk_ix, chunk) in chunks.iter().enumerate() {
            if let Some(header) = self.chunk_header(chunk, distant) {
                entries.push(header);
            }
            for &index in chunk.indices() {
                let task_id = self.row_specs[index].task.id;
                // No "+ row" inside Upcoming: adding happens in the normal
                // section, and the task moves here on its own.
                if !upcoming_ids.contains(&task_id) {
                    let above = (position > 0).then(|| order[position - 1]);
                    entries.push(ListEntry::Gap {
                        above,
                        below: Some(task_id),
                        input: self.inserting_input(above, Some(task_id)),
                    });
                }
                entries.push(ListEntry::Row {
                    task_id,
                    view: self.task_views[index].clone(),
                });
                position += 1;
            }
            // Bottom of the normal section: one insert gap after its last
            // row when a query chunk (Upcoming, Completed) follows, taking
            // the place of the suppressed gap above that chunk's first row.
            let main = matches!(
                chunk,
                RowChunk::Rows(_) | RowChunk::Section(..)
            );
            let next_main = chunks
                .get(chunk_ix + 1)
                .is_some_and(|next| matches!(next, RowChunk::Rows(_) | RowChunk::Section(..)));
            if main && !next_main && chunks.get(chunk_ix + 1).is_some() {
                let above = (position > 0).then(|| order[position - 1]);
                entries.push(ListEntry::Gap {
                    above,
                    below: None,
                    input: self.inserting_input(above, None),
                });
            }
        }
        // One trailing gap, so a task can still be added after the last row
        // — unless the list ends inside Upcoming, which takes no inserts.
        if let Some(last) = order.last().copied()
            && !upcoming_ids.contains(&last)
        {
            entries.push(ListEntry::Gap {
                above: Some(last),
                below: None,
                input: self.inserting_input(Some(last), None),
            });
        }
        // Bottom breathing room inside the scroll area: in-flow, so it
        // only shows when scrolled to the very bottom and never covers a
        // task the way an overlay would.
        if !order.is_empty() {
            entries.push(ListEntry::BottomPad);
        }
        entries
    }

    /// The header item of a chunk: a section name (top plus one sub level),
    /// "Upcoming" with its show-all toggle, or "Completed". A plain run of
    /// rows has no header.
    fn chunk_header(&self, chunk: &RowChunk, distant: usize) -> Option<ListEntry> {
        match chunk {
            RowChunk::Rows(_) => None,
            RowChunk::Section(name, _) => {
                let (top, sub) = split_subsection(name);
                Some(ListEntry::Header {
                    top: top.to_string(),
                    sub,
                    divided: false,
                    distant: None,
                    show_all: false,
                })
            }
            RowChunk::Upcoming(_) => Some(ListEntry::Header {
                top: "Upcoming".to_string(),
                sub: None,
                divided: true,
                // The toggle only shows when there is something to reveal.
                distant: (distant > 0).then_some(distant),
                show_all: self.show_all,
            }),
            RowChunk::Completed(_) => Some(ListEntry::Header {
                top: "Completed".to_string(),
                sub: None,
                divided: true,
                distant: None,
                show_all: false,
            }),
        }
    }

    /// The inline insert input open in this gap, if this is the gap it was
    /// opened in.
    fn inserting_input(&self, above: Option<u64>, below: Option<u64>) -> Option<Entity<InputState>> {
        self.inserting
            .as_ref()
            .filter(|pending| pending.above_id == above && pending.below_id == below)
            .map(|pending| pending.input.clone())
    }
}

/// System-local civil date key (`YYYY-MM-DD`) for detecting date
/// rollovers while the app stays open.
fn today_key() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::system())
        .date()
        .to_string()
}

/// Tasks starting later than this stay out of list queries unless the
/// "show all" toggle reveals them. Mirrors the SQL `+ 172800` cap.
const DISTANT_SECS: u64 = 2 * 86400;

/// Order row indices for sectioned display: unsectioned rows first (in
/// list order), then one group per section in display order, with
/// sub-sections nested directly under their top section (the top's own
/// rows first, then each sub in display order). Pure so it can be
/// unit-tested; `section_of` maps row index → section name, which may
/// carry one "Top / sub" level.
fn sectioned_order(
    indices: &[usize],
    section_of: &std::collections::HashMap<usize, String>,
    section_order: &[String],
) -> Vec<RowChunk> {
    let mut plain = Vec::new();
    let mut by_section: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for &index in indices {
        match section_of.get(&index) {
            Some(name) => by_section.entry(name.clone()).or_default().push(index),
            None => plain.push(index),
        }
    }
    // Display position of each full section name; unknown names sort
    // after the known ones.
    let position_of = |name: &str| {
        section_order
            .iter()
            .position(|ordered| ordered == name)
            .unwrap_or(usize::MAX)
    };
    // Group full section names by top section, keeping the top's own
    // rows (full name == top) first and sub-sections in display order.
    let mut by_top: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for name in by_section.keys() {
        let (top, _) = split_subsection(name);
        by_top.entry(top.to_string()).or_default().push(name.clone());
    }
    for names in by_top.values_mut() {
        names.sort_by_key(|name| {
            let (top, sub) = split_subsection(name);
            (sub.is_some(), position_of(name), name.clone(), top.to_string())
        });
    }
    let mut tops: Vec<String> = by_top.keys().cloned().collect();
    tops.sort_by_key(|top| {
        let first = by_top[top]
            .iter()
            .map(|name| position_of(name))
            .min()
            .unwrap_or(usize::MAX);
        (first, top.clone())
    });
    let mut chunks = Vec::new();
    if !plain.is_empty() {
        chunks.push(RowChunk::Rows(plain));
    }
    for top in tops {
        for name in by_top.remove(&top).unwrap_or_default() {
            if let Some(rows) = by_section.remove(&name) {
                chunks.push(RowChunk::Section(name, rows));
            }
        }
    }
    chunks
}

/// One flat run of rows, a section header group, or one of the trailing
/// divider-separated groups (Upcoming, Completed).
enum RowChunk {
    Rows(Vec<usize>),
    Section(String, Vec<usize>),
    Upcoming(Vec<usize>),
    Completed(Vec<usize>),
}

impl RowChunk {
    /// The row indices this chunk renders, in display order.
    fn indices(&self) -> &[usize] {
        match self {
            RowChunk::Rows(indices)
            | RowChunk::Section(_, indices)
            | RowChunk::Upcoming(indices)
            | RowChunk::Completed(indices) => indices,
        }
    }
}

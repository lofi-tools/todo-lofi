use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div, rgb,
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

pub struct TaskListView {
    task_views: Vec<Entity<TaskRow>>,
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
                NavBarEvent::OpenProjectPicker => {
                    // Picker open is handled by the Layout; the task list is
                    // unaffected.
                }
            });

        Self {
            task_views: Vec::new(),
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
    /// bottom; reopened tasks jump back up right away.
    pub fn on_task_done_toggled(&mut self, task_id: u64, done: bool, cx: &mut Context<Self>) {
        for row in self.task_views.clone() {
            row.update(cx, |row, cx| {
                if row.task_id() == task_id {
                    row.set_done(done, cx);
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
                this.update(cx, |this, cx| {
                    this.selected_labels = labels;
                    this.set_tasks_with_path(tasks, &path, &this.selected_labels.clone(), cx);
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
                this.update(cx, |this, cx| {
                    this.set_tasks_with_path(
                        tasks,
                        &selected_path,
                        &selected_labels,
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
        let create_task = self
            .store
            .insert_task(TaskCreate::default().title(title), tag_name, cx);

        self._fetch_tasks = Some(cx.spawn(async move |this, cx| {
            let new_tasks = match create_task.await {
                Ok(new_tasks) => new_tasks,
                Err(e) => {
                    tracing::error!("Failed to insert task: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.set_tasks_with_path(
                    new_tasks,
                    &this.selected_path.clone(),
                    &this.selected_labels.clone(),
                    cx,
                );
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
        cx: &mut Context<Self>,
    ) {
        self.editing = false;
        let selected_task_id = self.selected.as_ref().map(|task| task.id);
        self.task_views = tasks
            .into_iter()
            .map(|task| {
                let is_selected = Some(task.id) == selected_task_id;
                let row = cx.new(|cx| {
                    TaskRow::new(
                        task,
                        self.store.clone(),
                        selected_path.to_vec(),
                        selected_labels.to_vec(),
                        is_selected,
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
                })
                .detach();
                row
            })
            .collect();
    }

    pub fn set_tasks(&mut self, tasks: Vec<TaskWithMeta>, cx: &mut Context<Self>) {
        self.set_tasks_with_path(
            tasks,
            &self.selected_path.clone(),
            &self.selected_labels.clone(),
            cx,
        );
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

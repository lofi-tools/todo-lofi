use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::Disableable;
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::components::{DateTimePicker, DateTimePickerEvent};
use crate::store::Store;
use crate::theme::{APP_BG, CARD_BG, HAIRLINE};

use super::repeat_picker::{RepeatPicker, RepeatPickerEvent};
use super::task_picker::{TaskPicker, TaskPickerEvent};

/// How long after an outside mousedown closed a picker card before the
/// toggle button treats a click as a fresh open rather than the same click
/// that closed it (via `on_mouse_down_out`).
const OUTSIDE_CLOSE_IGNORE_WINDOW: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone)]
pub enum TaskDetailsEvent {
    Toggled {
        task_id: u64,
        done: bool,
    },
    TitleCommitted {
        task_id: u64,
        title: String,
    },
    PendingConfirmed {
        selected: Option<TaskWithMeta>,
    },
    PendingCancelled,
    SelectTask {
        task_id: u64,
    },
    /// Fresh DB state after a write, for syncing the task list row.
    TaskRefreshed(TaskWithMeta),
    /// A subtask was created; the task list reloads its current view.
    SubtaskCreated,
    /// A follow-up task was created; the task list reloads its current view.
    FollowUpCreated,
}

/// A selection change that arrived while edits were unsaved. `Some` selects
/// a task, `None` deselects.
type PendingSelection = Option<TaskWithMeta>;

pub struct TaskDetails {
    selected: Option<TaskWithMeta>,
    store: Store,
    editing_title: bool,
    title_input: Option<Entity<InputState>>,
    _title_subscription: Option<Subscription>,
    editing_description: bool,
    description_input: Option<Entity<InputState>>,
    _description_subscription: Option<Subscription>,
    confirming: bool,
    pending: Option<PendingSelection>,
    blockers: Vec<storage::Task>,
    _blockers_fetch: Option<gpui::Task<()>>,
    until_picker: Option<Entity<DateTimePicker>>,
    _until_picker_subscription: Option<Subscription>,
    /// When the card was last closed by an outside mousedown. The toggle
    /// button ignores a click within a short window so the capture-phase
    /// close and the bubble-phase toggle don't cancel out.
    until_outside_closed_at: Option<std::time::Instant>,
    time_edit: Option<TimeEditInputs>,
    focus_time_edit: bool,
    blocker_picker: Option<Entity<TaskPicker>>,
    _blocker_picker_subscription: Option<Subscription>,
    /// Same as `until_outside_closed_at`, for the blocker picker card.
    blocker_outside_closed_at: Option<std::time::Instant>,
    after_tasks: Vec<storage::Task>,
    _after_fetch: Option<gpui::Task<()>>,
    after_picker: Option<Entity<TaskPicker>>,
    _after_picker_subscription: Option<Subscription>,
    /// Same as `blocker_outside_closed_at`, for the after-task picker card.
    after_outside_closed_at: Option<std::time::Instant>,
    /// Inline message when a relationship write fails, e.g. adding a link
    /// that would close a dependency cycle.
    link_error: Option<String>,
    /// True while the "+ subtasks" button shows an inline title input.
    adding_subtask: bool,
    subtask_input: Option<Entity<InputState>>,
    _subtask_subscription: Option<Subscription>,
    subtasks: Vec<storage::Task>,
    _subtasks_fetch: Option<gpui::Task<()>>,
    /// The selected task's parent (when it is a subtask), for the parent
    /// link in the details view.
    parent: Option<storage::Task>,
    _parent_fetch: Option<gpui::Task<()>>,
    /// True while the "+ follow-up task" button shows an inline title input.
    adding_follow_up: bool,
    follow_up_input: Option<Entity<InputState>>,
    _follow_up_subscription: Option<Subscription>,
    /// Tasks that this task blocks ("Linked to").
    blocking: Vec<storage::Task>,
    _blocking_fetch: Option<gpui::Task<()>>,
    repeat_picker: Option<Entity<RepeatPicker>>,
    _repeat_subscription: Option<Subscription>,
    /// Same as the other `*_outside_closed_at` markers, for the repeat card.
    repeat_outside_closed_at: Option<std::time::Instant>,
}

struct TimeEditInputs {
    hour: Entity<InputState>,
    minute: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl Clone for TimeEditInputs {
    fn clone(&self) -> Self {
        // Clones are for rendering only; subscriptions stay owned by the
        // original in `time_edit`.
        Self {
            hour: self.hour.clone(),
            minute: self.minute.clone(),
            _subscriptions: Vec::new(),
        }
    }
}

impl TaskDetails {
    pub fn new(store: Store, _cx: &mut Context<Self>) -> Self {
        Self {
            selected: None,
            store,
            editing_title: false,
            title_input: None,
            _title_subscription: None,
            editing_description: false,
            description_input: None,
            _description_subscription: None,
            confirming: false,
            pending: None,
            blockers: Vec::new(),
            _blockers_fetch: None,
            until_picker: None,
            _until_picker_subscription: None,
            until_outside_closed_at: None,
            time_edit: None,
            focus_time_edit: false,
            blocker_picker: None,
            _blocker_picker_subscription: None,
            blocker_outside_closed_at: None,
            after_tasks: Vec::new(),
            _after_fetch: None,
            after_picker: None,
            _after_picker_subscription: None,
            after_outside_closed_at: None,
            link_error: None,
            adding_subtask: false,
            subtask_input: None,
            _subtask_subscription: None,
            subtasks: Vec::new(),
            _subtasks_fetch: None,
            parent: None,
            _parent_fetch: None,
            adding_follow_up: false,
            follow_up_input: None,
            _follow_up_subscription: None,
            blocking: Vec::new(),
            _blocking_fetch: None,
            repeat_picker: None,
            _repeat_subscription: None,
            repeat_outside_closed_at: None,
        }
    }

    pub fn set_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        self.apply_selected(task, cx);
    }

    fn apply_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        let parent_id = task.parent_id;
        let fetch = self.store.list_blockers(task.id, cx);
        let after_fetch = self.store.list_after(task.id, cx);
        let subtasks_fetch = self.store.list_subtasks(task.id, cx);
        let blocking_fetch = self.store.list_blocking_tasks(task.id, cx);
        self.selected = Some(task);
        self.blockers = Vec::new();
        self.after_tasks = Vec::new();
        self.subtasks = Vec::new();
        self.blocking = Vec::new();
        self.parent = None;
        self.link_error = None;
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        self.close_time_edit();
        self.abandon_subtask();
        self.abandon_follow_up();
        self._blockers_fetch = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(blockers) => {
                this.update(cx, |this, cx| {
                    this.blockers = blockers;
                    this._blockers_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch blockers: {e}");
            }
        }));
        self._after_fetch = Some(cx.spawn(async move |this, cx| match after_fetch.await {
            Ok(after_tasks) => {
                this.update(cx, |this, cx| {
                    this.after_tasks = after_tasks;
                    this._after_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch after tasks: {e}");
            }
        }));
        self._subtasks_fetch = Some(cx.spawn(async move |this, cx| {
            match subtasks_fetch.await {
                Ok(subtasks) => {
                    this.update(cx, |this, cx| {
                        this.subtasks = subtasks;
                        this._subtasks_fetch = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to fetch subtasks: {e}");
                }
            }
        }));
        if let Some(parent_id) = parent_id {
            let parent_fetch = self.store.get_task(parent_id, cx);
            self._parent_fetch = Some(cx.spawn(async move |this, cx| {
                match parent_fetch.await {
                    Ok(parent) => {
                        this.update(cx, |this, cx| {
                            this.parent = Some(parent);
                            this._parent_fetch = None;
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to fetch parent task: {e}");
                    }
                }
            }));
        }
        self._blocking_fetch = Some(cx.spawn(async move |this, cx| {
            match blocking_fetch.await {
                Ok(blocking) => {
                    this.update(cx, |this, cx| {
                        this.blocking = blocking;
                        this._blocking_fetch = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to fetch linked tasks: {e}");
                }
            }
        }));
        cx.notify();
    }

    /// Reload the selected task's subtasks from the DB, e.g. right after a
    /// new subtask was created.
    fn refresh_subtasks(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        let fetch = self.store.list_subtasks(task_id, cx);
        self._subtasks_fetch = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(subtasks) => {
                this.update(cx, |this, cx| {
                    this.subtasks = subtasks;
                    this._subtasks_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch subtasks: {e}");
            }
        }));
    }

    pub fn set_blockers(&mut self, blockers: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.blockers = blockers;
        cx.notify();
    }

    pub fn set_after_tasks(&mut self, after_tasks: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.after_tasks = after_tasks;
        cx.notify();
    }

    /// Fresh blocked state from the loaded blockers plus `blocked_until`,
    /// so reopening a blocker re-blocks immediately without a refetch.
    fn computed_blocked(&self) -> bool {
        let Some(task) = &self.selected else {
            return false;
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if task.blocked_until.is_some_and(|until| until > now_secs) {
            return true;
        }
        self.blockers.iter().any(|blocker| !blocker.done)
    }

    pub fn selected_task(&self) -> Option<TaskWithMeta> {
        self.selected.clone()
    }

    /// Request selecting a task. When edits are unsaved and the task is a
    /// different one, the request is stashed and a confirm dialog is shown
    /// instead; returns true when deferred.
    pub fn request_select(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) -> bool {
        let same_task = self.selected.as_ref().is_some_and(|t| t.id == task.id);
        if self.is_editing() && !same_task {
            self.pending = Some(Some(task));
            self.confirming = true;
            cx.notify();
            true
        } else {
            self.set_selected(task, cx);
            false
        }
    }

    /// Request deselecting. Defers with a confirm dialog when edits are
    /// unsaved; returns true when deferred.
    pub fn request_clear(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_editing() {
            self.pending = Some(None);
            self.confirming = true;
            cx.notify();
            true
        } else {
            self.clear(cx);
            false
        }
    }

    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.confirming = false;
        self.abandon_edits();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        match pending {
            Some(task) => self.apply_selected(task, cx),
            None => self.clear(cx),
        }
        let selected = self.selected.clone();
        cx.emit(TaskDetailsEvent::PendingConfirmed { selected });
        cx.notify();
    }

    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        if !self.confirming {
            return;
        }
        self.pending = None;
        self.confirming = false;
        cx.emit(TaskDetailsEvent::PendingCancelled);
        cx.notify();
    }

    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    pub fn update_title(&mut self, task_id: u64, title: String, cx: &mut Context<Self>) {
        if self
            .selected
            .as_ref()
            .is_some_and(|task| task.id == task_id)
        {
            if let Some(selected) = &mut self.selected {
                selected.task.title = title;
            }
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.blockers = Vec::new();
        self.after_tasks = Vec::new();
        self.subtasks = Vec::new();
        self.blocking = Vec::new();
        self.parent = None;
        self.link_error = None;
        self.close_until_panel();
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.cancel_editing(cx);
        cx.notify();
    }

    pub fn is_editing(&self) -> bool {
        self.editing_title || self.editing_description
    }

    fn abandon_edits(&mut self) {
        self.editing_title = false;
        self.title_input = None;
        self._title_subscription = None;
        self.editing_description = false;
        self.description_input = None;
        self._description_subscription = None;
        self.close_time_edit();
    }

    fn close_time_edit(&mut self) {
        self.time_edit = None;
        self.focus_time_edit = false;
    }

    /// Today's or tomorrow's date label plus current time for the Until row.
    /// Returns None for other dates, which render as static text.
    fn until_today_tomorrow(&self) -> Option<(String, u8, u8)> {
        let until = self.selected.as_ref()?.blocked_until?;
        let zoned = jiff::Timestamp::from_second(until as i64)
            .ok()?
            .to_zoned(jiff::tz::TimeZone::system());
        let today = crate::components::date_time_picker::today_date();
        let day_label = if zoned.date() == today {
            "today".to_string()
        } else if Some(zoned.date()) == today.tomorrow().ok() {
            "tomorrow".to_string()
        } else {
            return None;
        };
        Some((day_label, zoned.hour() as u8, zoned.minute() as u8))
    }

    /// Ensure the HH:MM inputs exist (creating + autofocus on first need),
    /// then commit their content, rebuilding them from the stored time when
    /// invalid or in the past.
    fn commit_time_inputs(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.time_edit.clone() else {
            return;
        };
        let Some(until) = self.selected.as_ref().and_then(|task| task.blocked_until) else {
            return;
        };
        let parse_cell = |entity: &Entity<InputState>, cx: &App| {
            entity
                .read(cx)
                .text()
                .to_string()
                .trim()
                .to_string()
                .parse::<u8>()
                .ok()
        };
        let (hour, minute) = (parse_cell(&edit.hour, cx), parse_cell(&edit.minute, cx));
        let timestamp = jiff::Timestamp::from_second(until as i64)
            .ok()
            .map(|stamp| stamp.to_zoned(jiff::tz::TimeZone::system()))
            .and_then(|zoned| {
                if hour.is_some_and(|h| h < 24) && minute.is_some_and(|m| m < 60) {
                    zoned
                        .date()
                        .at(hour.unwrap_or(0) as i8, minute.unwrap_or(0) as i8, 0, 0)
                        .to_zoned(jiff::tz::TimeZone::system())
                        .ok()
                        .map(|zoned| zoned.timestamp().as_second())
                } else {
                    None
                }
            });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64;
        match timestamp {
            Some(timestamp) if timestamp > now => {
                self.apply_blocked_until(timestamp as u64, false, cx);
            }
            _ => {
                self.time_edit = None;
                self.focus_time_edit = false;
                cx.notify();
            }
        }
    }

    pub fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        if self.editing_title || self.editing_description {
            self.editing_title = false;
            self.title_input = None;
            self._title_subscription = None;
            self.editing_description = false;
            self.description_input = None;
            self._description_subscription = None;
            cx.notify();
        }
    }

    fn begin_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        if self.editing_title {
            return;
        }
        let title = task.title.clone();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&title, window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_title_edit(cx);
            }
        });
        self.title_input = Some(input.clone());
        self._title_subscription = Some(subscription);
        self.editing_title = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_title_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.title_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        if title.is_empty() {
            self.cancel_editing(cx);
            return;
        }
        let task_id = task.id;
        if let Some(selected) = &mut self.selected {
            selected.task.title = title.clone();
        }
        self.editing_title = false;
        self.title_input = None;
        self._title_subscription = None;
        self.store.rename_task(task_id, title.clone(), cx).detach();
        cx.emit(TaskDetailsEvent::TitleCommitted { task_id, title });
        cx.notify();
    }

    fn begin_description_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        if self.editing_description {
            return;
        }
        let description = task.description.clone().unwrap_or_default();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&description, window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_description_edit(cx);
            }
        });
        self.description_input = Some(input.clone());
        self._description_subscription = Some(subscription);
        self.editing_description = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_description_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.description_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let description = input.read(cx).text().to_string();
        let description = description.trim().to_string();
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.task.description = if description.is_empty() {
                None
            } else {
                Some(description.clone())
            };
        }
        self.editing_description = false;
        self.description_input = None;
        self._description_subscription = None;
        let value = if description.is_empty() {
            None
        } else {
            Some(description)
        };
        cx.spawn(async move |this, cx| {
            if let Err(e) = store.set_task_description(task_id, value, cx).await {
                tracing::error!(?e, "Failed set_task_description");
            }
            this.update(cx, |_, cx| {
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Drop the inline subtask input without notifying (callers that clear
    /// state on selection change notify themselves).
    fn abandon_subtask(&mut self) {
        self.adding_subtask = false;
        self.subtask_input = None;
        self._subtask_subscription = None;
    }

    pub fn adding_subtask(&self) -> bool {
        self.adding_subtask
    }

    /// Toggle the inline subtask input: clicking the button while already
    /// adding cancels, otherwise an autofocused title input appears below
    /// the relationship buttons.
    fn begin_subtask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        if self.adding_subtask {
            self.cancel_subtask(cx);
            return;
        }
        self.abandon_follow_up();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Subtask title...", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_subtask(cx);
            }
        });
        let focus_input = input.clone();
        self.subtask_input = Some(input);
        self._subtask_subscription = Some(subscription);
        self.adding_subtask = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            focus_input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_subtask(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.subtask_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let parent_id = task.id;
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        self.abandon_subtask();
        if title.is_empty() {
            cx.notify();
            return;
        }
        let create = self.store.insert_subtask(parent_id, title, cx);
        cx.spawn(async move |this, cx| match create.await {
            Ok(_created) => {
                this.update(cx, |this, cx| {
                    this.refresh_subtasks(cx);
                    cx.emit(TaskDetailsEvent::SubtaskCreated);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to create subtask: {e}");
                this.update(cx, |this, cx| {
                    this.link_error = Some(format!("Couldn't create subtask: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_subtask(&mut self, cx: &mut Context<Self>) {
        if !self.adding_subtask {
            return;
        }
        self.abandon_subtask();
        cx.notify();
    }

    /// Drop the inline follow-up input without notifying (callers that
    /// clear state on selection change notify themselves).
    fn abandon_follow_up(&mut self) {
        self.adding_follow_up = false;
        self.follow_up_input = None;
        self._follow_up_subscription = None;
    }

    pub fn adding_follow_up(&self) -> bool {
        self.adding_follow_up
    }

    /// Toggle the inline follow-up input: clicking the button while already
    /// adding cancels, otherwise an autofocused title input appears below
    /// the relationship buttons.
    fn begin_follow_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        if self.adding_follow_up {
            self.cancel_follow_up(cx);
            return;
        }
        self.abandon_subtask();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Follow-up title...", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_follow_up(cx);
            }
        });
        let focus_input = input.clone();
        self.follow_up_input = Some(input);
        self._follow_up_subscription = Some(subscription);
        self.adding_follow_up = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            focus_input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_follow_up(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.follow_up_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let blocked_by = task.id;
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        self.abandon_follow_up();
        if title.is_empty() {
            cx.notify();
            return;
        }
        let create = self.store.create_follow_up(blocked_by, title, cx);
        cx.spawn(async move |this, cx| match create.await {
            Ok(_created) => {
                this.update(cx, |this, cx| {
                    this.link_error = None;
                    this.refresh_blocking(cx);
                    cx.emit(TaskDetailsEvent::FollowUpCreated);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to create follow-up task: {e}");
                this.update(cx, |this, cx| {
                    this.link_error = Some(format!("Couldn't create follow-up: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_follow_up(&mut self, cx: &mut Context<Self>) {
        if !self.adding_follow_up {
            return;
        }
        self.abandon_follow_up();
        cx.notify();
    }

    /// Reload the tasks this task blocks ("Linked to"), e.g. right after a
    /// follow-up task was created.
    fn refresh_blocking(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        let fetch = self.store.list_blocking_tasks(task_id, cx);
        self._blocking_fetch = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(blocking) => {
                this.update(cx, |this, cx| {
                    this.blocking = blocking;
                    this._blocking_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch linked tasks: {e}");
            }
        }));
    }

    /// Remove the blocker link so `linked_id` is no longer blocked by the
    /// selected task.
    fn remove_linked(&mut self, linked_id: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let remove = self.store.remove_blocker(linked_id, task.id, cx);
        cx.spawn(async move |this, cx| match remove.await {
            Ok(_) => {
                this.update(cx, |this, cx| {
                    this.refresh_blocking(cx);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to unlink task: {e}");
            }
        })
        .detach();
    }

    fn close_repeat_picker(&mut self) {
        self.repeat_picker = None;
        self._repeat_subscription = None;
    }

    pub fn close_repeat_picker_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_repeat_picker();
        cx.notify();
    }

    pub fn repeat_picker_open(&self) -> bool {
        self.repeat_picker.is_some()
    }

    /// True when the card was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_repeat_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.repeat_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_repeat_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.until_outside_closed_at = None;
        self.blocker_outside_closed_at = None;
        self.after_outside_closed_at = None;
        self.close_until_panel();
        self.close_blocker_picker();
        self.close_after_picker();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.link_error = None;
        let picker = cx.new(|cx| RepeatPicker::new(None, window, cx));
        let subscription = cx.subscribe(&picker, |this, _picker, event, cx| match event {
            RepeatPickerEvent::Saved { interval_days } => {
                this.close_repeat_picker();
                let Some(task) = this.selected.clone() else {
                    return;
                };
                let task_id = task.id;
                let name = task.title.clone();
                let set = this.store.set_repeat(task_id, name, *interval_days, cx);
                cx.spawn(async move |this, cx| match set.await {
                    Ok(_template) => {
                        this.update(cx, |this, cx| {
                            this.link_error = None;
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to save repeat template: {e}");
                        this.update(cx, |this, cx| {
                            this.link_error = Some(format!("Couldn't save repeat: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            RepeatPickerEvent::Removed => {
                this.close_repeat_picker();
                let Some(task_id) = this.selected.as_ref().map(|task| task.id) else {
                    return;
                };
                let remove = this.store.remove_repeat(task_id, cx);
                cx.spawn(async move |this, cx| match remove.await {
                    Ok(()) => {
                        this.update(cx, |this, cx| {
                            this.link_error = None;
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to remove repeat template: {e}");
                        this.update(cx, |this, cx| {
                            this.link_error = Some(format!("Couldn't remove repeat: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
        });
        self.repeat_picker = Some(picker.clone());
        self._repeat_subscription = Some(subscription);
        cx.notify();
        let fetch = self.store.get_repeat(task_id, cx);
        cx.spawn(async move |this, cx| {
            let template = match fetch.await {
                Ok(template) => template,
                Err(e) => {
                    tracing::error!("Failed to fetch repeat template: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                if let Some(picker) = this.repeat_picker.clone() {
                    picker.update(cx, |picker, cx| picker.set_current(template, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn repeat_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.repeat_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.repeat_outside_closed_at = Some(std::time::Instant::now());
                    this.close_repeat_picker_and_notify(cx);
                }))
                .child(picker)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn close_until_panel(&mut self) {
        self.until_picker = None;
        self._until_picker_subscription = None;
    }

    pub fn until_panel_open(&self) -> bool {
        self.until_picker.is_some()
    }

    pub fn close_until_panel_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_until_panel();
        cx.notify();
    }

    /// True when the card was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_until_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.until_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_until_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        self.blocker_outside_closed_at = None;
        self.after_outside_closed_at = None;
        self.repeat_outside_closed_at = None;
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        let picker = cx.new(|cx| DateTimePicker::new(window, cx));
        let subscription = cx.subscribe(&picker, |this, _picker, event, cx| match event {
            DateTimePickerEvent::Committed(until) => {
                eprintln!("DBG committed: {until}");
                this.apply_blocked_until(*until, true, cx);
            }
        });
        self.until_picker = Some(picker);
        self._until_picker_subscription = Some(subscription);
        cx.notify();
    }

    fn close_blocker_picker(&mut self) {
        self.blocker_picker = None;
        self._blocker_picker_subscription = None;
    }

    pub fn close_blocker_picker_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_blocker_picker();
        cx.notify();
    }

    pub fn blocker_picker_open(&self) -> bool {
        self.blocker_picker.is_some()
    }

    /// True when the picker was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_blocker_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.blocker_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_blocker_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.until_outside_closed_at = None;
        self.after_outside_closed_at = None;
        self.repeat_outside_closed_at = None;
        self.close_until_panel();
        self.close_after_picker();
        self.close_repeat_picker();
        let picker = cx.new(|cx| TaskPicker::new(Vec::new(), window, cx));
        let subscription = cx.subscribe(&picker, move |this, _picker, event, cx| match event {
            TaskPickerEvent::Selected(blocker_id) => {
                this.close_blocker_picker();
                let add = this.store.add_blocker(task_id, *blocker_id, cx);
                cx.spawn(async move |this, cx| match add.await {
                    Ok(blockers) => {
                        this.update(cx, |this, cx| {
                            this.set_blockers(blockers, cx);
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to add blocker: {e}");
                    }
                })
                .detach();
            }
            TaskPickerEvent::Dismissed => {
                this.close_blocker_picker();
                cx.notify();
            }
        });
        self.blocker_picker = Some(picker.clone());
        self._blocker_picker_subscription = Some(subscription);
        cx.notify();
        window.on_next_frame(move |window, cx| {
            picker.update(cx, |picker, cx| {
                picker.focus_filter(window, cx);
            });
        });
        let fetch = self.store.blocker_candidates(task_id, cx);
        cx.spawn(async move |this, cx| {
            let tasks = match fetch.await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::error!("Failed to fetch blocker candidates: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                if let Some(picker) = this.blocker_picker.clone() {
                    picker.update(cx, |picker, cx| picker.set_tasks(tasks, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn close_after_picker(&mut self) {
        self.after_picker = None;
        self._after_picker_subscription = None;
    }

    pub fn close_after_picker_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_after_picker();
        cx.notify();
    }

    pub fn after_picker_open(&self) -> bool {
        self.after_picker.is_some()
    }

    /// True when the picker was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_after_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.after_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_after_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.until_outside_closed_at = None;
        self.blocker_outside_closed_at = None;
        self.repeat_outside_closed_at = None;
        self.close_until_panel();
        self.close_blocker_picker();
        self.close_repeat_picker();
        self.link_error = None;
        let picker = cx.new(|cx| TaskPicker::new(Vec::new(), window, cx));
        let subscription = cx.subscribe(&picker, move |this, _picker, event, cx| match event {
            TaskPickerEvent::Selected(after_id) => {
                this.close_after_picker();
                let add = this.store.add_after(task_id, *after_id, cx);
                cx.spawn(async move |this, cx| match add.await {
                    Ok(after_tasks) => {
                        this.update(cx, |this, cx| {
                            this.link_error = None;
                            this.set_after_tasks(after_tasks, cx);
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to add after link: {e}");
                        this.update(cx, |this, cx| {
                            this.link_error = Some(format!("Couldn't add task: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            TaskPickerEvent::Dismissed => {
                this.close_after_picker();
                cx.notify();
            }
        });
        self.after_picker = Some(picker.clone());
        self._after_picker_subscription = Some(subscription);
        cx.notify();
        window.on_next_frame(move |window, cx| {
            picker.update(cx, |picker, cx| {
                picker.focus_filter(window, cx);
            });
        });
        let fetch = self.store.after_candidates(task_id, cx);
        cx.spawn(async move |this, cx| {
            let tasks = match fetch.await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::error!("Failed to fetch after candidates: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                if let Some(picker) = this.after_picker.clone() {
                    picker.update(cx, |picker, cx| picker.set_tasks(tasks, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_blocked_until(&mut self, until: u64, refocus_time: bool, cx: &mut Context<Self>) {
        self.write_blocked_until(Some(until), refocus_time, cx);
    }

    fn clear_blocked_until(&mut self, cx: &mut Context<Self>) {
        self.write_blocked_until(None, false, cx);
    }

    /// Optimistic local update, then persist and reload from the DB so both
    /// this panel and the task list converge on stored truth.
    fn write_blocked_until(
        &mut self,
        value: Option<u64>,
        refocus_time: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = &self.selected else {
            return;
        };
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.task.blocked_until = value;
        }
        self.close_until_panel();
        self.time_edit = None;
        self.focus_time_edit = refocus_time && self.until_today_tomorrow().is_some();
        let write = store.set_blocked_until(task_id, value, cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = write.await {
                tracing::error!(?e, "Failed set_blocked_until");
                return;
            }
            let reload = store.reload_task(task_id, cx);
            match reload.await {
                Ok(fresh) => {
                    this.update(cx, |this, cx| {
                        this.selected = Some(fresh.clone());
                        cx.emit(TaskDetailsEvent::TaskRefreshed(fresh));
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!(?e, "Failed to reload task after blocked_until write");
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Floating card below the "+ blocked until" button: menu-style rows
    /// separated by hairlines, floating above the content underneath.
    fn until_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.until_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.until_outside_closed_at = Some(std::time::Instant::now());
                    this.close_until_panel_and_notify(cx);
                }))
                .child(picker)
        } else {
            div()
        }
    }

    fn blocker_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.blocker_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.blocker_outside_closed_at = Some(std::time::Instant::now());
                    this.close_blocker_picker_and_notify(cx);
                }))
                .child(picker)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn after_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.after_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.after_outside_closed_at = Some(std::time::Instant::now());
                    this.close_after_picker_and_notify(cx);
                }))
                .child(picker)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn remove_blocker(&mut self, blocker_id: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let remove = self.store.remove_blocker(task.id, blocker_id, cx);
        cx.spawn(async move |this, cx| match remove.await {
            Ok(blockers) => {
                this.update(cx, |this, cx| {
                    this.set_blockers(blockers, cx);
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to remove blocker: {e}");
            }
        })
        .detach();
    }

    fn remove_after(&mut self, after_id: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let remove = self.store.remove_after(task.id, after_id, cx);
        cx.spawn(async move |this, cx| match remove.await {
            Ok(after_tasks) => {
                this.update(cx, |this, cx| {
                    this.set_after_tasks(after_tasks, cx);
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to remove after link: {e}");
            }
        })
        .detach();
    }

    /// "Blocked until" row. For today/tomorrow the right side is an
    /// editable HH:MM pair (created lazily, autofocused once after picking);
    /// other dates render as static text.
    fn until_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clear_button = Button::new("clear-blocked-until")
            .ghost()
            .compact()
            .label("×")
            .tooltip("Clear time block")
            .on_click(cx.listener(|this, _, _, cx| {
                this.clear_blocked_until(cx);
            }));

        if let Some((day_label, hour, minute)) = self.until_today_tomorrow() {
            if self.time_edit.is_none() {
                let make_cell = |value: String, window: &mut Window, cx: &mut Context<Self>| {
                    let input = cx.new(|cx| {
                        let mut state = InputState::new(window, cx);
                        state.set_value(&value, window, cx);
                        state
                    });
                    let subscription = cx.subscribe(&input, |this, _, event, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            this.commit_time_inputs(cx);
                        }
                    });
                    (input, subscription)
                };
                let (hour_input, hour_sub) = make_cell(format!("{hour:02}"), window, cx);
                let (minute_input, minute_sub) = make_cell(format!("{minute:02}"), window, cx);
                self.time_edit = Some(TimeEditInputs {
                    hour: hour_input.clone(),
                    minute: minute_input,
                    _subscriptions: vec![hour_sub, minute_sub],
                });
                if self.focus_time_edit {
                    self.focus_time_edit = false;
                    window.on_next_frame(move |window, cx| {
                        hour_input.update(cx, |state, cx| state.focus(window, cx));
                    });
                }
            }
            let time_cells = self.time_edit.clone().map(|edit| {
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .w(px(30.))
                            .child(Input::new(&edit.hour).small().appearance(false)),
                    )
                    .child(div().text_sm().text_color(rgb(0xa3a3a3)).child(":"))
                    .child(
                        div()
                            .w(px(30.))
                            .child(Input::new(&edit.minute).small().appearance(false)),
                    )
            });
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(0xe5e5e5))
                        .child(format!("Blocked until {day_label}")),
                )
                .children(time_cells)
                .child(clear_button)
        } else if let Some(until) = self.selected.as_ref().and_then(|t| t.blocked_until) {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(0xe5e5e5))
                        .child(format!("Blocked until {}", format_deadline(until))),
                )
                .child(clear_button)
        } else {
            div()
        }
    }

    fn relationships_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut section = div().v_flex().gap_2().mt_2().child(
            div()
                .text_base()
                .font_bold()
                .underline()
                .text_color(rgb(0xe5e5e5))
                .child("Linked to"),
        );

        // Overlay zone: the lists paint first, then the buttons row and the
        // picker cards on top, so open cards always cover (and receive hits
        // before) the content underneath. The two-line button row is 56px
        // tall, so the lists start below it and cards float just under it.
        let mut lists = div().v_flex().gap_2().pt(px(64.));

        if self.adding_subtask
            && let Some(input) = self.subtask_input.clone()
        {
            lists = lists.child(div().ml_2().child(Input::new(&input)));
        }
        if self.adding_follow_up
            && let Some(input) = self.follow_up_input.clone()
        {
            lists = lists.child(div().ml_2().child(Input::new(&input)));
        }

        if self.computed_blocked() {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child("Blocked by"),
            );
        }

        let blocker_rows = self
            .blockers
            .clone()
            .into_iter()
            .map(|blocker| {
                let blocker_id = blocker.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("blocker-title", blocker_id))
                            .flex_1()
                            .text_sm()
                            .text_color(if blocker.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(blocker.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask {
                                    task_id: blocker_id,
                                });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if blocker.done { "done" } else { "" }.to_string()),
                    )
                    .child(
                        Button::new(("remove-blocker", blocker_id))
                            .ghost()
                            .compact()
                            .label("×")
                            .tooltip("Remove blocker")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_blocker(blocker_id, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        if !blocker_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(blocker_rows));
        }

        if self
            .selected
            .as_ref()
            .and_then(|t| t.blocked_until)
            .is_some()
        {
            lists = lists.child(self.until_row(window, cx));
        }

        if !self.after_tasks.is_empty() {
            lists = lists.child(div().text_xs().text_color(rgb(0xa3a3a3)).child("After"));
        }

        let after_rows = self
            .after_tasks
            .clone()
            .into_iter()
            .map(|after| {
                let after_id = after.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("after-title", after_id))
                            .flex_1()
                            .text_sm()
                            .text_color(if after.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(after.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: after_id });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if after.done { "done" } else { "" }.to_string()),
                    )
                    .child(
                        Button::new(("remove-after", after_id))
                            .ghost()
                            .compact()
                            .label("×")
                            .tooltip("Remove after task")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_after(after_id, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        if !after_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(after_rows));
        }

        if !self.blocking.is_empty() {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child("Linked to"),
            );
        }
        let linked_rows = self
            .blocking
            .clone()
            .into_iter()
            .map(|linked| {
                let linked_id = linked.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("linked-title", linked_id))
                            .flex_1()
                            .text_sm()
                            .text_color(if linked.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(linked.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: linked_id });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if linked.done { "done" } else { "" }.to_string()),
                    )
                    .child(
                        Button::new(("remove-linked", linked_id))
                            .ghost()
                            .compact()
                            .label("×")
                            .tooltip("Unlink task")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_linked(linked_id, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        if !linked_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(linked_rows));
        }

        // Subtasks live inside the lists so they paint before (under) the
        // floating picker cards, which are siblings added after `lists`.
        if !self.subtasks.is_empty() {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child(format!("Subtasks ({})", self.subtasks.len())),
            );
        }
        let subtask_rows = self
            .subtasks
            .clone()
            .into_iter()
            .map(|subtask| {
                let subtask_id = subtask.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("subtask-title", subtask_id))
                            .flex_1()
                            .text_sm()
                            .text_color(if subtask.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(subtask.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: subtask_id });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if subtask.done { "done" } else { "" }.to_string()),
                    )
            })
            .collect::<Vec<_>>();
        if !subtask_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(subtask_rows));
        }

        if let Some(error) = &self.link_error {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xff6b6b))
                    .child(error.clone()),
            );
        }

        section = section.child(
            div()
                .relative()
                .child(lists)
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .right(px(0.))
                        .v_flex()
                        .gap_2()
                        .child(
                            div()
                                .h_flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(
                                    relation_button("add-blocker", "+ blocked by task").on_click(
                                        cx.listener(|this, _, window, cx| {
                                            if this.blocker_picker_open() {
                                                this.close_blocker_picker_and_notify(cx);
                                            } else if this.take_recent_blocker_outside_close() {
                                                // The mousedown before this click already
                                                // closed the picker; don't reopen it.
                                            } else {
                                                this.open_blocker_picker(window, cx);
                                            }
                                        }),
                                    ),
                                )
                                .child(
                                    relation_button("add-blocked-until", "+ blocked until")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if this.until_panel_open() {
                                                this.close_until_panel_and_notify(cx);
                                            } else if this.take_recent_until_outside_close() {
                                                // The mousedown before this click already
                                                // closed the card; don't reopen it.
                                            } else {
                                                this.open_until_panel(window, cx);
                                            }
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .h_flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(relation_button("add-after", "+ after task").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        if this.after_picker_open() {
                                            this.close_after_picker_and_notify(cx);
                                        } else if this.take_recent_after_outside_close() {
                                            // The mousedown before this click already
                                            // closed the picker; don't reopen it.
                                        } else {
                                            this.open_after_picker(window, cx);
                                        }
                                    }),
                                ))
                                .child(
                                    relation_button("add-subtask", "+ subtask").on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.begin_subtask(window, cx);
                                        }),
                                    ),
                                )
                                .child(
                                    relation_button("add-follow-up", "+ follow-up task").on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.begin_follow_up(window, cx);
                                        }),
                                    ),
                                )
                                .child(
                                    relation_button("repeat-task", "repeat")
                                        .icon(gpui_component_assets::IconName::RefreshCw)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if this.repeat_picker_open() {
                                                this.close_repeat_picker_and_notify(cx);
                                            } else if this.take_recent_repeat_outside_close() {
                                                // The mousedown before this click already
                                                // closed the card; don't reopen it.
                                            } else {
                                                this.open_repeat_picker(window, cx);
                                            }
                                        })),
                                ),
                        ),
                )
                .when(self.until_panel_open(), |this| {
                    this.child(self.until_card(cx))
                })
                .child(self.blocker_card(cx))
                .child(self.after_card(cx))
                .child(self.repeat_card(cx)),
        );

        section
    }

}

/// Small transparent relationship button: gray text with a gray hairline
/// outline, shared by the buttons in the relationships section.
fn relation_button(id: &'static str, label: &str) -> Button {
    Button::new(id)
        .ghost()
        .compact()
        .with_size(gpui_component::Size::Small)
        .border_1()
        .border_color(rgb(HAIRLINE))
        .text_color(rgb(0xa3a3a3))
        .label(label)
}

fn field_label(label: &str) -> impl IntoElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(rgb(0xa3a3a3))
        .child(label.to_string())
}

fn field(label: &str, value: String) -> impl IntoElement {
    div()
        .v_flex()
        .gap_1()
        .child(field_label(label))
        .child(div().text_sm().text_color(rgb(0xe5e5e5)).child(value))
}

fn format_deadline(deadline: u64) -> String {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let dl_secs = deadline as i64;
    let time = jiff::Timestamp::from_second(dl_secs)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()))
        .map(|t| t.strftime("%-I:%M %p").to_string())
        .unwrap_or_default();
    if dl_secs <= now_secs {
        if time.is_empty() {
            "overdue".to_string()
        } else {
            format!("overdue ({time})")
        }
    } else {
        let days = (dl_secs - now_secs) as u64 / 86400;
        if days == 0 {
            format!("today, {time}")
        } else if days == 1 {
            format!("tomorrow, {time}")
        } else if days < 7 {
            format!("in {days} days")
        } else {
            format!("in {} weeks", days / 7)
        }
    }
}

impl EventEmitter<TaskDetailsEvent> for TaskDetails {}

impl Render for TaskDetails {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.selected {
            None => div().flex_1().flex().items_center().justify_center().child(
                div()
                    .text_sm()
                    .text_color(rgb(0x737373))
                    .child("Select a task to see details"),
            ),
            Some(task) => {
                let task_id = task.id;
                let done = task.done;
                let store = self.store.clone();
                let entity = cx.entity().clone();
                let blocked = self.computed_blocked();

                let mut details = div().v_flex().gap_3();
                let mut header = div().v_flex().gap_1();
                if !task.leaf_tags.is_empty() {
                    header = header.child(div().h_flex().gap_1().flex_wrap().children(
                        task.leaf_tags.iter().map(|tag| {
                            div()
                                .text_size(px(10.))
                                .px(px(4.))
                                .rounded(px(2.))
                                .bg(rgb(0x2a2a2a))
                                .text_color(rgb(0xa3a3a3))
                                .child(format!("#{tag}"))
                        }),
                    ));
                }
                header = header.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .child(
                            Checkbox::new(("details-checkbox", task_id))
                                .with_size(px(22.))
                                .checked(done)
                                .disabled(blocked && !done)
                                .on_click(move |new_done, _window, cx| {
                                    let store = store.clone();
                                    let entity = entity.clone();
                                    let new_done = *new_done;
                                    cx.spawn(async move |cx| {
                                        if let Err(e) =
                                            store.toggle_task_done(task_id, new_done, cx).await
                                        {
                                            tracing::error!(?e, "Failed toggle_task_done");
                                        }
                                        entity.update(cx, |this, cx| {
                                            if let Some(selected) = &mut this.selected {
                                                selected.task.done = new_done;
                                            }
                                            cx.emit(TaskDetailsEvent::Toggled {
                                                task_id,
                                                done: new_done,
                                            });
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }),
                        )
                        .child(
                            if let Some(input) = self.title_input.clone() {
                                div()
                                    .id(("details-title-edit", task_id))
                                    .flex_1()
                                    .child(
                                        Input::new(&input)
                                            .small()
                                            .appearance(false)
                                            .bg(rgb(APP_BG))
                                            .border_1()
                                            .border_color(rgb(HAIRLINE))
                                            .rounded_md(),
                                    )
                            } else {
                                div()
                                    .id(("details-title", task_id))
                                    .flex_1()
                                    .text_xl()
                                    .font_bold()
                                    .text_color(if done {
                                        rgb(0x666666)
                                    } else {
                                        rgb(0xe5e5e5)
                                    })
                                    .when(done, |this| this.line_through())
                                    .child(task.title.clone())
                                    .on_click(cx.listener(|this, event, window, cx| {
                                        if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                        {
                                            this.begin_title_edit(window, cx);
                                        }
                                    }))
                            },
                        ),
                );
                details = details.child(header);
                if let Some(parent) = self.parent.clone() {
                    let parent_id = parent.id;
                    details = details.child(
                        div()
                            .v_flex()
                            .gap_1()
                            .child(field_label("Parent"))
                            .child(
                                div()
                                    .id(("parent-link", parent_id))
                                    .text_sm()
                                    .text_color(rgb(0x93c5fd))
                                    .hover(|this| this.underline())
                                    .child(parent.title.clone())
                                    .on_click(cx.listener(move |_this, _, _, cx| {
                                        cx.emit(TaskDetailsEvent::SelectTask {
                                            task_id: parent_id,
                                        });
                                    })),
                            ),
                    );
                }
                if self.editing_description {
                    if let Some(input) = self.description_input.clone() {
                        details = details.child(
                            div()
                                .v_flex()
                                .gap_1()
                                .child(field_label("Description"))
                                .child(
                                    div().id(("details-description-edit", task_id)).child(
                                        Input::new(&input)
                                            .small()
                                            .appearance(false)
                                            .bg(rgb(APP_BG))
                                            .border_1()
                                            .border_color(rgb(HAIRLINE))
                                            .rounded_md(),
                                    ),
                                ),
                        );
                    }
                } else if let Some(desc) = &task.description
                    && !desc.is_empty()
                {
                    details = details.child(
                        div()
                            .v_flex()
                            .gap_1()
                            .child(field_label("Description"))
                            .child(
                                div()
                                    .id(("details-description", task_id))
                                    .text_sm()
                                    .text_color(rgb(0xe5e5e5))
                                    .child(desc.clone())
                                    .on_click(cx.listener(|this, event, window, cx| {
                                        if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                        {
                                            this.begin_description_edit(window, cx);
                                        }
                                    })),
                            ),
                    );
                }
                if let Some(deadline) = task.deadline {
                    details = details.child(field("Deadline", format_deadline(deadline)));
                }
                if let Some(until) = task.blocked_until {
                    details = details.child(field("Blocked until", format_deadline(until)));
                }
                if let Some(branch) = &task.branch_name
                    && !branch.is_empty()
                {
                    details = details.child(field("Branch", branch.clone()));
                }
                details = details.child(self.relationships_section(window, cx));
                details
            }
        };

        div()
            .id("task-details")
            .relative()
            .h_full()
            .v_flex()
            .p_4()
            .gap_4()
            .border_l_1()
            .border_color(rgb(0x333333))
            .on_click(cx.listener(|_, _, _, cx| {
                cx.stop_propagation();
            }))
            .child(body)
            .when(self.confirming, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .right(px(0.))
                        .bottom(px(0.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(0x000000cc))
                        .child(
                            div()
                                .v_flex()
                                .gap_3()
                                .w(px(260.))
                                .p_4()
                                .rounded_md()
                                .bg(rgb(0x2a2a2a))
                                .border_1()
                                .border_color(rgb(HAIRLINE))
                                .child(
                                    div()
                                        .v_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(rgb(0xe5e5e5))
                                                .child("Discard unsaved changes?"),
                                        )
                                        .child(div().text_xs().text_color(rgb(0xa3a3a3)).child(
                                            "Your title and description edits will be lost.",
                                        )),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("keep-editing")
                                                .ghost()
                                                .label("Keep editing")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cancel_pending(cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("discard-changes")
                                                .danger()
                                                .label("Discard")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.confirm_pending(cx);
                                                })),
                                        ),
                                ),
                        ),
                )
            })
    }
}

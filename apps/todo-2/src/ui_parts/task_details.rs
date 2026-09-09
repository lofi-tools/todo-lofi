use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::Disableable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::components::{DateTimePicker, DateTimePickerEvent};
use crate::store::Store;
use crate::theme::{APP_BG, CARD_BG, HAIRLINE};

use super::task_picker::{TaskPicker, TaskPickerEvent};

#[derive(Clone)]
pub enum TaskDetailsEvent {
    Toggled { task_id: u64, done: bool },
    TitleCommitted { task_id: u64, title: String },
    PendingConfirmed { selected: Option<TaskWithMeta> },
    PendingCancelled,
    SelectTask { task_id: u64 },
    /// Fresh DB state after a write, for syncing the task list row.
    TaskRefreshed(TaskWithMeta),
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
    time_edit: Option<TimeEditInputs>,
    focus_time_edit: bool,
    blocker_picker: Option<Entity<TaskPicker>>,
    _blocker_picker_subscription: Option<Subscription>,
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
            time_edit: None,
            focus_time_edit: false,
            blocker_picker: None,
            _blocker_picker_subscription: None,
        }
    }

    pub fn set_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        self.apply_selected(task, cx);
    }

    fn apply_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        let fetch = self.store.list_blockers(task.id, cx);
        self.selected = Some(task);
        self.blockers = Vec::new();
        self.close_blocker_picker();
        self.close_time_edit();
        self._blockers_fetch = Some(cx.spawn(async move |this, cx| {
            match fetch.await {
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
            }
        }));
        cx.notify();
    }

    pub fn set_blockers(&mut self, blockers: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.blockers = blockers;
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
        self.close_blocker_picker();
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
        if self.selected.as_ref().is_some_and(|task| task.id == task_id) {
            if let Some(selected) = &mut self.selected {
                selected.task.title = title;
            }
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.blockers = Vec::new();
        self.close_until_panel();
        self.close_blocker_picker();
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
        let Some(until) = self
            .selected
            .as_ref()
            .and_then(|task| task.blocked_until)
        else {
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

    fn open_until_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        self.close_blocker_picker();
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

    fn open_blocker_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.close_until_panel();
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
                .top(px(30.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
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
                .top(px(30.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_blocker_picker_and_notify(cx);
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
                let (hour_input, hour_sub) =
                    make_cell(format!("{hour:02}"), window, cx);
                let (minute_input, minute_sub) =
                    make_cell(format!("{minute:02}"), window, cx);
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
                    .child(div().w(px(30.)).child(
                        Input::new(&edit.hour).small().appearance(false),
                    ))
                    .child(div().text_sm().text_color(rgb(0xa3a3a3)).child(":"))
                    .child(div().w(px(30.)).child(
                        Input::new(&edit.minute).small().appearance(false),
                    ))
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

    fn relationships_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut section = div().v_flex().gap_2().mt_2().child(
            div()
                .text_base()
                .font_bold()
                .underline()
                .text_color(rgb(0xe5e5e5))
                .child("Relationships"),
        );

        section = section.child(
            div()
                .relative()
                .child(
                    div()
                        .h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .h(px(30.))
                        .child(
                            relation_button("add-blocker", "+ blocked by task").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_blocker_picker(window, cx);
                                }),
                            ),
                        )
                        .child(
                        relation_button("add-blocked-until", "+ blocked until").on_click(
                            cx.listener(|this, _, window, cx| {
                                if this.until_panel_open() {
                                    this.close_until_panel_and_notify(cx);
                                } else {
                                    this.open_until_panel(window, cx);
                                }
                            }),
                        ),
                        )
                        .child(
                            relation_button("add-subtask", "+ subtasks").tooltip("Coming soon"),
                        )
                        .child(
                            relation_button("add-follow-up", "+ follow-up tasks")
                                .tooltip("Coming soon"),
                        ),
                )
                .when(self.until_panel_open(), |this| {
                    this.child(self.until_card(cx))
                })
                .child(self.blocker_card(cx)),
        );

        if self.computed_blocked() {
            section = section.child(
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
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: blocker_id });
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
            section = section.child(div().v_flex().gap_1().ml_2().children(blocker_rows));
        }

        if self.selected.as_ref().and_then(|t| t.blocked_until).is_some() {
            section = section.child(self.until_row(window, cx));
        }

        section
    }
}

/// Small transparent relationship button: gray text with a gray hairline
/// outline, shared by the four buttons in the relationships section.
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
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
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
                if self.editing_description {
                    if let Some(input) = self.description_input.clone() {
                        details = details.child(
                            div().v_flex().gap_1().child(field_label("Description")).child(
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
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0xa3a3a3))
                                                .child(
                                                    "Your title and description edits will be lost.",
                                                ),
                                        ),
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

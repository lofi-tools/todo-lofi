use gpui::{
    AppContext, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
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
use crate::store::Store;
use crate::theme::{APP_BG, CARD_BG, HAIRLINE};

use super::task_picker::{TaskPicker, TaskPickerEvent};

#[derive(Clone)]
pub enum TaskDetailsEvent {
    Toggled { task_id: u64, done: bool },
    TitleCommitted { task_id: u64, title: String },
    PendingConfirmed { selected: Option<TaskWithMeta> },
    PendingCancelled,
    /// A blocker row was clicked: navigate to that task.
    SelectTask { task_id: u64 },
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
    until_panel_open: bool,
    until_input: Option<Entity<InputState>>,
    _until_subscription: Option<Subscription>,
    until_error: Option<String>,
    /// Date picked in the panel (grid or quick action); time below applies.
    until_date: Option<jiff::civil::Date>,
    until_hour: u8,
    until_minute: u8,
    /// Show the time picker once a date is chosen via today/tomorrow/grid.
    show_time_picker: bool,
    /// Month currently shown in the calendar grid.
    view_year: i16,
    view_month: i8,
    blocker_picker: Option<Entity<TaskPicker>>,
    _blocker_picker_subscription: Option<Subscription>,
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
            until_panel_open: false,
            until_input: None,
            _until_subscription: None,
            until_error: None,
            until_date: None,
            until_hour: 9,
            until_minute: 0,
            show_time_picker: false,
            view_year: 0,
            view_month: 0,
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
        self.until_panel_open = false;
        self.until_input = None;
        self._until_subscription = None;
        self.until_error = None;
        self.until_date = None;
        self.show_time_picker = false;
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
        let Some(task) = &self.selected else {
            return;
        };
        let task_id = task.id;
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

    pub fn until_panel_open(&self) -> bool {
        self.until_panel_open
    }

    pub fn close_until_panel_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_until_panel();
        cx.notify();
    }

    fn open_until_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let today = jiff::Zoned::now().date();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Type a date", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_until_text(cx);
            }
        });
        self.until_input = Some(input.clone());
        self._until_subscription = Some(subscription);
        self.until_panel_open = true;
        self.until_error = None;
        self.until_date = None;
        self.until_hour = 9;
        self.until_minute = 0;
        self.show_time_picker = false;
        self.view_year = today.year();
        self.view_month = today.month();
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn apply_blocked_until(&mut self, until: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.task.blocked_until = Some(until);
        }
        self.close_until_panel();
        let set = store.set_blocked_until(task_id, Some(until), cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = set.await {
                tracing::error!(?e, "Failed set_blocked_until");
            }
            this.update(cx, |_, cx| {
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn commit_until_text(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.until_input.clone() else {
            return;
        };
        if self.selected.is_none() {
            return;
        }
        let raw = input.read(cx).text().to_string();
        match parse_blocked_until(&raw) {
            None => {
                self.until_error = Some("Use YYYY-MM-DD HH:MM, in the future".to_string());
                cx.notify();
            }
            Some(until) => self.apply_blocked_until(until, cx),
        }
    }

    /// Commit the picked date with the picked time.
    fn commit_until_datetime(&mut self, cx: &mut Context<Self>) {
        let Some(date) = self.until_date else {
            return;
        };
        if self.selected.is_none() {
            return;
        }
        let timestamp = date
            .at(self.until_hour as i8, self.until_minute as i8, 0, 0)
            .to_zoned(jiff::tz::TimeZone::system())
            .map(|zoned| zoned.timestamp().as_second())
            .unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64;
        if timestamp <= now {
            self.until_error = Some("Pick a time in the future".to_string());
            cx.notify();
            return;
        }
        self.apply_blocked_until(timestamp as u64, cx);
    }

    fn quick_until_today(&mut self, cx: &mut Context<Self>) {
        self.until_date = Some(today_date());
        self.show_time_picker = true;
        self.until_error = None;
        cx.notify();
    }

    fn quick_until_tomorrow(&mut self, cx: &mut Context<Self>) {
        self.until_date = today_date().tomorrow().ok();
        self.show_time_picker = true;
        self.until_error = None;
        cx.notify();
    }

    fn quick_until_weekend(&mut self, cx: &mut Context<Self>) {
        self.until_date = Some(next_monday_offset_weekday(today_date(), 5, false));
        self.until_hour = 9;
        self.until_minute = 0;
        self.show_time_picker = false;
        self.until_error = None;
        cx.notify();
        self.commit_until_datetime(cx);
    }

    fn quick_until_next_week(&mut self, cx: &mut Context<Self>) {
        self.until_date = Some(next_monday_offset_weekday(today_date(), 0, true));
        self.until_hour = 9;
        self.until_minute = 0;
        self.show_time_picker = false;
        self.until_error = None;
        cx.notify();
        self.commit_until_datetime(cx);
    }

    fn shift_view_month(&mut self, delta: i32, cx: &mut Context<Self>) {
        let mut year = self.view_year as i32;
        let mut month = self.view_month as i32 + delta;
        while month < 1 {
            month += 12;
            year -= 1;
        }
        while month > 12 {
            month -= 12;
            year += 1;
        }
        self.view_year = year as i16;
        self.view_month = month as i8;
        cx.notify();
    }

    fn shift_hour(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.until_hour = (self.until_hour as i32 + delta).rem_euclid(24) as u8;
        cx.notify();
    }

    fn shift_minute(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.until_minute = (self.until_minute as i32 + delta).rem_euclid(60) as u8;
        cx.notify();
    }

    fn clear_blocked_until(&mut self, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.task.blocked_until = None;
        }
        let clear = store.set_blocked_until(task_id, None, cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = clear.await {
                tracing::error!(?e, "Failed clear_blocked_until");
            }
            this.update(cx, |_, cx| {
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Floating card below the "+ blocked until" button: menu-style rows
    /// separated by hairlines, floating above the content underneath.
    fn until_card(&mut self, task_id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let mut card = div()
            .absolute()
            .top(px(30.))
            .left(px(0.))
            .right(px(0.))
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(HAIRLINE))
            .rounded_md()
            .v_flex()
            .gap_0();

        if let Some(input) = self.until_input.clone() {
            card = card.child(
                div()
                    .v_flex()
                    .gap_0()
                    .child(
                        div()
                            .id(("blocked-until-edit", task_id))
                            .child(
                                Input::new(&input)
                                    .small()
                                    .appearance(false)
                                    .bg(rgb(CARD_BG)),
                            ),
                    )
                    .children(self.until_error.clone().map(|error| {
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0xe06c60))
                            .child(error)
                    })),
            );
        }

        card = card.child(
            div()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(rgb(HAIRLINE))
                .h_flex()
                .flex_wrap()
                .gap_2()
                .child(
                    relation_button("quick-today", "Today")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.quick_until_today(cx);
                        })),
                )
                .child(
                    relation_button("quick-tomorrow", "Tomorrow")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.quick_until_tomorrow(cx);
                        })),
                )
                .child(
                    relation_button("quick-weekend", "This weekend")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.quick_until_weekend(cx);
                        })),
                )
                .child(
                    relation_button("quick-next-week", "Next week")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.quick_until_next_week(cx);
                        })),
                ),
        );

        {
            let details = cx.entity().clone();
            let year = self.view_year;
            let month = self.view_month;
            card = card.child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(HAIRLINE))
                    .child(crate::components::month_calendar(
                        year,
                        month,
                        self.until_date,
                        today_date(),
                        move |event, _window, cx| match event {
                            crate::components::CalendarEvent::SelectDay(date) => {
                                details.update(cx, |this, cx| {
                                    this.until_date = Some(date);
                                    this.show_time_picker = true;
                                    this.until_error = None;
                                    cx.notify();
                                });
                            }
                            crate::components::CalendarEvent::ShiftMonth(delta) => {
                                details.update(cx, |this, cx| {
                                    this.shift_view_month(delta, cx);
                                });
                            }
                        },
                    )),
            );
        }

        if self.show_time_picker {
            if let Some(date) = self.until_date {
                let hour = self.until_hour;
                let minute = self.until_minute;
                card = card.child(
                    div()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(rgb(HAIRLINE))
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe5e5e5))
                                .child(date.to_string()),
                        )
                        .child(
                            relation_button("hour-down", "-")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shift_hour(-1, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe5e5e5))
                                .child(format!("{hour:02}")),
                        )
                        .child(
                            relation_button("hour-up", "+")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shift_hour(1, cx);
                                })),
                        )
                        .child(div().text_sm().text_color(rgb(0xa3a3a3)).child(":"))
                        .child(
                            relation_button("minute-down", "-")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shift_minute(-15, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe5e5e5))
                                .child(format!("{minute:02}")),
                        )
                        .child(
                            relation_button("minute-up", "+")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shift_minute(15, cx);
                                })),
                        )
                        .child(
                            relation_button("set-until-datetime", "Set").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.commit_until_datetime(cx);
                                }),
                            ),
                        ),
                );
            }
        }

        card
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

    fn relationships_section(&mut self, task_id: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let mut section = div().v_flex().gap_2().mt_2().child(
            div()
                .text_base()
                .font_bold()
                .underline()
                .text_color(rgb(0xe5e5e5))
                .child("Relationships"),
        );

        if let Some(picker) = self.blocker_picker.clone() {
            section = section.child(
                div()
                    .v_flex()
                    .gap_2()
                    .child(picker)
                    .child(
                        relation_button("cancel-blocker-pick", "Cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.close_blocker_picker();
                                cx.notify();
                            }),
                        ),
                    ),
            );
        } else {
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
                                    if this.until_panel_open {
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
                    .when(self.until_panel_open, |this| {
                        this.child(self.until_card(task_id, cx))
                    }),
            );
        }

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

        if let Some(until) = self.selected.as_ref().and_then(|t| t.blocked_until) {
            section = section.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(0xe5e5e5))
                            .child(format!("Until {}", format_deadline(until))),
                    )
                    .child(
                        Button::new("clear-blocked-until")
                            .ghost()
                            .compact()
                            .label("×")
                            .tooltip("Clear time block")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clear_blocked_until(cx);
                            })),
                    ),
            );
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

/// Today's date in the system timezone.
fn today_date() -> jiff::civil::Date {
    jiff::Zoned::now().date()
}

/// Upcoming date with the given Monday-based weekday offset (0 = Monday,
/// 5 = Saturday). Stays on `from` when it already matches unless
/// `strict` is set, in which case it moves a full week ahead.
fn next_monday_offset_weekday(
    from: jiff::civil::Date,
    target: i8,
    strict: bool,
) -> jiff::civil::Date {
    let mut ahead = (target - from.weekday().to_monday_zero_offset() + 7) % 7;
    if strict && ahead == 0 {
        ahead = 7;
    }
    from.checked_add(jiff::ToSpan::days(ahead as i64))
        .expect("small date offset")
}

/// Parse "YYYY-MM-DD HH:MM" (or date only, midnight) in the system timezone.
/// Returns None for invalid input or times that are not in the future.
fn parse_blocked_until(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let datetime = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", raw)
        .or_else(|_| {
            jiff::civil::Date::strptime("%Y-%m-%d", raw).map(|date| date.at(0, 0, 0, 0))
        })
        .ok()?;
    let until = datetime
        .to_zoned(jiff::tz::TimeZone::system())
        .ok()?
        .timestamp()
        .as_second();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    (until > now).then_some(until as u64)
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                if let Some(branch) = &task.branch_name
                    && !branch.is_empty()
                {
                    details = details.child(field("Branch", branch.clone()));
                }
                details = details.child(self.relationships_section(task_id, cx));
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

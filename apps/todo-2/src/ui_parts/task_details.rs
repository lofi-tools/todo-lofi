use gpui::{
    AppContext, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::store::Store;
use crate::theme::{APP_BG, HAIRLINE};

#[derive(Clone)]
pub enum TaskDetailsEvent {
    Toggled { task_id: u64, done: bool },
    TitleCommitted { task_id: u64, title: String },
    PendingConfirmed { selected: Option<TaskWithMeta> },
    PendingCancelled,
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
        }
    }

    pub fn set_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        self.selected = Some(task);
        cx.notify();
    }

    pub fn selected_id(&self) -> Option<u64> {
        self.selected.as_ref().map(|task| task.id)
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
        self.selected = pending;
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

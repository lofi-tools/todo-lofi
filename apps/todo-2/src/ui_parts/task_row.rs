use gpui::{
    AppContext, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::Disableable;
use gpui_component::input::{Input, InputEvent, InputState};
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::store::Store;
use crate::theme::{APP_BG, HAIRLINE};

#[derive(Clone)]
pub enum TaskRowEvent {
    Selected(TaskWithMeta),
    EditStarted,
    EditEnded,
    TitleCommitted { task_id: u64, title: String },
}

pub struct TaskRow {
    task: TaskWithMeta,
    store: Store,
    selected_path: Vec<String>,
    selected_labels: Vec<String>,
    selected: bool,
    editing: bool,
    edit_input: Option<Entity<InputState>>,
    _edit_subscription: Option<Subscription>,
}

impl TaskRow {
    pub fn new(
        task: TaskWithMeta,
        store: Store,
        selected_path: Vec<String>,
        selected_labels: Vec<String>,
        selected: bool,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            task,
            store,
            selected_path,
            selected_labels,
            selected,
            editing: false,
            edit_input: None,
            _edit_subscription: None,
        }
    }

    pub fn set_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    pub fn task_id(&self) -> u64 {
        self.task.id
    }

    pub fn task_data(&self) -> TaskWithMeta {
        self.task.clone()
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn set_done(&mut self, done: bool, cx: &mut Context<Self>) {
        if self.task.done != done {
            self.task.task.done = done;
            cx.notify();
        }
    }

    pub fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        if self.task.title != title {
            self.task.task.title = title;
            cx.notify();
        }
    }

    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            return;
        }
        let title = self.task.title.clone();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&title, window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_edit(cx);
            }
        });
        self.edit_input = Some(input.clone());
        self._edit_subscription = Some(subscription);
        self.editing = true;
        cx.emit(TaskRowEvent::EditStarted);
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.edit_input.clone() else {
            return;
        };
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        if title.is_empty() {
            self.cancel_edit(cx);
            return;
        }
        let task_id = self.task.id;
        self.task.task.title = title.clone();
        self.editing = false;
        self.edit_input = None;
        self._edit_subscription = None;
        self.store.rename_task(task_id, title.clone(), cx).detach();
        cx.emit(TaskRowEvent::TitleCommitted { task_id, title });
        cx.notify();
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        self.editing = false;
        self.edit_input = None;
        self._edit_subscription = None;
        cx.emit(TaskRowEvent::EditEnded);
        cx.notify();
    }
}

impl EventEmitter<TaskRowEvent> for TaskRow {}

impl Render for TaskRow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {        let task_id = self.task.id;
        let done = self.task.done;
        let store = self.store.clone();
        let entity = cx.entity().clone();

        let visible_tags: Vec<_> = self
            .task
            .leaf_tags
            .iter()
            .filter(|t| !self.selected_path.contains(t) && !self.selected_labels.contains(t))
            .cloned()
            .collect();

        div()
            .id(("task", task_id))
            .h_flex()
            .h(px(44.))
            .when(self.editing, |this| this.h_auto().py_0p5())
            .items_center()
            .gap_3()
            .px_3()
            .rounded_md()
            .bg(if self.selected {
                rgb(0x3a3a3a)
            } else {
                rgb(APP_BG)
            })
            .hover(|s| {
                s.bg(if self.selected {
                    rgb(0x444444)
                } else {
                    rgb(0x2a2a2a)
                })
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                cx.emit(TaskRowEvent::Selected(this.task.clone()));
            }))
            .child(
                Checkbox::new(("checkbox", task_id))
                    .with_size(px(22.))
                    .checked(done)
                    .disabled(self.task.blocked && !done)
                    .on_click(move |new_done, _window, cx| {
                        let store = store.clone();
                        let entity = entity.clone();
                        let new_done = *new_done;
                        cx.spawn(async move |cx| {
                            if let Err(e) = store.toggle_task_done(task_id, new_done, cx).await {
                                tracing::error!(?e, "Failed toggle_task_done");
                            }
                            entity.update(cx, |this, cx| {
                                this.task.task.done = new_done;
                                cx.notify();
                            });
                        })
                        .detach();
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .gap_0p5()
            .child(if let Some(input) = self.edit_input.clone() {
                div().id(("task-title-edit", task_id)).pt_1().child(
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
                    .id(("task-title", task_id))
                    .text_base()
                    .text_color(if done { rgb(0x666666) } else { rgb(0xe5e5e5) })
                    .when(done, |this| this.line_through())
                    .child(self.task.title.clone())
                    .on_click(cx.listener(|this, event, window, cx| {
                        if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                        {
                            cx.stop_propagation();
                            this.begin_edit(window, cx);
                        }
                    }))
            })
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .children(visible_tags.into_iter().map(|tag| {
                                div()
                                    .text_size(px(10.))
                                    .px(px(4.))
                                    .rounded(px(2.))
                                    .bg(rgb(0x2a2a2a))
                                    .text_color(rgb(0xa3a3a3))
                                    .child(format!("#{tag}"))
                            })),
                    ),
            )
    }
}

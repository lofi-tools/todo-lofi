use crate::task_store::TaskStore;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, RenderOnce, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
};
use std::collections::HashSet;

#[derive(IntoElement)]
struct PlusRow {
    input_state: Entity<InputState>,
    editing_index: Entity<Option<usize>>,
    view: Entity<TaskList>,
    insert_at: usize,
}

impl PlusRow {
    fn new(
        input_state: Entity<InputState>,
        editing_index: Entity<Option<usize>>,
        view: Entity<TaskList>,
        insert_at: usize,
    ) -> Self {
        Self {
            input_state,
            editing_index,
            view,
            insert_at,
        }
    }
}

impl RenderOnce for PlusRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_editing = *self.editing_index.read(cx) == Some(self.insert_at);
        let insert_at = self.insert_at;
        let view = self.view.clone();

        div()
            .group("plus-row")
            .h_flex()
            .items_center()
            .relative()
            .when(is_editing, |this| this.gap_4())
            .when(!is_editing, |this| this.h(px(12.)).mt(-px(6.)).mb(-px(6.)))
            .child(
                div()
                    .w_8()
                    .h_flex()
                    .justify_center()
                    .when(!is_editing, |this| {
                        this.opacity(0.0)
                            .group_hover("plus-row", |s| s.opacity(1.0))
                    })
                    .child(
                        Button::new(format!("insert-{}", insert_at))
                            .ghost()
                            .p_0()
                            .size_6()
                            .label("+")
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.editing_index = Some(insert_at);
                                    this.input_state.update(cx, |state, cx| {
                                        state.set_value("", window, cx);
                                        state.focus(window, cx);
                                    });
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .when(is_editing, |this| {
                        this.child(Input::new(&self.input_state).cleanable(true))
                    })
                    .when(!is_editing, |this| {
                        this.opacity(0.0)
                            .group_hover("plus-row", |s| s.opacity(1.0))
                            .h_px()
                            .bg(cx.theme().border.opacity(0.3))
                    }),
            )
    }
}

#[derive(IntoElement)]
struct TaskItemRow {
    view: Entity<TaskList>,
    task_id: u64,
    title: String,
    completed: bool,
    tags: Vec<String>,
}

impl TaskItemRow {
    fn new(
        view: Entity<TaskList>,
        task_id: u64,
        title: String,
        completed: bool,
        tags: Vec<String>,
    ) -> Self {
        Self {
            view,
            task_id,
            title,
            completed,
            tags,
        }
    }
}

impl RenderOnce for TaskItemRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let task_id = self.task_id;
        let is_completed = self.completed;
        let view = self.view.clone();
        let tags = self.tags.clone();

        div()
            .h_flex()
            .items_center()
            .gap_4()
            .py_0p5()
            .pl_2()
            .ml(px(8.))
            .rounded_md()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
            .when(is_completed, |this| this.opacity(0.4))
            .child(
                crate::components::checkbox::Checkbox::new(format!("check-{}", task_id))
                    .checked(is_completed)
                    .with_size(Pixels::from(22.))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            let _ = this.toggle_task(task_id, cx);
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .child(
                        div()
                            .text_base()
                            .when(is_completed, |this| {
                                this.text_color(cx.theme().muted_foreground)
                            })
                            .child(self.title.clone()),
                    )
                    .child(div().h_flex().gap_2().children(tags.into_iter().map(|t| {
                        div()
                            .text_xs()
                            .px_1()
                            .rounded_sm()
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("#{}", t))
                    }))),
            )
    }
}

pub struct TaskList {
    pub selected_tag: Entity<Option<String>>,
    pub task_store: Entity<TaskStore>,
    pub editing_index: Option<usize>,
    pub input_state: Entity<InputState>,
}

impl TaskList {
    fn save_task(&mut self, cx: &mut Context<Self>) {
        let task_store = self.task_store.read(cx);
        let title = self
            .input_state
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string();
        if !title.is_empty()
            && let Some(index) = self.editing_index
        {
            let selected_tag = self.selected_tag.read(cx).clone();
            let mut tags = Vec::new();
            if let Some(ref tag) = selected_tag {
                tags.push(tag.clone());
                if let Ok(ancestors) = task_store.ancestors_of(tag) {
                    tags.extend(ancestors);
                }
            }
            let _ = task_store.insert_task(index, &title, tags);
            self.editing_index = None;
            cx.notify();
        }
    }
    fn toggle_task(&mut self, id: u64, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let task_store = self.task_store.read(cx);
        task_store.toggle_task(id)?;
        Ok(())
    }
}

impl Render for TaskList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        cx.subscribe(&self.input_state, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                this.save_task(cx);
            }
        })
        .detach();

        let view = cx.entity().clone();
        let selected_tag = self.selected_tag.read(cx);
        let task_store = self.task_store.read(cx);

        let filtered_tasks = match selected_tag.as_deref() {
            Some(tag) => task_store.tasks_for_tag(tag).unwrap_or_default(),
            None => task_store.tasks().unwrap_or_default(),
        };

        div()
            .on_action(cx.listener(
                |this, _action: &gpui_component::input::Escape, _window, cx| {
                    this.editing_index = None;
                    cx.notify();
                },
            ))
            .v_flex()
            .relative()
            .child(PlusRow::new(
                self.input_state.clone(),
                cx.new(|_| self.editing_index),
                view.clone(),
                0,
            ))
            .children((0..=filtered_tasks.len()).flat_map(|i| {
                let task = filtered_tasks.get(i);

                let should_show = task.is_some();

                if !should_show {
                    return vec![].into_iter();
                }

                let mut elements: Vec<_> = Vec::new();

                if let Some(task) = task {
                    elements.push(
                        TaskItemRow::new(
                            view.clone(),
                            task.id,
                            task.title.clone(),
                            task.completed,
                            task.tags.clone(),
                        )
                        .into_any_element(),
                    );
                }

                elements.push(
                    PlusRow::new(
                        self.input_state.clone(),
                        cx.new(|_| self.editing_index),
                        view.clone(),
                        i + 1,
                    )
                    .into_any_element(),
                );

                elements.into_iter()
            }))
    }
}

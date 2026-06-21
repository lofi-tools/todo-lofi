use crate::task_store::TaskStore;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Render, RenderOnce, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{
    ActiveTheme, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    scroll::ScrollableElement,
};
use std::collections::HashSet;
use storage::prelude::TaskWithMeta;

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
        let view2 = self.view.clone();
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
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                view.update(cx, |this, cx| {
                    this.selected_task_id
                        .update(cx, |id, _| *id = Some(task_id));
                    cx.notify();
                });
            })
            .child(
                crate::components::checkbox::Checkbox::new(format!("check-{}", task_id))
                    .checked(is_completed)
                    .with_size(Pixels::from(22.))
                    .on_click(move |_, _, cx| {
                        view2.update(cx, |this, cx| {
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
    pub selected_task_id: Entity<Option<u64>>,
    pub task_store: Entity<TaskStore>,
    pub editing_index: Option<usize>,
    pub input_state: Entity<InputState>,
    pub excluded_tags: HashSet<String>,
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

    fn build_task_rows(
        &self,
        view: &Entity<TaskList>,
        filtered_tasks: &[TaskWithMeta],
        _cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let task_store = self.task_store.read(_cx);
        filtered_tasks
            .iter()
            .map(|task| {
                let display_tags: Vec<String> = task
                    .direct_tags
                    .iter()
                    .filter(|t| !self.excluded_tags.contains(t.as_str()))
                    .cloned()
                    .collect();
                TaskItemRow::new(
                    view.clone(),
                    task.id,
                    task.title.clone(),
                    task_store.is_completed(task.id),
                    display_tags,
                )
                .into_any_element()
            })
            .collect()
    }

    fn build_interleaved_plus_rows(
        &self,
        view: &Entity<TaskList>,
        filtered_tasks: &[TaskWithMeta],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let editing_index = cx.new(|_| self.editing_index);
        let task_rows = self.build_task_rows(view, filtered_tasks, cx);

        let mut elements = Vec::with_capacity(task_rows.len() * 2 + 1);
        elements.push(
            PlusRow::new(
                self.input_state.clone(),
                editing_index.clone(),
                view.clone(),
                0,
            )
            .into_any_element(),
        );

        let mut task_iter = task_rows.into_iter();
        for (i, _task) in filtered_tasks.iter().enumerate() {
            if let Some(row) = task_iter.next() {
                elements.push(row);
            }
            elements.push(
                PlusRow::new(
                    self.input_state.clone(),
                    editing_index.clone(),
                    view.clone(),
                    i + 1,
                )
                .into_any_element(),
            );
        }

        elements
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

        self.excluded_tags = match selected_tag.as_deref() {
            Some(tag) => {
                let mut excluded = HashSet::new();
                excluded.insert(tag.to_string());
                if let Ok(ancestors) = task_store.ancestors_of(tag) {
                    excluded.extend(ancestors);
                }
                excluded
            }
            None => HashSet::new(),
        };

        let filtered_tasks = match selected_tag.as_deref() {
            Some(tag) => task_store.tasks_for_tag(tag).unwrap_or_default(),
            None => task_store.tasks().unwrap_or_default(),
        };

        let title = match selected_tag.as_deref() {
            Some(tag) => format!("Tasks: {}", tag),
            None => "All Tasks".to_string(),
        };

        div()
            .flex_1()
            .v_flex()
            .p_8()
            .gap_6()
            .overflow_y_scrollbar()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .w_full()
                    .child(
                        div()
                            .text_3xl()
                            .font_bold()
                            .ml(px(16.))
                            .mb(px(16.))
                            .child(title),
                    )
                    .child(
                        div()
                            .on_action(cx.listener(
                                |this, _action: &gpui_component::input::Escape, _window, cx| {
                                    this.editing_index = None;
                                    cx.notify();
                                },
                            ))
                            .v_flex()
                            .children(self.build_interleaved_plus_rows(&view, &filtered_tasks, cx)),
                    ),
            )
    }
}

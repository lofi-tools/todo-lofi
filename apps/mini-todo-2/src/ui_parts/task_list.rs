use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription,
    Window, div, rgb,
};
use gpui_component::input::*;
use gpui_component::StyledExt;
use storage::task::TaskCreate;

use crate::store::Store;

pub struct TaskList {
    tasks: Vec<storage::Task>,
    input: Entity<InputState>,
    store: Store,
    needs_clear: bool,
    _insert_task: Option<gpui::Task<()>>,
    _subscription: Subscription,
}

impl TaskList {
    pub fn new(input: Entity<InputState>, store: Store, cx: &mut Context<Self>) -> Self {
        let input_clone = input.clone();
        let subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                if title.is_empty() {
                    return;
                }
                this.insert_task(title, cx);
            }
        });

        Self {
            tasks: Vec::new(),
            input,
            store,
            needs_clear: false,
            _insert_task: None,
            _subscription: subscription,
        }
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        let create_task = self
            .store
            .insert_task(TaskCreate::default().title(title), cx);

        self._insert_task = Some(cx.spawn(async move |this, cx| {
            let new_tasks = match create_task.await {
                Ok(new_tasks) => new_tasks,
                Err(e) => {
                    tracing::error!("Failed to insert task: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.tasks = new_tasks;
                this.needs_clear = true;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_tasks(&mut self, tasks: Vec<storage::Task>) {
        self.tasks = tasks;
    }
}

impl Render for TaskList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_clear {
            self.needs_clear = false;
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        let tasks = self.tasks.clone();

        div()
            .flex_1()
            .v_flex()
            .p_8()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .font_bold()
                    .text_color(rgb(0xe5e5e5ff))
                    .child("Mini Todo"),
            )
            .child(Input::new(&self.input))
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .gap_2()
                    .children(tasks.into_iter().map(|task| {
                        div()
                            .id(("task", task.id))
                            .h_flex()
                            .gap_3()
                            .py_1()
                            .px_3()
                            .rounded_md()
                            .hover(|s| s.bg(rgb(0x2a2a2aff)))
                            .child(
                                div()
                                    .text_base()
                                    .text_color(rgb(0xa3a3a3ff))
                                    .child(task.title.clone()),
                            )
                    })),
            )
    }
}

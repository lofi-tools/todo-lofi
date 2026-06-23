use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Styled, Window, div, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::store::Store;

pub struct TaskView {
    task: TaskWithMeta,
    store: Store,
    selected_path: Vec<String>,
}

impl TaskView {
    pub fn new(
        task: TaskWithMeta,
        store: Store,
        selected_path: Vec<String>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            task,
            store,
            selected_path,
        }
    }

    pub fn set_selected_path(&mut self, path: Vec<String>) {
        self.selected_path = path;
    }
}

impl Render for TaskView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let task_id = self.task.id;
        let done = self.task.done;
        let store = self.store.clone();
        let entity = cx.entity().clone();

        let visible_tags: Vec<_> = self
            .task
            .inferred_tags
            .iter()
            .filter(|t| !self.selected_path.contains(t))
            .cloned()
            .collect();

        div()
            .id(("task", task_id))
            .h_flex()
            .gap_3()
            .py_1()
            .px_3()
            .rounded_md()
            .hover(|s| s.bg(rgb(0x2a2a2a)))
            .child(
                Checkbox::new(("checkbox", task_id))
                    .with_size(px(22.))
                    .checked(done)
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
                    .child(
                        div()
                            .text_base()
                            .text_color(rgb(0xa3a3a3))
                            .child(self.task.title.clone()),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .children(visible_tags.into_iter().map(|tag| {
                                div()
                                    .text_xs()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(rgb(0x2a2a2a))
                                    .text_color(rgb(0xa3a3a3))
                                    .child(format!("#{tag}"))
                            })),
                    ),
            )
    }
}

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Styled, Window, div, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;

use crate::components::Checkbox;
use crate::store::Store;

pub struct TaskView {
    task: storage::Task,
    store: Store,
}

impl TaskView {
    pub fn new(task: storage::Task, store: Store, _cx: &mut Context<Self>) -> Self {
        Self { task, store }
    }
}

impl Render for TaskView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let task_id = self.task.id;
        let done = self.task.done;
        let store = self.store.clone();
        let entity = cx.entity().clone();

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
                    .with_size(gpui_component::Size::Small)
                    .checked(done)
                    .on_click(move |new_done, _window, cx| {
                        let store = store.clone();
                        let entity = entity.clone();
                        let new_done = *new_done;
                        cx.spawn(async move |cx| {
                            let _ = store.toggle_task_done(task_id, new_done, cx).await;
                            let _ = entity.update(cx, |this, cx| {
                                this.task.done = new_done;
                                cx.notify();
                            });
                        })
                        .detach();
                    }),
            )
            .child(
                div()
                    .text_base()
                    .text_color(rgb(0xa3a3a3))
                    .child(self.task.title.clone()),
            )
    }
}

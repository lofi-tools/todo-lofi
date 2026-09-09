use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, Styled, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::store::Store;

pub struct TaskRow {
    task: TaskWithMeta,
    store: Store,
    selected_path: Vec<String>,
}

impl TaskRow {
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
}

impl Render for TaskRow {
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
            .h(px(44.))
            .items_center()
            .gap_3()
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
                            .text_color(if done { rgb(0x666666) } else { rgb(0xe5e5e5) })
                            .when(done, |this| this.line_through())
                            .child(self.task.title.clone()),
                    )
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

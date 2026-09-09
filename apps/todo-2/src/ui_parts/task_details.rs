use gpui::{
    Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::store::Store;

#[derive(Clone)]
pub enum TaskDetailsEvent {
    Toggled { task_id: u64, done: bool },
}

pub struct TaskDetails {
    selected: Option<TaskWithMeta>,
    store: Store,
}

impl TaskDetails {
    pub fn new(store: Store, _cx: &mut Context<Self>) -> Self {
        Self {
            selected: None,
            store,
        }
    }

    pub fn set_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        self.selected = Some(task);
        cx.notify();
    }

    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
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
                            div()
                                .flex_1()
                                .text_xl()
                                .font_bold()
                                .text_color(if done { rgb(0x666666) } else { rgb(0xe5e5e5) })
                                .when(done, |this| this.line_through())
                                .child(task.title.clone()),
                        ),
                );
                details = details.child(header);
                if let Some(desc) = &task.description
                    && !desc.is_empty()
                {
                    details = details.child(field("Description", desc.clone()));
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
    }
}

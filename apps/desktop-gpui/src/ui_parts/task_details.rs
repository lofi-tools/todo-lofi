use crate::task_store::{TaskStore, UiTask};
use gpui::{App, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use gpui_component::{
    ActiveTheme, StyledExt,
    button::{Button, ButtonVariants},
};

pub struct TaskDetails {
    pub task_store: Entity<TaskStore>,
    pub selected_task_id: Entity<Option<u64>>,
}

impl TaskDetails {
    fn get_selected_task(&self, cx: &App) -> Option<UiTask> {
        let task_id = *self.selected_task_id.read(cx);
        let task_id = task_id?;
        let task_store = self.task_store.read(cx);
        task_store
            .tasks()
            .ok()?
            .into_iter()
            .find(|t| t.id == task_id)
    }
}

impl Render for TaskDetails {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let task = self.get_selected_task(cx);
        let selected_task_id = self.selected_task_id.clone();
        let muted_fg = cx.theme().muted_foreground;

        div()
            .relative()
            .h_full()
            .flex_1()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(gpui::rgba(0x2525_25ff))
            .children(task.map(|task| {
                let mut details = div().v_flex().gap_3();

                details = details.child(detail_row("Title", task.title.clone(), muted_fg));

                if let Some(ref desc) = task.description
                    && !desc.is_empty()
                {
                    details = details.child(detail_row("Description", desc.clone(), muted_fg));
                }

                if let Some(deadline) = task.deadline {
                    details = details.child(detail_row(
                        "Deadline",
                        format_deadline(deadline),
                        muted_fg,
                    ));
                }

                details = details.child(priority_section(&task, muted_fg));

                if !task.tags.is_empty() {
                    details = details.child(
                        div().v_flex().gap_1().child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(muted_fg)
                                .child("Tags"),
                        ).child(div().h_flex().gap_2().flex_wrap().children(
                            task.tags.iter().map(|t| {
                                div()
                                    .text_sm()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(cx.theme().muted)
                                    .text_color(muted_fg)
                                    .child(format!("#{}", t))
                            }),
                        )),
                    );
                }

                details = details.child(
                    div().v_flex().gap_1().child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted_fg)
                            .child("Status"),
                    ).child(div().text_sm().child(if task.completed {
                        "Completed"
                    } else {
                        "Open"
                    })),
                );

                div()
                    .h_full()
                    .v_flex()
                    .p_4()
                    .gap_4()
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_lg().font_bold().child("Task Details"))
                            .child(
                                Button::new("close-details")
                                    .ghost()
                                    .size_6()
                                    .label("×")
                                    .on_click(move |_, _, cx| {
                                        selected_task_id.update(cx, |id, _| *id = None);
                                    }),
                            ),
                    )
                    .child(details)
            }))
    }
}

fn detail_row(label: &str, value: String, muted_fg: gpui::Hsla) -> impl IntoElement {
    div()
        .v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(muted_fg)
                .child(label.to_string()),
        )
        .child(div().text_sm().child(value))
}

fn priority_section(task: &UiTask, muted_fg: gpui::Hsla) -> impl IntoElement {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let priority_score = compute_priority_score(task, now_secs);

    let deadline_factor = match task.deadline {
        None => 1.0,
        Some(dl) => {
            let diff = dl as f64 - now_secs as f64;
            86400.0_f64 / diff.max(1.0)
        }
    };

    div()
        .v_flex()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(muted_fg)
                .child("Priority"),
        )
        .child(
            div()
                .v_flex()
                .gap_1p5()
                .child(priority_score_row(priority_score, muted_fg))
                .child(sub_row(
                    "Importance",
                    &format!("{:.1}", task.importance_factor),
                    muted_fg,
                ))
                .child(sub_row(
                    "Urgency",
                    &format!("{:.1}", task.urgency_factor),
                    muted_fg,
                ))
                .child(sub_row(
                    "Deadline factor",
                    &format!("{:.2}", deadline_factor),
                    muted_fg,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted_fg.alpha(0.5))
                        .child(format!(
                            "score = importance × deadline_factor = {:.1} × {:.2}",
                            task.importance_factor, deadline_factor
                        )),
                ),
        )
}

fn priority_score_row(score: f64, muted_fg: gpui::Hsla) -> impl IntoElement {
    let (label, color) = if score >= 3.0 {
        ("High", gpui::hsla(0.97 / 360.0, 0.84, 0.60, 1.0))
    } else if score >= 1.5 {
        ("Medium", gpui::hsla(48.0 / 360.0, 0.92, 0.46, 1.0))
    } else {
        ("Low", muted_fg)
    };

    div()
        .h_flex()
        .items_center()
        .justify_between()
        .child(div().text_base().font_bold().child(format!("{:.2}", score)))
        .child(
            div()
                .text_xs()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .text_color(color)
                .child(label),
        )
}

fn sub_row(label: &str, value: &str, muted_fg: gpui::Hsla) -> impl IntoElement {
    div()
        .h_flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_xs()
                .text_color(muted_fg)
                .child(label.to_string()),
        )
        .child(div().text_sm().child(value.to_string()))
}

fn format_deadline(deadline: u64) -> String {
    use jiff::Timestamp;

    let now = Timestamp::now();
    let dl = Timestamp::from_second(deadline as i64).unwrap_or(now);
    let diff_secs = (dl.as_second() - now.as_second()).unsigned_abs();

    let weekday = dl.strftime("%a").to_string();
    let within_7_days = diff_secs <= 7 * 86400;
    let within_365_days = diff_secs <= 365 * 86400;

    if within_7_days {
        weekday
    } else if within_365_days {
        format!("{}, {}", weekday, dl.strftime("%B %-d"))
    } else {
        format!(
            "{}, {}, {}",
            weekday,
            dl.strftime("%B %-d"),
            dl.strftime("%Y")
        )
    }
}

fn compute_priority_score(task: &UiTask, now_secs: u64) -> f64 {
    let deadline_factor = match task.deadline {
        None => 1.0,
        Some(dl) => {
            let diff = dl as f64 - now_secs as f64;
            86400.0_f64 / diff.max(1.0)
        }
    };
    task.importance_factor * deadline_factor
}

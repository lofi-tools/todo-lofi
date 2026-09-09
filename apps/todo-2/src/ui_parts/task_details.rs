use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, px, rgb};
use gpui_component::StyledExt;
use storage::TaskWithMeta;

pub struct TaskDetails {
    selected: Option<TaskWithMeta>,
}

impl TaskDetails {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self { selected: None }
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let diff = deadline.saturating_sub(now);
    let days = diff / 86400;
    if deadline <= now {
        "overdue".to_string()
    } else if days == 0 {
        "today".to_string()
    } else if days == 1 {
        "tomorrow".to_string()
    } else if days < 7 {
        format!("in {days} days")
    } else {
        format!("in {} weeks", days / 7)
    }
}

impl Render for TaskDetails {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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
                let mut details = div().v_flex().gap_3();
                details = details.child(field("Title", task.title.clone()));
                details = details.child(field(
                    "Status",
                    if task.done { "Done".to_string() } else { "Open".to_string() },
                ));
                if let Some(desc) = &task.description
                    && !desc.is_empty()
                {
                    details = details.child(field("Description", desc.clone()));
                }
                if let Some(deadline) = task.deadline {
                    details = details.child(field("Deadline", format_deadline(deadline)));
                }
                if !task.leaf_tags.is_empty() {
                    let chips = div().h_flex().gap_1().flex_wrap().children(
                        task.leaf_tags.iter().map(|tag| {
                            div()
                                .text_size(px(10.))
                                .px(px(4.))
                                .rounded(px(2.))
                                .bg(rgb(0x2a2a2a))
                                .text_color(rgb(0xa3a3a3))
                                .child(format!("#{tag}"))
                        }),
                    );
                    details = details
                        .child(div().v_flex().gap_1().child(field_label("Tags")).child(chips));
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
            .h_full()
            .v_flex()
            .p_4()
            .gap_4()
            .border_l_1()
            .border_color(rgb(0x333333))
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xa3a3a3))
                    .child("Details"),
            )
            .child(body)
    }
}

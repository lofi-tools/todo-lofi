//! Modal for picking a task to block on, listing candidate blockers with
//! mouse selection and Esc to dismiss.

use gpui::{
    div, px, rgb, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt;

const CONTEXT: &str = "TaskPicker";

/// Dismiss action dispatched by Esc in the picker context.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = task_picker, no_json)]
pub struct Dismiss;

/// Register the picker's key bindings. Called once at startup.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(CONTEXT))]);
}

#[derive(Clone, Debug)]
pub enum TaskPickerEvent {
    /// The user picked a task (by id).
    Selected(u64),
    /// The user dismissed the modal (Esc).
    Dismissed,
}

pub struct TaskPicker {
    tasks: Vec<storage::Task>,
    focus_handle: FocusHandle,
}

impl TaskPicker {
    pub fn new(tasks: Vec<storage::Task>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            tasks,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_tasks(&mut self, tasks: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.tasks = tasks;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
    }
}

impl Focusable for TaskPicker {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<TaskPickerEvent> for TaskPicker {}

impl Render for TaskPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self
            .tasks
            .iter()
            .enumerate()
            .map(|(row_index, task)| {
                let task_id = task.id;
                div()
                    .id(gpui::ElementId::named_usize("task-option", row_index))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .hover(|s| s.bg(rgb(0x2a2a2a)))
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        cx.emit(TaskPickerEvent::Selected(task_id));
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(if task.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(task.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if task.done { "done" } else { "" }.to_string()),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_this, _: &Dismiss, _, cx| {
                cx.emit(TaskPickerEvent::Dismissed);
            }))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xa3a3a3))
                    .child("Blocked by"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .h(px(320.))
                    .overflow_y_scrollbar()
                    .children(rows),
            )
            .children(if self.tasks.is_empty() {
                Some(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa3a3a3))
                        .child("No tasks can block this task"),
                )
            } else {
                None
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x666666))
                    .child("Click to select · Esc cancel"),
            )
    }
}

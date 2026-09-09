//! Modal for picking a task to block on, with a fuzzy filter, keyboard
//! navigation (↑/↓/Enter/Esc) and mouse selection.

use gpui::{
    div, px, rgb, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, StatefulInteractiveElement,
    Styled, Window,
};
use gpui_component::input::{
    Input, InputEvent, InputState, MoveDown as InputMoveDown, MoveUp as InputMoveUp,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::Sizable;
use gpui_component::StyledExt;

const CONTEXT: &str = "TaskPicker";

/// Confirm the cursor row. Enter inside the filter input raises
/// `InputEvent::PressEnter` (handled via subscription); this binding covers
/// Enter pressed while focus is elsewhere in the picker.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = task_picker, no_json)]
pub struct SelectFocused;

/// Dismiss action dispatched by Esc in the picker context.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = task_picker, no_json)]
pub struct Dismiss;

/// Register the picker's key bindings. Called once at startup.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        KeyBinding::new("enter", SelectFocused, Some(CONTEXT)),
    ]);
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
    /// Indices into `tasks` matching the current filter, in rank order.
    visible: Vec<usize>,
    query: Entity<InputState>,
    cursor: usize,
    focus_handle: FocusHandle,
    _filter_subscription: gpui::Subscription,
}

impl TaskPicker {
    pub fn new(tasks: Vec<storage::Task>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let visible = (0..tasks.len()).collect();
        let query = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Filter tasks...", window, cx);
            state
        });
        let filter_subscription = cx.subscribe(&query, |this, _, event, cx| match event {
            InputEvent::Change => this.apply_filter(cx),
            InputEvent::PressEnter { .. } => this.select_current(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            tasks,
            visible,
            query,
            cursor: 0,
            focus_handle: cx.focus_handle(),
            _filter_subscription: filter_subscription,
        }
    }

    pub fn set_tasks(&mut self, tasks: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.tasks = tasks;
        self.apply_filter(cx);
    }

    /// Focus the fuzzy-filter input so typing filters immediately.
    pub fn focus_filter(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Subsequence fuzzy match with a simple rank: consecutive prefix matches
    /// score best, plain subsequence matches after that.
    fn rank(query: &str, name: &str) -> Option<usize> {
        if query.is_empty() {
            return Some(0);
        }
        let query = query.to_lowercase();
        let name_lower = name.to_lowercase();
        if let Some(prefix) = name_lower.strip_prefix(&query) {
            return Some(prefix.len());
        }
        let mut search = name_lower.char_indices().peekable();
        let mut matched: Vec<usize> = Vec::new();
        for q in query.chars() {
            loop {
                match search.next() {
                    Some((index, c)) if c == q => {
                        matched.push(index);
                        break;
                    }
                    Some(_) => continue,
                    None => return None,
                }
            }
        }
        let spread = matched.last().copied().unwrap_or(0) - matched.first().copied().unwrap_or(0);
        Some(name_lower.len() + spread)
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().to_string();
        let mut ranked: Vec<(usize, usize)> = self
            .tasks
            .iter()
            .enumerate()
            .filter_map(|(index, task)| Self::rank(&query, &task.title).map(|score| (index, score)))
            .collect();
        ranked.sort_by_key(|(_, score)| *score);
        self.visible = ranked.into_iter().map(|(index, _)| index).collect();
        self.cursor = 0;
        cx.notify();
    }

    fn select_current(&mut self, cx: &mut Context<Self>) {
        if let Some(&index) = self.visible.get(self.cursor) {
            cx.emit(TaskPickerEvent::Selected(self.tasks[index].id));
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let count = self.visible.len() as isize;
        let next = (self.cursor as isize + delta).rem_euclid(count);
        self.cursor = next as usize;
        cx.notify();
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
            .visible
            .iter()
            .enumerate()
            .map(|(row_index, &task_index)| {
                let task = self.tasks[task_index].clone();
                let is_cursor = row_index == self.cursor;
                div()
                    .id(gpui::ElementId::named_usize("task-option", row_index))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(if is_cursor {
                        rgb(0x2a2a2a)
                    } else {
                        rgb(0x1e1e1e)
                    })
                    .hover(|s| s.bg(rgb(0x2a2a2a)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cursor = row_index;
                        this.select_current(cx);
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
                cx.stop_propagation();
                cx.emit(TaskPickerEvent::Dismissed);
            }))
            .on_action(cx.listener(|this, _: &InputMoveUp, _, cx| {
                this.move_cursor(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &InputMoveDown, _, cx| {
                this.move_cursor(1, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFocused, _, cx| {
                this.select_current(cx);
            }))
            .flex()
            .flex_col()
            .gap_3()
            .child(Input::new(&self.query).with_size(gpui_component::Size::Medium))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .h(px(320.))
                    .overflow_y_scrollbar()
                    .children(rows),
            )
            .children(if self.visible.is_empty() {
                Some(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa3a3a3))
                        .child("No matching tasks"),
                )
            } else {
                None
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x666666))
                    .child("↑/↓ navigate · Enter select · Esc cancel"),
            )
    }
}

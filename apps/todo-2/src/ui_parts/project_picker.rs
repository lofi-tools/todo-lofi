//! Modal for picking a local git-repo project to tag, with a fuzzy filter,
//! keyboard navigation (↑/↓/Enter/Esc) and mouse selection.

use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
    div, px, rgb,
};
use gpui_component::input::{
    Input, InputEvent, InputState, MoveDown as InputMoveDown, MoveUp as InputMoveUp,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::Sizable;

use crate::projects::Project;

const CONTEXT: &str = "ProjectPicker";

/// A no-op action dispatched by Enter in the picker context. Enter inside the
/// filter input raises `InputEvent::PressEnter` (handled via subscription);
/// this binding swallows Enter pressed while focus is elsewhere in the
/// picker so it doesn't bubble to the dialog's own Enter handling.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = project_picker, no_json)]
pub struct SelectFocused;

/// Dismiss action dispatched by Esc in the picker context.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = project_picker, no_json)]
pub struct Cancel;

/// Register the picker's key bindings. Called once at startup, before the
/// first picker is created. Bindings live in the `ProjectPicker` context so
/// they only match while the picker modal has focus. `up`/`down` reuse the
/// input's own MoveUp/MoveDown actions (bound to arrow keys already by
/// gpui-component) so the arrows work both in the filter input (cursor
/// moves) and on the list (handled below) depending on focus; Enter raises
/// the input's `PressEnter` event, which the parent handles.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
        KeyBinding::new("enter", SelectFocused, Some(CONTEXT)),
    ]);
}

#[derive(Clone, Debug)]
pub enum ProjectPickerEvent {
    /// The user picked a project (Enter or click).
    Selected(Project),
    /// The user dismissed the modal (Esc).
    Dismissed,
}

pub struct ProjectPicker {
    projects: Vec<Project>,
    /// Indices into `projects` matching the current filter, in rank order.
    visible: Vec<usize>,
    query: Entity<InputState>,
    cursor: usize,
    focus_handle: FocusHandle,
    _filter_subscription: gpui::Subscription,
}

impl ProjectPicker {
    pub fn new(projects: Vec<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let visible = (0..projects.len()).collect();
        let query = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Filter projects...", window, cx);
            state
        });
        let filter_subscription = cx.subscribe(&query, |this, _, event, cx| {
            match event {
                InputEvent::Change => this.apply_filter(cx),
                // Enter inside the filter field confirms the focused row,
                // matching the keyboard contract of the picker itself.
                InputEvent::PressEnter { .. } => this.select_current(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            }
        });
        Self {
            projects,
            visible,
            query,
            cursor: 0,
            focus_handle: cx.focus_handle(),
            _filter_subscription: filter_subscription,
        }
    }

    /// Subsequence fuzzy match with a simple rank: consecutive prefix matches
    /// score best, plain subsequence matches after that. Good enough for
    /// narrowing a directory list by typing a few letters.
    fn rank(query: &str, name: &str) -> Option<usize> {
        if query.is_empty() {
            return Some(0);
        }
        let query = query.to_lowercase();
        let name_lower = name.to_lowercase();
        if let Some(prefix) = name_lower.strip_prefix(&query) {
            return Some(prefix.len());
        }
        // Plain subsequence check; rank by spread of matched chars.
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
        let spread = matched.last().copied().unwrap_or(0)
            - matched.first().copied().unwrap_or(0);
        Some(name_lower.len() + spread)
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        let mut ranked: Vec<(usize, usize)> = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(index, project)| {
                Self::rank(&query, &project.name).map(|score| (index, score))
            })
            .collect();
        ranked.sort_by_key(|(_, score)| *score);
        self.visible = ranked.into_iter().map(|(index, _)| index).collect();
        self.cursor = 0;
        cx.notify();
    }

    fn select_current(&mut self, cx: &mut Context<Self>) {
        if let Some(&index) = self.visible.get(self.cursor) {
            let project = self.projects[index].clone();
            cx.emit(ProjectPickerEvent::Selected(project));
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

    /// Focus the fuzzy-filter input so typing filters immediately. The
    /// picker's own focus handle only carries the key context; the input is
    /// what should receive keystrokes.
    pub fn focus_filter(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |state, cx| state.focus(window, cx));
    }
}

impl Focusable for ProjectPicker {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ProjectPickerEvent> for ProjectPicker {}

impl Render for ProjectPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;

        let rows = self
            .visible
            .iter()
            .enumerate()
            .map(|(row_index, &project_index)| {
                let project = self.projects[project_index].clone();
                let is_cursor = row_index == self.cursor;
                div()
                    .id(gpui::ElementId::named_usize("project-option", row_index))
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
                    .child(project.name.clone())
            })
            .collect::<Vec<_>>();

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_this, _: &Cancel, _, cx| {
                cx.emit(ProjectPickerEvent::Dismissed);
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
                    // The scrollable wrapper inherits the element's size
                    // (not max-height), so the list needs a fixed height to
                    // actually scroll instead of growing to fit its rows.
                    .h(px(320.))
                    .overflow_y_scrollbar()
                    .children(rows),
            )
            .children(if self.visible.is_empty() {
                Some(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa3a3a3))
                        .child("No matching projects"),
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

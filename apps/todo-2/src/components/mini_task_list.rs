use gpui::{
    App, ClickEvent, Div, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::StyledExt;
use std::rc::Rc;

/// One row of a mini task list: the look the details pane gives a coding
/// run's workflow items — a state glyph, the title, and a "done" tag once
/// the task is complete — shared by an expanded task row's subtasks and by
/// the details pane's subtask list.
#[derive(IntoElement)]
pub struct MiniTaskItem {
    id: u64,
    title: SharedString,
    done: bool,
    /// A blocked task cannot be started yet: its glyph dims and stops
    /// taking clicks.
    blocked: bool,
    /// Ticking the task by clicking its state glyph; the new state is
    /// passed in. Without it the glyph is a plain marker.
    on_toggle: Option<Rc<dyn Fn(bool, &mut Window, &mut App)>>,
    /// Selecting the task by clicking its title.
    on_select: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl MiniTaskItem {
    pub fn new(id: u64, title: impl Into<SharedString>, done: bool) -> Self {
        Self {
            id,
            title: title.into(),
            done,
            blocked: false,
            on_toggle: None,
            on_select: None,
        }
    }

    pub fn blocked(mut self, blocked: bool) -> Self {
        self.blocked = blocked;
        self
    }

    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    pub fn on_select(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MiniTaskItem {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            title,
            done,
            blocked,
            on_toggle,
            on_select,
        } = self;
        let glyph_color = if done {
            0x6b6b6b
        } else if blocked {
            0x4f4f4f
        } else {
            0x8a8a8a
        };
        let glyph = div()
            .id(("mini-task-glyph", id))
            .text_xs()
            .text_color(rgb(glyph_color))
            .child(if done { "☑" } else { "☐" });
        let glyph = match on_toggle.filter(|_| !blocked) {
            // The glyph is the row's affordance, so a mini task list needs
            // no checkbox of its own.
            Some(on_toggle) => glyph.cursor_pointer().on_click(move |_: &ClickEvent, window, cx| {
                cx.stop_propagation();
                on_toggle(!done, window, cx);
            }),
            None => glyph,
        };
        let title = div()
            .id(("mini-task-title", id))
            .flex_1()
            .min_w_0()
            .text_sm()
            .text_color(if done { rgb(0x666666) } else { rgb(0xe5e5e5) })
            .child(title);
        let title = match on_select {
            Some(on_select) => title.cursor_pointer().on_click(
                move |event: &ClickEvent, window, cx| {
                    // Only the first click of a double click selects, so
                    // the row behind keeps its own double-click meaning.
                    if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 1) {
                        cx.stop_propagation();
                        on_select(event, window, cx);
                    }
                },
            ),
            None => title,
        };
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py(px(3.))
            .rounded_md()
            .child(glyph)
            .child(title)
            .when(done, |this| {
                this.child(div().text_xs().text_color(rgb(0x737373)).child("done"))
            })
    }
}

/// A mini task list: [`MiniTaskItem`]s stacked with the details pane's list
/// spacing. Callers add their own indent.
pub fn mini_task_list(items: impl IntoIterator<Item = MiniTaskItem>) -> Div {
    div().v_flex().gap_1().children(items)
}

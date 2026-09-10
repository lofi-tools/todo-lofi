use gpui::{
    App, AppContext, AnyElement, ClickEvent, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::FluentBuilder, px, rgb, svg,
};
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::Disableable;
use gpui_component::input::{Input, InputEvent, InputState};
use storage::TaskWithMeta;

use crate::components::Checkbox;
use crate::store::Store;
use crate::theme::{APP_BG, HAIRLINE};

#[derive(Clone)]
pub enum TaskRowEvent {
    Selected(TaskWithMeta),
    EditStarted,
    EditEnded,
    TitleCommitted { task_id: u64, title: String },
    DoneToggled { task_id: u64, done: bool },
}

/// One level of a "task blocks X (which blocks Y)" chain, rendered inline
/// after an arrow next to the blocker's title.
#[derive(Clone)]
pub struct ChainNode {
    pub task: TaskWithMeta,
    pub nested: Vec<ChainNode>,
}

/// What a task row displays about the tasks its task blocks.
pub struct RowBlocking {
    /// The tasks rendered inline (arrow + grayed title) after the
    /// blocker's title.
    pub blocked: Vec<ChainNode>,
    /// Every task the row's task exclusively blocks, shown via the
    /// "blocks N" chip and its expandable list.
    pub blocks: Vec<TaskWithMeta>,
}

pub struct TaskRow {
    task: TaskWithMeta,
    /// What this row's task blocks (inline chain + blocks-N list).
    blocking: RowBlocking,
    store: Store,
    selected_path: Vec<String>,
    selected_labels: Vec<String>,
    selected: bool,
    editing: bool,
    /// True while a completed task is jumping to the bottom of the list;
    /// clicks on the row are ignored so nothing lands mid-animation.
    locked: bool,
    /// Whether the "blocks N" chip is expanded to show the blocked tasks.
    blocks_expanded: bool,
    /// Subtask progress (done, total), shown right of the task's tags.
    subtask_progress: Option<(u64, u64)>,
    edit_input: Option<Entity<InputState>>,
    _edit_subscription: Option<Subscription>,
}

impl TaskRow {
    pub fn new(
        task: TaskWithMeta,
        blocking: RowBlocking,
        store: Store,
        selected_path: Vec<String>,
        selected_labels: Vec<String>,
        selected: bool,
        subtask_progress: Option<(u64, u64)>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            task,
            blocking,
            store,
            selected_path,
            selected_labels,
            selected,
            editing: false,
            locked: false,
            blocks_expanded: false,
            subtask_progress,
            edit_input: None,
            _edit_subscription: None,
        }
    }

    pub fn set_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    pub fn task_id(&self) -> u64 {
        self.task.id
    }

    pub fn task_data(&self) -> TaskWithMeta {
        self.task.clone()
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn set_done(&mut self, done: bool, cx: &mut Context<Self>) {
        if self.task.done != done {
            self.task.task.done = done;
            cx.notify();
        }
    }

    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        if self.locked != locked {
            self.locked = locked;
            cx.notify();
        }
    }

    /// Update the blocked flag of a task shown in this row's chain or
    /// "blocks N" list (e.g. the blocker completed, so the dependant is no
    /// longer blocked).
    pub fn set_chain_blocked(&mut self, task_id: u64, blocked: bool, cx: &mut Context<Self>) {
        fn update_chain(nodes: &mut [ChainNode], task_id: u64, blocked: bool) -> bool {
            for node in nodes {
                if node.task.id == task_id {
                    node.task.blocked = blocked;
                    return true;
                }
                if update_chain(&mut node.nested, task_id, blocked) {
                    return true;
                }
            }
            false
        }
        let changed = update_chain(&mut self.blocking.blocked, task_id, blocked)
            || self
                .blocking
                .blocks
                .iter_mut()
                .any(|task| if task.id == task_id {
                    task.blocked = blocked;
                    true
                } else {
                    false
                });
        if changed {
            cx.notify();
        }
    }

    pub fn toggle_blocks(&mut self, cx: &mut Context<Self>) {
        self.blocks_expanded = !self.blocks_expanded;
        cx.notify();
    }

    pub fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        if self.task.title != title {
            self.task.task.title = title;
            cx.notify();
        }
    }

    pub fn set_task_data(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        self.task = task;
        cx.notify();
    }

    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            return;
        }
        let title = self.task.title.clone();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&title, window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_edit(cx);
            }
        });
        self.edit_input = Some(input.clone());
        self._edit_subscription = Some(subscription);
        self.editing = true;
        cx.emit(TaskRowEvent::EditStarted);
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.edit_input.clone() else {
            return;
        };
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        if title.is_empty() {
            self.cancel_edit(cx);
            return;
        }
        let task_id = self.task.id;
        self.task.task.title = title.clone();
        self.editing = false;
        self.edit_input = None;
        self._edit_subscription = None;
        self.store.rename_task(task_id, title.clone(), cx).detach();
        cx.emit(TaskRowEvent::TitleCommitted { task_id, title });
        cx.notify();
    }

    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        self.editing = false;
        self.edit_input = None;
        self._edit_subscription = None;
        cx.emit(TaskRowEvent::EditEnded);
        cx.notify();
    }
}

impl EventEmitter<TaskRowEvent> for TaskRow {}

impl Render for TaskRow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {        let task_id = self.task.id;
        let done = self.task.done;
        let store = self.store.clone();
        let entity = cx.entity().clone();
        let entity_for_checkbox = entity.clone();

        let visible_tags: Vec<_> = self
            .task
            .leaf_tags
            .iter()
            .filter(|t| !self.selected_path.contains(t) && !self.selected_labels.contains(t))
            .cloned()
            .collect();
        // Blocked tasks are grayed out while unworkable; completed tasks
        // gray out the moment they are done (before jumping to the bottom).
        let muted = done || self.task.blocked;
        let locked = self.locked;
        // The row grows with its content: fixed 44px when collapsed, auto
        // height when editing or when the "blocks N" list is expanded so
        // the extra rows get their own vertical space.
        let expanded = self.blocks_expanded && self.blocking.blocks.len() > 1;

        div()
            .id(("task", task_id))
            .h_flex()
            .when(self.editing || expanded, |this| this.h_auto().py_0p5())
            .when(!self.editing && !expanded, |this| this.h(px(44.)))
            .items_center()
            .when(expanded, |this| this.items_start())
            .gap_3()
            .px_3()
            .rounded_md()
            .opacity(if muted { 0.55 } else { 1.0 })
            .bg(if self.selected {
                rgb(0x3a3a3a)
            } else {
                rgb(APP_BG)
            })
            .hover(|s| {
                s.bg(if self.selected {
                    rgb(0x444444)
                } else {
                    rgb(0x2a2a2a)
                })
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if !this.locked {
                    cx.emit(TaskRowEvent::Selected(this.task.clone()));
                }
            }))
            .child(
                Checkbox::new(("checkbox", task_id))
                    .with_size(px(22.))
                    .checked(done)
                    .disabled(self.task.blocked && !done)
                    .on_click(move |new_done, _window, cx| {
                        if locked {
                            return;
                        }
                        let store = store.clone();
                        let entity = entity_for_checkbox.clone();
                        let new_done = *new_done;
                        cx.spawn(async move |cx| {
                            if let Err(e) = store.toggle_task_done(task_id, new_done, cx).await {
                                tracing::error!(?e, "Failed toggle_task_done");
                            }
                            entity.update(cx, |this, cx| {
                                this.task.task.done = new_done;
                                cx.emit(TaskRowEvent::DoneToggled {
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
                    .v_flex()
                    .gap_0p5()
            .child(if let Some(input) = self.edit_input.clone() {
                div().id(("task-title-edit", task_id)).pt_1().child(
                    Input::new(&input)
                        .small()
                        .appearance(false)
                        .bg(rgb(APP_BG))
                        .border_1()
                        .border_color(rgb(HAIRLINE))
                        .rounded_md(),
                )
            } else {
                div()
                    .id(("task-title", task_id))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("task-title-text", task_id))
                            .text_base()
                            .text_color(if done { rgb(0x666666) } else { rgb(0xe5e5e5) })
                            .when(done, |this| this.line_through())
                            .child(self.task.title.clone())
                            .on_click(cx.listener(|this, event, window, cx| {
                                if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                {
                                    cx.stop_propagation();
                                    this.begin_edit(window, cx);
                                }
                            })),
                    )
                    .children(
                        self.blocking
                            .blocked
                            .iter()
                            .flat_map(chain_children),
                    )
                    .when(self.blocking.blocks.len() > 1, |this| {
                        this.child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_0p5()
                                // Same arrow as the chain titles, tinted
                                // to match the chip text; kept outside the
                                // badge so it is not part of the pill.
                                .child(arrow_svg(0xa3a3a3))
                                .child(
                                    div()
                                        .id(("blocks-chip", task_id))
                                        .h_flex()
                                        .items_center()
                                        .gap_0p5()
                                        .px(px(4.))
                                        .rounded(px(2.))
                                        .bg(rgb(0x2a2a2a))
                                        .text_color(rgb(0xa3a3a3))
                                        .text_size(px(10.))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_blocks(cx);
                                        }))
                                        .child(format!("blocks {}", self.blocking.blocks.len()))
                                        .child(if self.blocks_expanded { "▾" } else { "▸" }),
                                ),
                        )
                    })
            })
            .when(self.blocks_expanded && self.blocking.blocks.len() > 1, |this| {
                this                    .child(
                        div()
                            .id(("blocks-list", task_id))
                            .v_flex()
                            .pl_4()
                            .children(self.blocking.blocks.iter().map(|task| {
                                blocked_title(task.clone(), &entity)
                            })),
                    )
            })
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
                            }))
                            .when_some(self.subtask_progress, |this, (done, total)| {
                                // Subtask progress, right of the tags.
                                this.child(
                                    div()
                                        .h_flex()
                                        .items_center()
                                        .gap_0p5()
                                        .text_size(px(10.))
                                        .text_color(rgb(0xa3a3a3))
                                        .child(
                                            svg()
                                                .size(px(12.))
                                                .data(SUBTASK_SVG)
                                                .text_color(rgb(0xa3a3a3)),
                                        )
                                        .child(format!("{done}/{total}")),
                                )
                            }),
                    ),
            )
    }
}

/// Right-pointing arrow separating a blocker from the task it blocks.
/// Rendered as an alpha mask tinted by the given text color (same
/// technique as the repeat icon), so it matches the blocked task's shade.
/// The tint must be set on the svg element itself: gpui's `Svg` only
/// paints when its own style has a text color, it does not inherit the
/// parent's.
fn arrow_svg(color: u32) -> impl IntoElement {
    div()
        .h_flex()
        .items_center()
        .mx_1()
        .child(svg().size_3().data(ARROW_SVG).text_color(rgb(color)))
}

/// The text color of a blocked/blocking task title: the muted gray when
/// it is blocked or done, the lighter gray otherwise.
fn blocked_color(muted: bool) -> u32 {
    if muted { 0x666666 } else { 0xcccccc }
}

/// Render a chain node inline after the blocker's title: arrow + grayed
/// title, recursing into nested nodes. The titles are display-only: a
/// click anywhere on the row selects the main task, not the blocked one.
fn chain_children(node: &ChainNode) -> Vec<AnyElement> {
    let mut children: Vec<AnyElement> = Vec::new();
    let muted = node.task.done || node.task.blocked;
    let color = blocked_color(muted);
    children.push(arrow_svg(color).into_any_element());
    let task = node.task.clone();
    children.push(
        div()
            .id(("chain-title", task.id))
            .text_base()
            .text_color(rgb(color))
            .when(task.done, |this| this.line_through())
            .child(task.title.clone())
            .into_any_element(),
    );
    for nested in &node.nested {
        children.extend(chain_children(nested));
    }
    children
}

/// One expanded "blocks N" entry: arrow + grayed title.
fn blocked_title(task: TaskWithMeta, row_entity: &Entity<TaskRow>) -> impl IntoElement {
    let muted = task.done || task.blocked;
    let color = blocked_color(muted);
    let row_entity = row_entity.clone();
    let row_entity_for_click = row_entity.clone();
    div()
        .id(("blocked-row", task.id))
        .h_flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_0p5()
        .rounded_md()
        .child(arrow_svg(color))
        .child(
            div()
                .id(("blocked-title", task.id))
                .text_base()
                .text_color(rgb(color))
                .when(task.done, |this| this.line_through())
                .child(task.title.clone())
                .cursor_pointer()
                .on_click(move |event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 1) {
                        cx.stop_propagation();
                        row_entity_for_click.update(cx, |_row, cx| {
                            cx.emit(TaskRowEvent::Selected(task.clone()));
                        });
                    }
                }),
        )
}

/// Lucide `arrow-right`, drawn with an opaque stroke so the alpha-mask
/// rendering tints it with the element's text color.
const ARROW_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>"##;

/// Todoist's subtask glyph: three bars, each indented further right (the
/// classic "indent list" icon). Opaque stroke so the alpha-mask rendering
/// tints it with the element's text color.
const SUBTASK_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6h16"/><path d="M8 12h12"/><path d="M12 18h8"/></svg>"##;

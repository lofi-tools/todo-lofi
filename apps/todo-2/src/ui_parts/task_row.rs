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
    /// The row's N/M counter was clicked; the list decides which row (if
    /// any) stays expanded, since only one may be expanded at a time.
    SubtasksToggled { task_id: u64 },
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
    /// Whether the "N/M" counter is expanded to show the subtasks.
    subtasks_expanded: bool,
    /// The task's direct subtasks, collapsed under the row: the first one
    /// renders inline right of the title, the rest behind the expandable
    /// "N/M" counter.
    subtasks: Vec<TaskWithMeta>,
    edit_input: Option<Entity<InputState>>,
    _edit_subscription: Option<Subscription>,
}

impl TaskRow {
    pub fn new(
        task: TaskWithMeta,
        blocking: RowBlocking,
        subtasks: Vec<TaskWithMeta>,
        store: Store,
        selected_path: Vec<String>,
        selected_labels: Vec<String>,
        selected: bool,
        subtasks_expanded: bool,
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
            subtasks_expanded,
            subtasks,
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

    /// The list view owns which row is expanded (only one at a time); this
    /// just mirrors that decision onto this row's render.
    pub fn set_subtasks_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if self.subtasks_expanded != expanded {
            self.subtasks_expanded = expanded;
            cx.notify();
        }
    }

    /// Update the done flag of a subtask shown under this row (e.g. the
    /// subtask was completed in the details panel), keeping the N/M
    /// counter fresh without a DB round-trip.
    pub fn set_subtask_done(&mut self, task_id: u64, done: bool, cx: &mut Context<Self>) {
        let changed = self.subtasks.iter_mut().any(|task| {
            if task.id == task_id {
                task.task.done = done;
                true
            } else {
                false
            }
        });
        if changed {
            cx.notify();
        }
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
        // The checkbox closure needs its own copy: it is `move`, so it
        // would otherwise consume `store` before the subtask list uses it.
        let store_for_checkbox = store.clone();
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
        // …unless the row carries a live (unblocked, undone) chain title:
        // it must read as white as any pending row, so the row stays at
        // full opacity and the done/blocked titles rely on their explicit
        // gray + strikethrough instead.
        let live_chain = chain_live(&self.blocking.blocked)
            || self
                .blocking
                .blocks
                .iter()
                .any(|task| !task.done && !task.blocked);
        let locked = self.locked;
        // The row is a vertical stack: a fixed-height header carrying the
        // whole unexpanded row (checkbox, titles, tags), with the expanded
        // "blocks N" and subtask lists below it — so expanding never moves
        // the header's checkbox or title.

        div()
            .id(("task", task_id))
            .v_flex()
            .gap_1()
            .px_3()
            .rounded_md()
            .opacity(if muted && !live_chain { 0.55 } else { 1.0 })
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
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .when(self.editing, |this| this.h_auto().py_0p5())
                    .when(!self.editing, |this| this.h(px(44.)))
                    .child(
                        Checkbox::new(("checkbox", task_id))
                    .with_size(px(22.))
                    .checked(done)
                    .disabled(self.task.blocked && !done)
                    .on_click(move |new_done, _window, cx| {
                        if locked {
                            return;
                        }
                        let store = store_for_checkbox.clone();
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
                    .when_some(self.subtasks.first().cloned(), |this, task| {
                        // The first subtask's title sits directly right of
                        // the main task; display-only, like chain titles.
                        this.child(subtask_inline(task))
                    })
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
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .when_some(start_chip(&self.task), |this, chip| this.child(chip))
                            .children(visible_tags.into_iter().map(|tag| {
                        div()
                            .text_size(px(10.))
                            .px(px(4.))
                            .rounded(px(2.))
                            .bg(rgb(0x2a2a2a))
                            .text_color(rgb(0xa3a3a3))
                            .child(format!("#{tag}"))
                    }))
                    .when(!self.subtasks.is_empty(), |this| {
                        let done =
                            self.subtasks.iter().filter(|task| task.done).count();
                        let total = self.subtasks.len();
                        // Subtask progress, right of the tags; the
                        // N/M counter expands/collapses the subtask
                        // list under the row (one row at a time).
                        this.child(
                            div()
                                .id(("subtask-progress", task_id))
                                .h_flex()
                                .items_center()
                                .gap_0p5()
                                .text_size(px(10.))
                                .text_color(rgb(0xa3a3a3))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    let task_id = this.task.id;
                                    cx.emit(TaskRowEvent::SubtasksToggled { task_id });
                                }))
                                .child(subtask_icon(0xa3a3a3))
                                .child(format!("{done}/{total}"))
                                .child(if self.subtasks_expanded { "▾" } else { "▸" }),
                        )
                            }),
                    )
                )
            )
            // Expanded lists sit below the header, stacked vertically in
            // the row — never beside it — so the header never moves.
            .when(self.blocks_expanded && self.blocking.blocks.len() > 1, |this| {
                this.child(
                    div()
                        .h_flex()
                        .gap_3()
                        .child(div().w(px(22.)).flex_none())
                        .child(
                            div()
                                .flex_1()
                                .v_flex()
                                .child(
                                    div()
                                        .id(("blocks-list", task_id))
                                        .v_flex()
                                        .pl_4()
                                        .children(self.blocking.blocks.iter().map(|task| {
                                            blocked_title(task.clone(), &entity)
                                        })),
                                ),
                        ),
                )
            })
            .when(self.subtasks_expanded && !self.subtasks.is_empty(), |this| {
                this.child(
                    div()
                        .flex_1()
                        .v_flex()
                        .child(
                            div()
                                .id(("subtask-list", task_id))
                                .v_flex()
                                .children(self.subtasks.iter().map(|task| {
                                    subtask_title(task.clone(), &entity, &store)
                                })),
                        ),
                )
            })
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
        .child(svg().size_3().data(ARROW_SVG).text_color(rgb(color)))
}

/// "Starts …" chip for not-yet-doable rows: relative day plus time in
/// the system timezone ("Starts today, 5:50 PM"). `None` for doable
/// tasks, which show no chip.
fn start_chip(task: &TaskWithMeta) -> Option<impl IntoElement> {
    let start = task.blocked_until?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let label = start_day_label(start, now_secs)?;
    Some(
        div()
            .text_size(px(10.))
            .px(px(4.))
            .rounded(px(2.))
            .bg(rgb(0x2a2a2a))
            .text_color(rgb(0xa3a3a3))
            .child(format!("Starts {label}")),
    )
}

/// Relative start label ("today, 5:50 PM") for a future epoch, or `None`
/// when already started.
fn start_day_label(start: u64, now_secs: u64) -> Option<String> {
    if start <= now_secs {
        return None;
    }
    let zone = jiff::tz::TimeZone::system();
    let day = jiff::Timestamp::from_second(start as i64)
        .ok()?
        .to_zoned(zone.clone());
    let today = jiff::Timestamp::from_second(now_secs as i64)
        .ok()?
        .to_zoned(zone)
        .date();
    let date = day.date();
    let days_out = date.duration_since(today).as_secs() / 86400;
    let day_label = if days_out <= 0 {
        "today".to_string()
    } else if days_out == 1 {
        "tomorrow".to_string()
    } else if days_out < 7 {
        day.strftime("%a").to_string()
    } else {
        day.strftime("%b %-d").to_string()
    };
    let time = day.strftime("%-I:%M %p").to_string();
    Some(format!("{day_label}, {time}"))
}

/// The text color of a blocked/blocking task title: the muted gray when
/// it is blocked or done, the normal pending white otherwise (same as
/// top-level pending rows).
fn blocked_color(muted: bool) -> u32 {
    if muted { 0x666666 } else { 0xe5e5e5 }
}

/// The subtask glyph (three indented bars), tinted by the given color.
/// The tint must be set on the svg element itself (see `arrow_svg`).
fn subtask_icon(color: u32) -> impl IntoElement {
    div()
        .h_flex()
        .items_center()
        .child(svg().size(px(12.)).data(SUBTASK_SVG).text_color(rgb(color)))
}

/// The first subtask's title, rendered inline right of the main task.
/// Display-only: a click anywhere on the row selects the main task.
fn subtask_inline(task: TaskWithMeta) -> impl IntoElement {
    let muted = task.done || task.blocked;
    let color = blocked_color(muted);
    div()
        .id(("subtask-title", task.id))
        .h_flex()
        .items_center()
        .gap_0p5()
        .text_base()
        .text_color(rgb(color))
        .when(task.done, |this| this.line_through())
        .child(subtask_icon(color))
        .child(task.title.clone())
}

/// One expanded subtask row: checkbox + title, indented under the main
/// task. The checkbox ticks the subtask complete; clicking the title
/// selects the subtask.
fn subtask_title(task: TaskWithMeta, row_entity: &Entity<TaskRow>, store: &Store) -> impl IntoElement {
    let muted = task.done || task.blocked;
    let color = blocked_color(muted);
    let task_id = task.id;
    let done = task.done;
    let row_entity = row_entity.clone();
    let row_entity_for_checkbox = row_entity.clone();
    let row_entity_for_click = row_entity.clone();
    let store = store.clone();
    div()
        .id(("subtask-row", task.id))
        .h_flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_0p5()
        .rounded_md()
        .child(
            Checkbox::new(("subtask-checkbox", task_id))
                .with_size(px(16.))
                .checked(done)
                .disabled(task.blocked && !done)
                .on_click(move |new_done, _window, cx| {
                    // Ticking a subtask must not also select the main task.
                    cx.stop_propagation();
                    let store = store.clone();
                    let entity = row_entity_for_checkbox.clone();
                    let new_done = *new_done;
                    cx.spawn(async move |cx| {
                        if let Err(e) = store.toggle_task_done(task_id, new_done, cx).await {
                            tracing::error!(?e, "Failed toggle_task_done");
                        }
                        entity.update(cx, |_this, cx| {
                            cx.emit(TaskRowEvent::DoneToggled { task_id, done: new_done });
                            cx.notify();
                        });
                    })
                    .detach();
                }),
        )
        .child(
            div()
                .id(("subtask-title-text", task.id))
                .text_sm()
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

/// True when a chain contains a live title (neither done nor blocked):
/// such a row must render at full opacity so the live title reads as
/// white as any pending row.
fn chain_live(nodes: &[ChainNode]) -> bool {
    nodes
        .iter()
        .any(|node| (!node.task.done && !node.task.blocked) || chain_live(&node.nested))
}

/// Render a chain node inline after the blocker's title: arrow + grayed
/// title, recursing into nested nodes. The titles are display-only: a
/// click anywhere on the row selects the main task, not the blocked one.
fn chain_children(node: &ChainNode) -> Vec<AnyElement> {    let mut children: Vec<AnyElement> = Vec::new();
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

#[cfg(test)]
mod tests {
    use super::start_day_label;

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn test_start_day_label_relative_days() {
        let now = now_secs();
        assert!(start_day_label(now - 10, now).is_none());
        assert!(start_day_label(now, now).is_none());

        let today = start_day_label(now + 3600, now).unwrap();
        assert!(today.starts_with("today, "), "got {today}");

        // Tomorrow 10am local is unambiguously "tomorrow".
        let zone = jiff::tz::TimeZone::system();
        let tomorrow_10am = jiff::Timestamp::from_second(now as i64)
            .unwrap()
            .to_zoned(zone.clone())
            .date()
            .at(10, 0, 0, 0)
            .to_zoned(zone)
            .unwrap()
            .timestamp()
            .as_second() as u64
            + 86400;
        let tomorrow = start_day_label(tomorrow_10am, now).unwrap();
        assert!(tomorrow.starts_with("tomorrow, "), "got {tomorrow}");

        let far = start_day_label(now + 10 * 86400, now).unwrap();
        assert!(!far.starts_with("today"), "got {far}");
        assert!(!far.starts_with("tomorrow"), "got {far}");
    }
}

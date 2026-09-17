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

use crate::components::{Checkbox, MiniTaskItem, mini_task_list};
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
    /// The row's own content changed height (an expanded list opened or
    /// closed, inline editing started or ended). The list measures rows
    /// once, so it has to be told to measure this one again.
    LayoutChanged { task_id: u64 },
}

/// One level of a "task blocks X (which blocks Y)" chain, rendered inline
/// on the title row as "then X".
#[derive(Clone)]
pub struct ChainNode {
    pub task: TaskWithMeta,
    pub nested: Vec<ChainNode>,
}

/// What a task row displays about the tasks its task blocks.
pub struct RowBlocking {
    /// The tasks rendered inline ("then" + grayed title) after the
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
    /// renders inline on the title row, just right of the "N/M" counter,
    /// the rest behind that counter's expandable list.
    subtasks: Vec<TaskWithMeta>,
    edit_input: Option<Entity<InputState>>,
    _edit_subscription: Option<Subscription>,
    /// The GitHub issue this task is synced from, when it is issue-backed:
    /// the row shows its number as the source badge (spec §5.8).
    issue: Option<storage::TaskIssue>,
    _issue_load: Option<gpui::Task<()>>,
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
        cx: &mut Context<Self>,
    ) -> Self {
        let task_id = task.id;
        let store = store.clone();
        let load_store = store.clone();
        let mut this = Self {
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
            issue: None,
            _issue_load: None,
        };
        // The issue lives in a link table rather than on the task, so the
        // badge arrives a tick later instead of blocking the row's render.
        let fetch = load_store.github_issue_for_task(task_id, cx);
        this._issue_load = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(issue) => {
                this.update(cx, |this, cx| {
                    this.issue = issue;
                    this._issue_load = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => tracing::error!("Failed to fetch the GitHub issue: {e}"),
        }));
        this
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

    /// Update the row's own blocked flag: its last open blocker completed,
    /// or was reopened. The row re-grays (or comes back) without a reload,
    /// and its checkbox follows.
    pub fn set_blocked(&mut self, blocked: bool, cx: &mut Context<Self>) {
        if self.task.blocked != blocked {
            self.task.blocked = blocked;
            cx.notify();
        }
    }

    /// Replace what this row shows about the tasks it blocks. The list
    /// re-derives its rows when a done-toggle changes which tasks are
    /// visible: a dependant that just stood on its own row leaves this
    /// row's chain and "blocks N" chip before the next reload.
    pub fn set_blocking(&mut self, blocking: RowBlocking, cx: &mut Context<Self>) {
        self.blocking = blocking;
        cx.emit(TaskRowEvent::LayoutChanged {
            task_id: self.task.id,
        });
        cx.notify();
    }

    pub fn toggle_blocks(&mut self, cx: &mut Context<Self>) {
        self.blocks_expanded = !self.blocks_expanded;
        cx.emit(TaskRowEvent::LayoutChanged {
            task_id: self.task.id,
        });
        cx.notify();
    }

    /// The list view owns which row is expanded (only one at a time); this
    /// just mirrors that decision onto this row's render.
    pub fn set_subtasks_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if self.subtasks_expanded != expanded {
            self.subtasks_expanded = expanded;
            cx.emit(TaskRowEvent::LayoutChanged {
                task_id: self.task.id,
            });
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
        // Reloaded data can add or drop the metadata sub-row, so the row's
        // height is no longer known to the list.
        cx.emit(TaskRowEvent::LayoutChanged {
            task_id: self.task.id,
        });
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
        cx.emit(TaskRowEvent::LayoutChanged { task_id: self.task.id });
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
        cx.emit(TaskRowEvent::LayoutChanged { task_id });
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
        cx.emit(TaskRowEvent::LayoutChanged {
            task_id: self.task.id,
        });
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
        // The sub-row under the title carries only start/tag/blocks
        // metadata: with none of it the sub-row is not rendered and the
        // header shrinks to the title's own height.
        let has_subrow = start_chip(&self.task).is_some()
            || !visible_tags.is_empty()
            || self.blocking.blocks.len() > 1;
        // A parent with outstanding subtasks reads as delegated: its own
        // title grays out so the subtask inline is the readable half.
        let has_pending_subtasks = self.subtasks.iter().any(|task| !task.done);
        // Blocked tasks are grayed out while unworkable; completed tasks
        // gray out the moment they are done (before jumping to the bottom).
        let muted = done || self.task.blocked;
        let title_muted = done || has_pending_subtasks;
        // …unless the row carries a live (unblocked, undone) dependant in
        // its chain or blocks list: that task is still workable, so the row
        // stays at full opacity and done/blocked titles rely on their
        // explicit gray + strikethrough instead.
        let live_chain = chain_live(&self.blocking.blocked)
            || self
                .blocking
                .blocks
                .iter()
                .any(|task| !task.done && !task.blocked);
        let locked = self.locked;
        // The row is a vertical stack: a header carrying the whole
        // unexpanded row (checkbox, title, sub-row), with the expanded
        // "blocks N" and subtask lists below it — so expanding never moves
        // the header's checkbox or title.

        div()
            .id(("task", task_id))
            .group("task-row")
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
                    // The header grows with whatever it holds, so the
                    // sub-row stays inside it instead of bleeding past the
                    // row's highlight. Only a row that actually shows a
                    // sub-row holds the full height; a bare title stays
                    // short.
                    .py(px(2.))
                    .when(has_subrow, |this| this.min_h(px(38.)))
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
                            .text_color(if title_muted { rgb(0x666666) } else { rgb(0xe5e5e5) })
                            .when(done, |this| this.line_through())
                            .child(self.task.title.clone())
                            .on_click(cx.listener(|this, event, window, cx| {
                                if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                {
                                    cx.stop_propagation();
                                    // App-owned read-only rows do not enter
                                    // edit mode; the hover lock explains why.
                                    if this.task.is_managed_read_only() {
                                        return;
                                    }
                                    this.begin_edit(window, cx);
                                }
                            })),
                    )
                    // Source badge: the issue number an imported task came
                    // from, so a synced task is identifiable at a glance.
                    .when_some(self.issue.clone(), |this, issue| {
                        this.child(
                            div()
                                .id(("task-issue", task_id))
                                .h_flex()
                                .items_center()
                                .px(px(4.))
                                .py(px(1.))
                                .rounded(px(2.))
                                .bg(rgb(0x2a2a2a))
                                .text_size(px(10.))
                                .text_color(if issue.state.tombstoned {
                                    rgb(0x737373)
                                } else {
                                    rgb(0xa3a3a3)
                                })
                                .child(
                                    match issue
                                        .state
                                        .url
                                        .clone()
                                        .filter(|_| !issue.state.tombstoned)
                                    {
                                        Some(url) => div()
                                            .id(("task-issue-open", task_id))
                                            .cursor_pointer()
                                            .hover(|this| this.text_color(rgb(0xe5e5e5)))
                                            .child(format!("#{}", issue.issue.number))
                                            .on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                crate::todoist_auth::open_browser(&url);
                                            })
                                            .into_any_element(),
                                        None => div()
                                            .child(format!("#{}", issue.issue.number))
                                            .into_any_element(),
                                    },
                                ),
                        )
                    })
                    // Ownership marker: appears only on hover so the list
                    // stays uncluttered, and names the owning app.
                    .when(self.task.is_managed_read_only(), |this| {
                        let label = self
                            .task
                            .managed_label
                            .clone()
                            .unwrap_or_else(|| "an app".to_string());
                        this.child(
                            div()
                                .id(("task-lock", task_id))
                                .h_flex()
                                .items_center()
                                .gap_1()
                                .opacity(0.0)
                                .group_hover("task-row", |s| s.opacity(1.0))
                                .child(
                                    svg()
                                        .size(px(12.))
                                        .data(LOCK_SVG)
                                        .text_color(rgb(0xa3a3a3)),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(0xa3a3a3))
                                        .child(format!("Managed by {label}")),
                                ),
                        )
                    })
                    .children(
                        self.blocking
                            .blocked
                            .iter()
                            .flat_map(chain_children),
                    )
                    .when(!self.subtasks.is_empty(), |this| {
                        let done = self.subtasks.iter().filter(|task| task.done).count();
                        let total = self.subtasks.len();
                        // The N/M counter expands/collapses the subtask
                        // list under the row (one row at a time); the next
                        // subtask's title sits right of it, display-only
                        // like the chain titles.
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
                    })
                    .when_some(self.subtasks.first().cloned(), |this, task| {
                        this.child(subtask_inline(task, &store, &entity))
                    })
            })
                    // The sub-row under the title: start chip, tags and the
                    // "blocks N" chip. Only the chip is clickable; the rest
                    // is display-only. A row with nothing to put here skips
                    // the sub-row entirely so the header stays short.
                    .when(has_subrow, |this| {
                        this.child(
                            div()
                                .h_flex()
                                .items_center()
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
                                .when(self.blocking.blocks.len() > 1, |this| {
                                    this.child(
                                        div()
                                            .h_flex()
                                            .items_center()
                                            .gap_0p5()
                                            // The arrow is tinted to match the
                                            // chip text; kept outside the
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
                                                    .child(format!(
                                                        "blocks {}",
                                                        self.blocking.blocks.len()
                                                    ))
                                                    .child(if self.blocks_expanded { "▾" } else { "▸" }),
                                            ),
                                    )
                                }),
                        )
                    })
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
                // The subtasks read like the workflow items the details
                // pane shows for a run's steps, indented under the title
                // like the expanded "blocks N" list beside it.
                this.child(
                    div()
                        .h_flex()
                        .gap_3()
                        .child(div().w(px(22.)).flex_none())
                        .child(
                            mini_task_list(self.subtasks.iter().map(|task| {
                                let task = task.clone();
                                let task_id = task.id;
                                let done = task.done;
                                let entity_for_toggle = entity.clone();
                                let entity_for_select = entity.clone();
                                let store = store.clone();
                                MiniTaskItem::new(task_id, task.title.clone(), done)
                                    // A blocked subtask cannot be ticked
                                    // off yet.
                                    .blocked(task.blocked && !done)
                                    .on_toggle(move |new_done, _window, cx| {
                                        let store = store.clone();
                                        let entity = entity_for_toggle.clone();
                                        cx.spawn(async move |cx| {
                                            if let Err(e) = store
                                                .toggle_task_done(task_id, new_done, cx)
                                                .await
                                            {
                                                tracing::error!(?e, "Failed toggle_task_done");
                                            }
                                            entity.update(cx, |_row, cx| {
                                                cx.emit(TaskRowEvent::DoneToggled {
                                                    task_id,
                                                    done: new_done,
                                                });
                                                cx.notify();
                                            });
                                        })
                                        .detach();
                                    })
                                    .on_select(move |_event, _window, cx| {
                                        entity_for_select.update(cx, |_row, cx| {
                                            cx.emit(TaskRowEvent::Selected(task.clone()));
                                        });
                                    })
                            }))
                            .id(("subtask-list", task_id))
                            .flex_1(),
                        ),
                )
            })
    }
}

/// Right-pointing arrow separating a blocker from the task it blocks in
/// the expanded "blocks N" list. Rendered as an alpha mask tinted by the
/// given text color (same technique as the repeat icon), so it matches
/// the blocked task's shade. The tint must be set on the svg element
/// itself: gpui's `Svg` only paints when its own style has a text color,
/// it does not inherit the parent's.
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

/// The sub-task glyph shown ahead of the "N/M" counter, tinted by the
/// given color. The tint must be set on the svg element itself (see
/// `arrow_svg`).
fn subtask_icon(color: u32) -> impl IntoElement {
    div()
        .h_flex()
        .items_center()
        .child(svg().size(px(12.)).data(SUBTASK_SVG).text_color(rgb(color)))
}

/// The first subtask's title, rendered inline on the title row right of
/// the N/M counter, smaller than the main title but larger than the row's
/// metadata. Its round checkbox mirrors the main one at px(14.) vs px(22.);
/// clicking it toggles the subtask, while the title itself stays
/// display-only and selects the main task.
fn subtask_inline(task: TaskWithMeta, store: &Store, row_entity: &Entity<TaskRow>) -> impl IntoElement {
    let muted = task.done || task.blocked;
    let color = blocked_color(muted);
    let subtask_id = task.id;
    let subtask_done = task.done;
    let subtask_blocked = task.blocked;
    let store = store.clone();
    let row_entity = row_entity.clone();
    div()
        .id(("subtask-inline", subtask_id))
        .h_flex()
        .items_center()
        .gap_1p5()
        .child(
            Checkbox::new(("subtask-check", subtask_id))
                .with_size(px(14.))
                .checked(subtask_done)
                .disabled(subtask_blocked && !subtask_done)
                .on_click(move |new_done, _window, cx| {
                    cx.stop_propagation();
                    let store = store.clone();
                    let row_entity = row_entity.clone();
                    let new_done = *new_done;
                    cx.spawn(async move |cx| {
                        if let Err(e) =
                            store.toggle_task_done(subtask_id, new_done, cx).await
                        {
                            tracing::error!(?e, "Failed toggle_task_done");
                        }
                        row_entity.update(cx, |_row, cx| {
                            cx.emit(TaskRowEvent::DoneToggled {
                                task_id: subtask_id,
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
                .id(("subtask-title", subtask_id))
                .text_size(px(12.))
                .text_color(rgb(color))
                .when(task.done, |this| this.line_through())
                .child(task.title.clone()),
        )
}

/// True when a chain contains a live title (neither done nor blocked):
/// such a row must render at full opacity so the dependant it hands over
/// to is not dimmed with the blocker.
fn chain_live(nodes: &[ChainNode]) -> bool {
    nodes
        .iter()
        .any(|node| (!node.task.done && !node.task.blocked) || chain_live(&node.nested))
}

/// Render one hop of a blocker chain inline after the blocker's title: the
/// word "then" followed by the blocked task's title, recursing into nested
/// hops so a chain reads as one sentence off the row's own title ("Write
/// the spec then Implement it then Ship"). The connector is the dimmest
/// text in the row so the task it hands over to stays the readable half of
/// the pair, and both stay at the small size of the row's metadata. The
/// titles are display-only: a click anywhere on the row selects the main
/// task, not the blocked one.
fn chain_children(node: &ChainNode) -> Vec<AnyElement> {
    let mut children: Vec<AnyElement> = Vec::new();
    let task = node.task.clone();
    children.push(
        div()
            .id(("chain-hop", task.id))
            .h_flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .text_size(px(10.))
                    .px(px(5.))
                    .py(px(0.))
                    .rounded(px(3.))
                    .bg(rgb(0x2a2a2a))
                    .text_color(rgb(0x666666))
                    .child("then"),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(0xa3a3a3))
                    .when(task.done, |this| this.line_through())
                    .child(task.title.clone()),
            )
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
/// rendering tints it with the element's text color. Also used by the app
/// header's history arrows (turned around for "back").
pub(crate) const ARROW_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>"##;

/// Lucide `lock`, drawn with an opaque stroke so the alpha-mask rendering
/// tints it with the element's text color (same technique as the arrows).
const LOCK_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>"##;

/// The sub-task glyph (a branch with a node hanging off it), vendored as
/// an asset: the app only ships the icons in the kit's `default-icons.txt`
/// list, so a bundled path would resolve to nothing and render blank.
const SUBTASK_SVG: &[u8] = include_bytes!("../../assets/icons/subtask.svg");

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

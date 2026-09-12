use gpui::{
    AnyElement, App, AppContext, BoxShadow, ClickEvent, Context, Div, Entity, EventEmitter,
    InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Subscription, Window, div, hsla, prelude::FluentBuilder, px, rgb, svg,
};
use gpui_component::Disableable;
use gpui_component::IconName;
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use storage::TaskWithMeta;
use storage::prelude::{CODING_PHASES, RunNote, RunStepView, RunView, run_notes};

use crate::components::Checkbox;
use crate::components::{DateTimePicker, DateTimePickerEvent};
use crate::store::Store;
use crate::theme::{APP_BG, CARD_BG, HAIRLINE, PANEL_HOVER};

use super::agent_pane::build_task_context;
use super::repeat_picker::{RepeatPicker, RepeatPickerEvent, repeat_label};
use super::task_picker::{TaskPicker, TaskPickerEvent};

/// Where a phase sits in the run, derived from its step rows. A re-spec cycle
/// leaves earlier steps done while a fresh one is open, so an open step wins
/// over a done one of the same node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhaseState {
    Pending,
    Active,
    Done,
}

fn phase_state(steps: &[RunStepView], node_id: &str) -> PhaseState {
    let mut seen = false;
    for step in steps.iter().filter(|step| step.node.id == node_id) {
        if !step.task.done {
            return PhaseState::Active;
        }
        seen = true;
    }
    if seen {
        PhaseState::Done
    } else {
        PhaseState::Pending
    }
}

/// The one step the run is waiting on, if any. Coding runs keep a single
/// open step per cycle.
fn current_step(steps: &[RunStepView]) -> Option<&RunStepView> {
    steps.iter().find(|step| !step.task.done)
}

/// Placeholder title for a coding phase that has not materialized yet, kept
/// in step with the seeded `coding-task` recipe's node titles so the roadmap
/// rows read like the real steps they preview.
fn phase_roadmap_title(phase: &str) -> &'static str {
    match phase {
        "interview" => "Interview & spec the feature",
        "spec" => "Approve the spec",
        "implement" => "Implement the feature",
        "review" => "Review & annotate",
        "merge" => "Merge the branch",
        _ => "Complete the next coding step",
    }
}

/// The UI's cycle counter: the run starts at round 1 and every rejection
/// opens a new one. Derived from the log rather than from the step rows,
/// because a rejection tombstones the previous cycle's steps.
fn round_number(notes: &[RunNote]) -> usize {
    1 + notes.iter().filter(|note| note.kind == "reject").count()
}

/// Assign each log entry its cycle (1-based) by counting rejections, then
/// return the log newest-first for display.
fn notes_by_round(notes: &[RunNote]) -> Vec<(usize, RunNote)> {
    let mut round = 1;
    let mut tagged = Vec::with_capacity(notes.len());
    for note in notes {
        tagged.push((round, note.clone()));
        if note.kind == "reject" {
            round += 1;
        }
    }
    tagged.reverse();
    tagged
}

/// The self-contained interview prompt the app drops into the agent pane for
/// the coding-run `interview` phases. It is fully expanded here (never the raw
/// `/interview` slash form): the pane's ACP agent is `opencode`, which does not
/// register an `interview` command and silently drops unknown `/`-prefixed
/// prompts, so the app sends the complete instruction as an ordinary prompt.
/// The final line is the request to interview (`Request to interview: …`).
const INTERVIEW_BASE_PROMPT: &str = "\
You are running an interview for a feature request. Your job is to gather context \
and ask clarifying questions before producing a detailed spec.

## Process

1. First, gather relevant context about the request — read files, search the \
   codebase, check existing docs — whatever helps you understand the current \
   state.
2. Then ask clarifying questions in several rounds. Ask about edge cases, \
   preferences, constraints, and design decisions — the things you cannot infer \
   on your own. Write the questions out in the conversation; there is no \
   question tool, so never wait for one.
3. When you have enough context, write a detailed spec file.

## Spec file output

- Write the spec to `./docs/spec/<slug>-spec.md` where `<slug>` is derived from \
   the request (a short kebab-case name).
- If the request doesn't suggest an obvious slug, use a sensible name in the \
   same `docs/spec/` location.
- The spec should be detailed: capture everything you learned during the \
   interview — requirements, constraints, decisions, open questions, and the \
   planned approach.
- Create the `docs/spec/` directory if it doesn't exist.
- If a `save_spec` tool is available, call it with the final spec so the app \
   stores it on the task.

## Final reply

- After writing the spec file, reply with a short summary plus the spec file \
   path (e.g. `Wrote spec to ./docs/spec/add-oauth-spec.md`).
- The summary is included even if the interview only produced a spec file.

## Request

Request to interview: ";

/// The prompt the app drops into the agent pane for a phase (§8.1 of the
/// coding workflow spec). The user edits and sends it; nothing is auto-sent.
fn phase_prompt(
    task: &TaskWithMeta,
    notes: &[RunNote],
    branch: Option<&str>,
    phase: &str,
) -> String {
    let context = build_task_context(task);
    match phase {
        "interview" => {
            let mut target = task.title.clone();
            if let Some(description) = task.description.as_deref()
                && !description.is_empty()
            {
                target.push_str(&format!("\n\n{description}"));
            }
            let round = round_number(notes);
            if round > 1 {
                let last_rejection = notes
                    .iter()
                    .filter(|note| note.kind == "reject")
                    .next_back();
                target.push_str(&format!("\n\nThis is round {round}."));
                if let Some(note) = last_rejection
                    && !note.body.trim().is_empty()
                {
                    target.push_str(&format!(" The last review rejected the result: {}", note.body));
                }
            }
            format!("{INTERVIEW_BASE_PROMPT}{target}")
        }
        "implement" => {
            let mut prompt = String::from("Implement the approved spec.\n\n");
            prompt.push_str(&context);
            if let Some(spec) = task.spec.as_deref().filter(|spec| !spec.is_empty()) {
                prompt.push_str(&format!("\n\nSpec:\n{spec}"));
            }
            let annotations: Vec<String> = notes
                .iter()
                .filter(|note| matches!(note.kind.as_str(), "annotation" | "reject"))
                .map(|note| format!("- {} ({}): {}", note.kind, note.phase, note.body))
                .collect();
            if !annotations.is_empty() {
                prompt.push_str(&format!("\n\nFeedback so far:\n{}", annotations.join("\n")));
            }
            let branch = branch.unwrap_or("the feature branch");
            prompt.push_str(&format!("\n\nWork on branch `{branch}`. Do not merge it."));
            prompt
        }
        "review" => format!(
            "{context}\n\nSummarise what you changed on `{}` and call `complete_phase`, then wait for my review notes.",
            branch.unwrap_or("the feature branch")
        ),
        "sub-interview" => {
            let mut target = task.title.clone();
            if let Some(description) = task.description.as_deref()
                && !description.is_empty()
            {
                target.push_str(&format!("\n\n{description}"));
            }
            format!("{INTERVIEW_BASE_PROMPT}{target}")
        }
        other => format!("{context}\n\nContinue the {other} phase."),
    }
}

/// How long after an outside mousedown closed a picker card before the
/// toggle button treats a click as a fresh open rather than the same click
/// that closed it (via `on_mouse_down_out`).
const OUTSIDE_CLOSE_IGNORE_WINDOW: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone)]
pub enum TaskDetailsEvent {
    Toggled {
        task_id: u64,
        done: bool,
    },
    TitleCommitted {
        task_id: u64,
        title: String,
    },
    PendingConfirmed {
        selected: Option<TaskWithMeta>,
    },
    PendingCancelled,
    SelectTask {
        task_id: u64,
    },
    /// Fresh DB state after a write, for syncing the task list row.
    TaskRefreshed(TaskWithMeta),
    /// A subtask was created; the task list reloads its current view.
    SubtaskCreated,
    /// A follow-up task was created; the task list reloads its current view.
    FollowUpCreated,
    /// A coding phase action changed run state; the task list and workflow
    /// panel reload (steps may have spawned or completed).
    CodingChanged,
    /// Put a composed phase prompt into the agent pane and switch to it. The
    /// user still edits and sends it.
    CodingLaunch {
        phase: String,
        prompt: String,
    },
}

/// A selection change that arrived while edits were unsaved. `Some` selects
/// a task, `None` deselects.
type PendingSelection = Option<TaskWithMeta>;

pub struct TaskDetails {
    selected: Option<TaskWithMeta>,
    store: Store,
    editing_title: bool,
    title_input: Option<Entity<InputState>>,
    _title_subscription: Option<Subscription>,
    editing_description: bool,
    description_input: Option<Entity<InputState>>,
    _description_subscription: Option<Subscription>,
    editing_tags: bool,
    tags_input: Option<Entity<InputState>>,
    _tags_subscription: Option<Subscription>,
    /// The tags currently shown as chips inside the tags editor.
    tag_draft: Vec<String>,
    /// True when the tags input should be replaced with a fresh empty one on
    /// the next render — defers the recreation so the subscriber callback does
    /// not need to touch `Window`.
    needs_tag_input_clear: bool,
    confirming: bool,
    pending: Option<PendingSelection>,
    blockers: Vec<storage::Task>,
    _blockers_fetch: Option<gpui::Task<()>>,
    until_picker: Option<Entity<DateTimePicker>>,
    _until_picker_subscription: Option<Subscription>,
    /// When the card was last closed by an outside mousedown. The toggle
    /// button ignores a click within a short window so the capture-phase
    /// close and the bubble-phase toggle don't cancel out.
    until_outside_closed_at: Option<std::time::Instant>,
    time_edit: Option<TimeEditInputs>,
    focus_time_edit: bool,
    blocker_picker: Option<Entity<TaskPicker>>,
    _blocker_picker_subscription: Option<Subscription>,
    /// Same as `until_outside_closed_at`, for the blocker picker card.
    blocker_outside_closed_at: Option<std::time::Instant>,
    after_tasks: Vec<storage::Task>,
    _after_fetch: Option<gpui::Task<()>>,
    after_picker: Option<Entity<TaskPicker>>,
    _after_picker_subscription: Option<Subscription>,
    /// Same as `blocker_outside_closed_at`, for the after-task picker card.
    after_outside_closed_at: Option<std::time::Instant>,
    /// Inline message when a relationship write fails, e.g. adding a link
    /// that would close a dependency cycle.
    link_error: Option<String>,
    /// True while the "+ subtasks" button shows an inline title input.
    adding_subtask: bool,
    subtask_input: Option<Entity<InputState>>,
    _subtask_subscription: Option<Subscription>,
    subtasks: Vec<storage::Task>,
    _subtasks_fetch: Option<gpui::Task<()>>,
    /// The selected task's parent (when it is a subtask), for the parent
    /// link in the details view.
    parent: Option<storage::Task>,
    _parent_fetch: Option<gpui::Task<()>>,
    /// True while the "+ follow-up task" button shows an inline title input.
    adding_follow_up: bool,
    follow_up_input: Option<Entity<InputState>>,
    _follow_up_subscription: Option<Subscription>,
    /// Tasks that this task blocks ("Linked to").
    blocking: Vec<storage::Task>,
    _blocking_fetch: Option<gpui::Task<()>>,
    repeat_picker: Option<Entity<RepeatPicker>>,
    _repeat_subscription: Option<Subscription>,
    /// Same as the other `*_outside_closed_at` markers, for the repeat card.
    repeat_outside_closed_at: Option<std::time::Instant>,
    /// The selected task's repeat template, shown as a "Repeats" field.
    repeat_template: Option<storage::RepeatTaskTemplate>,
    _repeat_template_fetch: Option<gpui::Task<()>>,
    /// The selected task's coding run (root or nested root), for the step list
    /// at the bottom of the panel.
    coding: Option<RunView>,
    /// Sub-tasks the model created *under* a phase step, keyed by step task id.
    step_subtasks: std::collections::HashMap<u64, Vec<TaskWithMeta>>,
    _coding_fetch: Option<gpui::Task<()>>,
    /// Inline reason a coding action could not run (missing directory, dirty
    /// tree, merge conflict, …).
    coding_error: Option<String>,
    /// Whether the run was already auto-started for the current selection, so
    /// selecting a feature task starts its coding run exactly once.
    coding_auto_started: bool,
    /// Whether the selected task sits in a directory-backed project (a
    /// `project:` tag or a tag with a configured directory). Only those
    /// tasks get a coding workflow at all.
    coding_directory_backed: bool,
    /// The reject-notes box is open (spec gate or review gate).
    coding_notes_open: bool,
    coding_notes_step: Option<u64>,
    coding_notes_input: Option<Entity<InputState>>,
    _coding_notes_subscription: Option<Subscription>,
    /// The manual-spec box is open (the fallback when no interview agent is
    /// wired up).
    coding_spec_open: bool,
    coding_spec_input: Option<Entity<InputState>>,
    _coding_spec_subscription: Option<Subscription>,
    /// The spec artifact is expanded.
    coding_spec_expanded: bool,
}

struct TimeEditInputs {
    hour: Entity<InputState>,
    minute: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl Clone for TimeEditInputs {
    fn clone(&self) -> Self {
        // Clones are for rendering only; subscriptions stay owned by the
        // original in `time_edit`.
        Self {
            hour: self.hour.clone(),
            minute: self.minute.clone(),
            _subscriptions: Vec::new(),
        }
    }
}

impl TaskDetails {
    pub fn new(store: Store, _cx: &mut Context<Self>) -> Self {
        Self {
            selected: None,
            store,
            editing_title: false,
            title_input: None,
            _title_subscription: None,
            editing_description: false,
            description_input: None,
            _description_subscription: None,
            editing_tags: false,
            tags_input: None,
            _tags_subscription: None,
            tag_draft: Vec::new(),
            needs_tag_input_clear: false,
            confirming: false,
            pending: None,
            blockers: Vec::new(),
            _blockers_fetch: None,
            until_picker: None,
            _until_picker_subscription: None,
            until_outside_closed_at: None,
            time_edit: None,
            focus_time_edit: false,
            blocker_picker: None,
            _blocker_picker_subscription: None,
            blocker_outside_closed_at: None,
            after_tasks: Vec::new(),
            _after_fetch: None,
            after_picker: None,
            _after_picker_subscription: None,
            after_outside_closed_at: None,
            link_error: None,
            adding_subtask: false,
            subtask_input: None,
            _subtask_subscription: None,
            subtasks: Vec::new(),
            _subtasks_fetch: None,
            parent: None,
            _parent_fetch: None,
            adding_follow_up: false,
            follow_up_input: None,
            _follow_up_subscription: None,
            blocking: Vec::new(),
            _blocking_fetch: None,
            repeat_picker: None,
            _repeat_subscription: None,
            repeat_outside_closed_at: None,
            repeat_template: None,
            _repeat_template_fetch: None,
            coding: None,
            step_subtasks: std::collections::HashMap::new(),
            _coding_fetch: None,
            coding_error: None,
            coding_auto_started: false,
            coding_directory_backed: false,
            coding_notes_open: false,
            coding_notes_step: None,
            coding_notes_input: None,
            _coding_notes_subscription: None,
            coding_spec_open: false,
            coding_spec_input: None,
            _coding_spec_subscription: None,
            coding_spec_expanded: false,
        }
    }

    pub fn set_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        self.apply_selected(task, cx);
    }

    fn apply_selected(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) {
        let parent_id = task.parent_id;
        let task_id = task.id;
        let auto_start = task.node_id.is_none() && task.parent_id.is_none() && !task.done;
        let fetch = self.store.list_blockers(task.id, cx);
        let after_fetch = self.store.list_after(task.id, cx);
        let subtasks_fetch = self.store.list_subtasks(task.id, cx);
        let blocking_fetch = self.store.list_blocking_tasks(task.id, cx);
        self.selected = Some(task);
        self.blockers = Vec::new();
        self.after_tasks = Vec::new();
        self.subtasks = Vec::new();
        self.blocking = Vec::new();
        self.parent = None;
        self.repeat_template = None;
        self.link_error = None;
        self.coding = None;
        self.step_subtasks = std::collections::HashMap::new();
        self.coding_error = None;
        self.coding_auto_started = false;
        self.coding_directory_backed = false;
        self.coding_spec_expanded = false;
        self.close_coding_notes();
        self.close_coding_spec();
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        self.close_time_edit();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.abandon_tags();
        self._blockers_fetch = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(blockers) => {
                this.update(cx, |this, cx| {
                    this.blockers = blockers;
                    this._blockers_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch blockers: {e}");
            }
        }));
        self._after_fetch = Some(cx.spawn(async move |this, cx| match after_fetch.await {
            Ok(after_tasks) => {
                this.update(cx, |this, cx| {
                    this.after_tasks = after_tasks;
                    this._after_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch after tasks: {e}");
            }
        }));
        self._subtasks_fetch = Some(cx.spawn(async move |this, cx| match subtasks_fetch.await {
            Ok(subtasks) => {
                this.update(cx, |this, cx| {
                    this.subtasks = subtasks;
                    this._subtasks_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch subtasks: {e}");
            }
        }));
        if let Some(parent_id) = parent_id {
            let parent_fetch = self.store.get_task(parent_id, cx);
            self._parent_fetch = Some(cx.spawn(async move |this, cx| match parent_fetch.await {
                Ok(parent) => {
                    this.update(cx, |this, cx| {
                        this.parent = Some(parent);
                        this._parent_fetch = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to fetch parent task: {e}");
                }
            }));
        }
        self._blocking_fetch = Some(cx.spawn(async move |this, cx| match blocking_fetch.await {
            Ok(blocking) => {
                this.update(cx, |this, cx| {
                    this.blocking = blocking;
                    this._blocking_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch linked tasks: {e}");
            }
        }));
        let repeat_fetch = self.store.get_repeat(task_id, cx);
        self._repeat_template_fetch =
            Some(cx.spawn(async move |this, cx| match repeat_fetch.await {
                Ok(template) => {
                    this.update(cx, |this, cx| {
                        this.repeat_template = template;
                        this._repeat_template_fetch = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to fetch repeat template: {e}");
                }
            }));
        self.load_coding(task_id, auto_start, cx);
        cx.notify();
    }

    /// Re-fetch the selected task's coding run (after a phase action, or when
    /// the workflow panel reports a change).
    pub fn refresh_coding(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.load_coding(task_id, false, cx);
    }

    /// Fetch the coding run for `task_id` together with the sub-tasks hanging
    /// off each of its phase steps, so the step rows can nest them. When
    /// `auto_start` is set and the task has no run at all, start it first so
    /// the select of a feature task lands on its already-active interview.
    fn load_coding(&mut self, task_id: u64, auto_start: bool, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._coding_fetch = Some(cx.spawn(async move |this, cx| {
            let view = match store.coding_run_for_task(task_id, cx).await {
                Ok(view) => view,
                Err(error) => {
                    tracing::error!("Failed to fetch coding run: {error}");
                    this.update(cx, |this, cx| {
                        if this.selected.as_ref().map(|task| task.id) != Some(task_id) {
                            return;
                        }
                        this.coding_error = Some(error.to_string());
                        this._coding_fetch = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let mut view = view;
            if view.is_none() && auto_start {
                // Auto-start is for directory-backed projects only: a travel
                // checklist or any other non-project tag's task has no repo
                // to run in, so it never gets an interview or other steps.
                let directory_backed = store.coding_directory(task_id, cx).await.unwrap_or(false);
                this.update(cx, |this, cx| {
                    if this.selected.as_ref().map(|task| task.id) == Some(task_id) {
                        this.coding_directory_backed = directory_backed;
                        if !directory_backed {
                            this._coding_fetch = None;
                        }
                        cx.notify();
                    }
                })
                .ok();
                if !directory_backed {
                    return;
                }
                let claimed = this
                    .update(cx, |this, _| {
                        if this.coding_auto_started || this.coding_error.is_some() {
                            return false;
                        }
                        this.coding_auto_started = true;
                        true
                    })
                    .unwrap_or(false);
                if claimed {
                    match store.start_coding_run(task_id, cx).await {
                        Ok(_run_id) => {
                            view = store.coding_run_for_task(task_id, cx).await.ok().flatten();
                        }
                        Err(error) => {
                            tracing::error!("Auto-start of the coding run failed: {error}");
                            this.update(cx, |this, cx| {
                                if this.selected.as_ref().map(|task| task.id) != Some(task_id) {
                                    return;
                                }
                                this.coding_error = Some(error.to_string());
                                this._coding_fetch = None;
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    }
                }
            }
            let step_ids: Vec<u64> = view
                .as_ref()
                .map(|view| view.steps.iter().map(|step| step.task.id).collect())
                .unwrap_or_default();
            let step_subtasks = if step_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                match store.subtasks_map(step_ids, cx).await {
                    Ok(map) => map,
                    Err(error) => {
                        tracing::error!("Failed to fetch step sub-tasks: {error}");
                        std::collections::HashMap::new()
                    }
                }
            };
            let auto_started = view.is_some() && auto_start;
            let has_run = view.is_some();
            this.update(cx, |this, cx| {
                if this.selected.as_ref().map(|task| task.id) != Some(task_id) {
                    return;
                }
                this.coding = view;
                this.step_subtasks = step_subtasks;
                this._coding_fetch = None;
                if has_run {
                    this.coding_directory_backed = true;
                }
                if auto_started {
                    this.coding_error = None;
                    cx.emit(TaskDetailsEvent::CodingChanged);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Reload the selected task's subtasks from the DB, e.g. right after a
    /// new subtask was created.
    fn refresh_subtasks(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        let fetch = self.store.list_subtasks(task_id, cx);
        self._subtasks_fetch = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(subtasks) => {
                this.update(cx, |this, cx| {
                    this.subtasks = subtasks;
                    this._subtasks_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch subtasks: {e}");
            }
        }));
    }

    pub fn set_blockers(&mut self, blockers: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.blockers = blockers;
        cx.notify();
    }

    pub fn set_after_tasks(&mut self, after_tasks: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.after_tasks = after_tasks;
        cx.notify();
    }

    /// Fresh blocked state from the loaded blockers plus `blocked_until`,
    /// so reopening a blocker re-blocks immediately without a refetch.
    fn computed_blocked(&self) -> bool {
        let Some(task) = &self.selected else {
            return false;
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if task.blocked_until.is_some_and(|until| until > now_secs) {
            return true;
        }
        self.blockers.iter().any(|blocker| !blocker.done)
    }

    pub fn selected_task(&self) -> Option<TaskWithMeta> {
        self.selected.clone()
    }

    /// Request selecting a task. When edits are unsaved and the task is a
    /// different one, the request is stashed and a confirm dialog is shown
    /// instead; returns true when deferred.
    pub fn request_select(&mut self, task: TaskWithMeta, cx: &mut Context<Self>) -> bool {
        let same_task = self.selected.as_ref().is_some_and(|t| t.id == task.id);
        if self.is_editing() && !same_task {
            self.pending = Some(Some(task));
            self.confirming = true;
            cx.notify();
            true
        } else {
            self.set_selected(task, cx);
            false
        }
    }

    /// Request deselecting. Defers with a confirm dialog when edits are
    /// unsaved; returns true when deferred.
    pub fn request_clear(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_editing() {
            self.pending = Some(None);
            self.confirming = true;
            cx.notify();
            true
        } else {
            self.clear(cx);
            false
        }
    }

    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.confirming = false;
        self.abandon_edits();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.abandon_tags();
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        match pending {
            Some(task) => self.apply_selected(task, cx),
            None => self.clear(cx),
        }
        let selected = self.selected.clone();
        cx.emit(TaskDetailsEvent::PendingConfirmed { selected });
        cx.notify();
    }

    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        if !self.confirming {
            return;
        }
        self.pending = None;
        self.confirming = false;
        cx.emit(TaskDetailsEvent::PendingCancelled);
        cx.notify();
    }

    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

    pub fn update_title(&mut self, task_id: u64, title: String, cx: &mut Context<Self>) {
        if self
            .selected
            .as_ref()
            .is_some_and(|task| task.id == task_id)
        {
            if let Some(selected) = &mut self.selected {
                selected.task.title = title;
            }
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.blockers = Vec::new();
        self.after_tasks = Vec::new();
        self.subtasks = Vec::new();
        self.blocking = Vec::new();
        self.parent = None;
        self.repeat_template = None;
        self.link_error = None;
        self.close_until_panel();
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.cancel_editing(cx);
        cx.notify();
    }

    pub fn is_editing(&self) -> bool {
        self.editing_title || self.editing_description || self.editing_tags
    }

    fn abandon_edits(&mut self) {
        self.editing_title = false;
        self.title_input = None;
        self._title_subscription = None;
        self.editing_description = false;
        self.description_input = None;
        self._description_subscription = None;
        self.close_time_edit();
    }

    fn close_time_edit(&mut self) {
        self.time_edit = None;
        self.focus_time_edit = false;
    }

    /// Today's or tomorrow's date label plus current time for the Until row.
    /// Returns None for other dates, which render as static text.
    fn until_today_tomorrow(&self) -> Option<(String, u8, u8)> {
        let until = self.selected.as_ref()?.blocked_until?;
        let zoned = jiff::Timestamp::from_second(until as i64)
            .ok()?
            .to_zoned(jiff::tz::TimeZone::system());
        let today = crate::components::date_time_picker::today_date();
        let day_label = if zoned.date() == today {
            "today".to_string()
        } else if Some(zoned.date()) == today.tomorrow().ok() {
            "tomorrow".to_string()
        } else {
            return None;
        };
        Some((day_label, zoned.hour() as u8, zoned.minute() as u8))
    }

    /// Ensure the HH:MM inputs exist (creating + autofocus on first need),
    /// then commit their content, rebuilding them from the stored time when
    /// invalid or in the past.
    fn commit_time_inputs(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.time_edit.clone() else {
            return;
        };
        let Some(until) = self.selected.as_ref().and_then(|task| task.blocked_until) else {
            return;
        };
        let parse_cell = |entity: &Entity<InputState>, cx: &App| {
            entity
                .read(cx)
                .text()
                .to_string()
                .trim()
                .to_string()
                .parse::<u8>()
                .ok()
        };
        let (hour, minute) = (parse_cell(&edit.hour, cx), parse_cell(&edit.minute, cx));
        let timestamp = jiff::Timestamp::from_second(until as i64)
            .ok()
            .map(|stamp| stamp.to_zoned(jiff::tz::TimeZone::system()))
            .and_then(|zoned| {
                if hour.is_some_and(|h| h < 24) && minute.is_some_and(|m| m < 60) {
                    zoned
                        .date()
                        .at(hour.unwrap_or(0) as i8, minute.unwrap_or(0) as i8, 0, 0)
                        .to_zoned(jiff::tz::TimeZone::system())
                        .ok()
                        .map(|zoned| zoned.timestamp().as_second())
                } else {
                    None
                }
            });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64;
        match timestamp {
            Some(timestamp) if timestamp > now => {
                self.apply_blocked_until(timestamp as u64, false, cx);
            }
            _ => {
                self.time_edit = None;
                self.focus_time_edit = false;
                cx.notify();
            }
        }
    }

    pub fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        if self.editing_title || self.editing_description || self.editing_tags {
            self.editing_title = false;
            self.title_input = None;
            self._title_subscription = None;
            self.editing_description = false;
            self.description_input = None;
            self._description_subscription = None;
            self.abandon_tags();
            cx.notify();
        }
    }

    fn begin_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        if self.editing_title {
            return;
        }
        let title = task.title.clone();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&title, window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_title_edit(cx);
            }
        });
        self.title_input = Some(input.clone());
        self._title_subscription = Some(subscription);
        self.editing_title = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_title_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.title_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        if title.is_empty() {
            self.cancel_editing(cx);
            return;
        }
        let task_id = task.id;
        if let Some(selected) = &mut self.selected {
            selected.task.title = title.clone();
        }
        self.editing_title = false;
        self.title_input = None;
        self._title_subscription = None;
        self.store.rename_task(task_id, title.clone(), cx).detach();
        cx.emit(TaskDetailsEvent::TitleCommitted { task_id, title });
        cx.notify();
    }

    fn begin_description_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        if self.editing_description {
            return;
        }
        let description = task.description.clone().unwrap_or_default();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(&description, window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_description_edit(cx);
            }
        });
        self.description_input = Some(input.clone());
        self._description_subscription = Some(subscription);
        self.editing_description = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_description_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.description_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let description = input.read(cx).text().to_string();
        let description = description.trim().to_string();
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.task.description = if description.is_empty() {
                None
            } else {
                Some(description.clone())
            };
        }
        self.editing_description = false;
        self.description_input = None;
        self._description_subscription = None;
        let value = if description.is_empty() {
            None
        } else {
            Some(description)
        };
        cx.spawn(async move |this, cx| {
            if let Err(e) = store.set_task_description(task_id, value, cx).await {
                tracing::error!(?e, "Failed set_task_description");
            }
            this.update(cx, |_, cx| {
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Drop the inline tag input without notifying (callers that clear state
    /// on selection change notify themselves).
    fn abandon_tags(&mut self) {
        self.editing_tags = false;
        self.tags_input = None;
        self._tags_subscription = None;
        self.tag_draft = Vec::new();
        self.needs_tag_input_clear = false;
    }

    /// Open the tags editor: the current tags as chips inline in the field,
    /// with a blank text slot after the last one. Enter turns the pending
    /// text into a chip; Enter with no pending text saves the lot.
    fn begin_tags_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_tags {
            return;
        }
        let Some(task) = &self.selected else {
            return;
        };
        self.tag_draft = task.direct_tags.clone();
        self.reset_tag_input(window, cx);
        self.editing_tags = true;
        cx.notify();
    }

    /// Swap in a fresh, empty tag input. Used both to open the editor and,
    /// after Enter turns the pending text into a chip, to restart typing.
    fn reset_tag_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.needs_tag_input_clear = false;
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Add tags…", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| match event {
            InputEvent::PressEnter { .. } => this.on_tag_input_enter(cx),
            InputEvent::Change => this.on_tag_input_change(cx),
            InputEvent::Blur => this.commit_tags_edit(cx),
            _ => {}
        });
        self.tags_input = Some(input.clone());
        self._tags_subscription = Some(subscription);
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Enter in the tag input: non-empty text becomes a chip (and typing
    /// restarts empty); empty text saves all current chips.
    fn on_tag_input_enter(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.tags_input.clone() else {
            return;
        };
        let text = input.read(cx).text().to_string();
        if text.trim().is_empty() {
            self.commit_tags_edit(cx);
            return;
        }
        self.add_pending_tag(&text);
        self.needs_tag_input_clear = true;
        cx.notify();
    }

    /// Comma in the tag input: flush whatever was typed as a chip and restart
    /// with an empty field.
    fn on_tag_input_change(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.tags_input.clone() else {
            return;
        };
        let text = input.read(cx).text().to_string();
        if !text.trim_end().ends_with(',') {
            return;
        }
        self.add_pending_tag(text.trim_end_matches(',').trim());
        self.needs_tag_input_clear = true;
        cx.notify();
    }

    /// Turn raw pending text into a chip, unless it is empty or already present.
    fn add_pending_tag(&mut self, tag_raw: &str) {
        let tag = tag_raw.trim().trim_start_matches('#').to_lowercase();
        if tag.is_empty() {
            return;
        }
        if !self.tag_draft.iter().any(|draft| draft.to_lowercase() == tag) {
            self.tag_draft.push(tag);
        }
    }

    /// Remove a chip from the draft and keep editing.
    fn remove_tag_draft(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tag_draft.len() {
            self.tag_draft.remove(index);
            cx.notify();
        }
    }

    fn commit_tags_edit(&mut self, cx: &mut Context<Self>) {
        if !self.editing_tags {
            return;
        }
        let Some(task) = &self.selected else {
            return;
        };
        let tags = self.tag_draft.clone();
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.direct_tags = tags.clone();
        }
        self.abandon_tags();
        cx.spawn(async move |this, cx| {
            if let Err(e) = store.set_task_tags(task_id, tags, cx).await {
                tracing::error!("Failed to set tags: {e}");
                return;
            }
            let reload = store.reload_task(task_id, cx);
            match reload.await {
                Ok(fresh) => {
                    this.update(cx, |this, cx| {
                        if this.selected.as_ref().map(|task| task.id) != Some(task_id) {
                            return;
                        }
                        this.selected = Some(fresh.clone());
                        cx.emit(TaskDetailsEvent::TaskRefreshed(fresh));
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!("Failed to reload task after tags write: {e}");
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Drop the inline subtask input without notifying (callers that clear
    /// state on selection change notify themselves).
    fn abandon_subtask(&mut self) {
        self.adding_subtask = false;
        self.subtask_input = None;
        self._subtask_subscription = None;
    }

    pub fn adding_subtask(&self) -> bool {
        self.adding_subtask
    }

    /// Toggle the inline subtask input: clicking the button while already
    /// adding cancels, otherwise an autofocused title input appears below
    /// the relationship buttons.
    fn begin_subtask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        if self.adding_subtask {
            self.cancel_subtask(cx);
            return;
        }
        self.abandon_follow_up();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Subtask title...", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_subtask(cx);
            }
        });
        let focus_input = input.clone();
        self.subtask_input = Some(input);
        self._subtask_subscription = Some(subscription);
        self.adding_subtask = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            focus_input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_subtask(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.subtask_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let parent_id = task.id;
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        self.abandon_subtask();
        if title.is_empty() {
            cx.notify();
            return;
        }
        let create = self.store.insert_subtask(parent_id, title, cx);
        cx.spawn(async move |this, cx| match create.await {
            Ok(_created) => {
                this.update(cx, |this, cx| {
                    this.refresh_subtasks(cx);
                    cx.emit(TaskDetailsEvent::SubtaskCreated);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to create subtask: {e}");
                this.update(cx, |this, cx| {
                    this.link_error = Some(format!("Couldn't create subtask: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_subtask(&mut self, cx: &mut Context<Self>) {
        if !self.adding_subtask {
            return;
        }
        self.abandon_subtask();
        cx.notify();
    }

    /// Drop the inline follow-up input without notifying (callers that
    /// clear state on selection change notify themselves).
    fn abandon_follow_up(&mut self) {
        self.adding_follow_up = false;
        self.follow_up_input = None;
        self._follow_up_subscription = None;
    }

    pub fn adding_follow_up(&self) -> bool {
        self.adding_follow_up
    }

    /// Toggle the inline follow-up input: clicking the button while already
    /// adding cancels, otherwise an autofocused title input appears below
    /// the relationship buttons.
    fn begin_follow_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        if self.adding_follow_up {
            self.cancel_follow_up(cx);
            return;
        }
        self.abandon_subtask();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Follow-up title...", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_follow_up(cx);
            }
        });
        let focus_input = input.clone();
        self.follow_up_input = Some(input);
        self._follow_up_subscription = Some(subscription);
        self.adding_follow_up = true;
        cx.notify();
        window.on_next_frame(move |window, cx| {
            focus_input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn commit_follow_up(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.follow_up_input.clone() else {
            return;
        };
        let Some(task) = &self.selected else {
            return;
        };
        let blocked_by = task.id;
        let title = input.read(cx).text().to_string();
        let title = title.trim().to_string();
        self.abandon_follow_up();
        if title.is_empty() {
            cx.notify();
            return;
        }
        let create = self.store.create_follow_up(blocked_by, title, cx);
        cx.spawn(async move |this, cx| match create.await {
            Ok(_created) => {
                this.update(cx, |this, cx| {
                    this.link_error = None;
                    this.refresh_blocking(cx);
                    cx.emit(TaskDetailsEvent::FollowUpCreated);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to create follow-up task: {e}");
                this.update(cx, |this, cx| {
                    this.link_error = Some(format!("Couldn't create follow-up: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub fn cancel_follow_up(&mut self, cx: &mut Context<Self>) {
        if !self.adding_follow_up {
            return;
        }
        self.abandon_follow_up();
        cx.notify();
    }

    /// Reload the tasks this task blocks ("Linked to"), e.g. right after a
    /// follow-up task was created.
    fn refresh_blocking(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        let fetch = self.store.list_blocking_tasks(task_id, cx);
        self._blocking_fetch = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(blocking) => {
                this.update(cx, |this, cx| {
                    this.blocking = blocking;
                    this._blocking_fetch = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch linked tasks: {e}");
            }
        }));
    }

    /// Remove the blocker link so `linked_id` is no longer blocked by the
    /// selected task.
    fn remove_linked(&mut self, linked_id: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let remove = self.store.remove_blocker(linked_id, task.id, cx);
        cx.spawn(async move |this, cx| match remove.await {
            Ok(_) => {
                this.update(cx, |this, cx| {
                    this.refresh_blocking(cx);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to unlink task: {e}");
            }
        })
        .detach();
    }

    fn close_repeat_picker(&mut self) {
        self.repeat_picker = None;
        self._repeat_subscription = None;
    }

    pub fn close_repeat_picker_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_repeat_picker();
        cx.notify();
    }

    pub fn repeat_picker_open(&self) -> bool {
        self.repeat_picker.is_some()
    }

    /// True when the card was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_repeat_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.repeat_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_repeat_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.until_outside_closed_at = None;
        self.blocker_outside_closed_at = None;
        self.after_outside_closed_at = None;
        self.close_until_panel();
        self.close_blocker_picker();
        self.close_after_picker();
        self.abandon_subtask();
        self.abandon_follow_up();
        self.link_error = None;
        let picker = cx.new(|cx| RepeatPicker::new(None, window, cx));
        let subscription = cx.subscribe(&picker, |this, _picker, event, cx| match event {
            RepeatPickerEvent::Saved {
                interval_days,
                time_of_day,
            } => {
                this.close_repeat_picker();
                let Some(task) = this.selected.clone() else {
                    return;
                };
                let task_id = task.id;
                let name = task.title.clone();
                // The picker edits interval and due time only; keep any
                // existing start time.
                let start_time_of_day =
                    this.repeat_template.clone().and_then(|t| t.start_time_of_day);
                let set = this.store.set_repeat(
                    task_id,
                    name,
                    *interval_days,
                    *time_of_day,
                    start_time_of_day,
                    cx,
                );
                cx.spawn(async move |this, cx| match set.await {
                    Ok(template) => {
                        this.update(cx, |this, cx| {
                            this.link_error = None;
                            // Show the chosen interval in the details right away.
                            this.repeat_template = Some(template);
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to save repeat template: {e}");
                        this.update(cx, |this, cx| {
                            this.link_error = Some(format!("Couldn't save repeat: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            RepeatPickerEvent::Removed => {
                this.close_repeat_picker();
                let Some(task_id) = this.selected.as_ref().map(|task| task.id) else {
                    return;
                };
                let remove = this.store.remove_repeat(task_id, cx);
                cx.spawn(async move |this, cx| match remove.await {
                    Ok(()) => {
                        this.update(cx, |this, cx| {
                            this.link_error = None;
                            this.repeat_template = None;
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to remove repeat template: {e}");
                        this.update(cx, |this, cx| {
                            this.link_error = Some(format!("Couldn't remove repeat: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
        });
        self.repeat_picker = Some(picker.clone());
        self._repeat_subscription = Some(subscription);
        cx.notify();
        let fetch = self.store.get_repeat(task_id, cx);
        cx.spawn(async move |this, cx| {
            let template = match fetch.await {
                Ok(template) => template,
                Err(e) => {
                    tracing::error!("Failed to fetch repeat template: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                if let Some(picker) = this.repeat_picker.clone() {
                    picker.update(cx, |picker, cx| picker.set_current(template, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn repeat_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.repeat_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.repeat_outside_closed_at = Some(std::time::Instant::now());
                    this.close_repeat_picker_and_notify(cx);
                }))
                .child(picker)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn close_until_panel(&mut self) {
        self.until_picker = None;
        self._until_picker_subscription = None;
    }

    pub fn until_panel_open(&self) -> bool {
        self.until_picker.is_some()
    }

    pub fn close_until_panel_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_until_panel();
        cx.notify();
    }

    /// True when the card was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_until_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.until_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_until_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        self.blocker_outside_closed_at = None;
        self.after_outside_closed_at = None;
        self.repeat_outside_closed_at = None;
        self.close_blocker_picker();
        self.close_after_picker();
        self.close_repeat_picker();
        let picker = cx.new(|cx| DateTimePicker::new(window, cx));
        let subscription = cx.subscribe(&picker, |this, _picker, event, cx| match event {
            DateTimePickerEvent::Committed(until) => {
                eprintln!("DBG committed: {until}");
                this.apply_blocked_until(*until, true, cx);
            }
        });
        self.until_picker = Some(picker);
        self._until_picker_subscription = Some(subscription);
        cx.notify();
    }

    fn close_blocker_picker(&mut self) {
        self.blocker_picker = None;
        self._blocker_picker_subscription = None;
    }

    pub fn close_blocker_picker_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_blocker_picker();
        cx.notify();
    }

    pub fn blocker_picker_open(&self) -> bool {
        self.blocker_picker.is_some()
    }

    /// True when the picker was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_blocker_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.blocker_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_blocker_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.until_outside_closed_at = None;
        self.after_outside_closed_at = None;
        self.repeat_outside_closed_at = None;
        self.close_until_panel();
        self.close_after_picker();
        self.close_repeat_picker();
        let picker = cx.new(|cx| TaskPicker::new(Vec::new(), window, cx));
        let subscription = cx.subscribe(&picker, move |this, _picker, event, cx| match event {
            TaskPickerEvent::Selected(blocker_id) => {
                this.close_blocker_picker();
                let add = this.store.add_blocker(task_id, *blocker_id, cx);
                cx.spawn(async move |this, cx| match add.await {
                    Ok(blockers) => {
                        this.update(cx, |this, cx| {
                            this.set_blockers(blockers, cx);
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to add blocker: {e}");
                    }
                })
                .detach();
            }
            TaskPickerEvent::Dismissed => {
                this.close_blocker_picker();
                cx.notify();
            }
        });
        self.blocker_picker = Some(picker.clone());
        self._blocker_picker_subscription = Some(subscription);
        cx.notify();
        window.on_next_frame(move |window, cx| {
            picker.update(cx, |picker, cx| {
                picker.focus_filter(window, cx);
            });
        });
        let fetch = self.store.blocker_candidates(task_id, cx);
        cx.spawn(async move |this, cx| {
            let tasks = match fetch.await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::error!("Failed to fetch blocker candidates: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                if let Some(picker) = this.blocker_picker.clone() {
                    picker.update(cx, |picker, cx| picker.set_tasks(tasks, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn close_after_picker(&mut self) {
        self.after_picker = None;
        self._after_picker_subscription = None;
    }

    pub fn close_after_picker_and_notify(&mut self, cx: &mut Context<Self>) {
        self.close_after_picker();
        cx.notify();
    }

    pub fn after_picker_open(&self) -> bool {
        self.after_picker.is_some()
    }

    /// True when the picker was closed by an outside mousedown within the
    /// ignore window, consuming the marker so only that closing click is
    /// swallowed.
    fn take_recent_after_outside_close(&mut self) -> bool {
        let Some(closed_at) = self.after_outside_closed_at.take() else {
            return false;
        };
        closed_at.elapsed() < OUTSIDE_CLOSE_IGNORE_WINDOW
    }

    fn open_after_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        self.until_outside_closed_at = None;
        self.blocker_outside_closed_at = None;
        self.repeat_outside_closed_at = None;
        self.close_until_panel();
        self.close_blocker_picker();
        self.close_repeat_picker();
        self.link_error = None;
        let picker = cx.new(|cx| TaskPicker::new(Vec::new(), window, cx));
        let subscription = cx.subscribe(&picker, move |this, _picker, event, cx| match event {
            TaskPickerEvent::Selected(after_id) => {
                this.close_after_picker();
                let add = this.store.add_after(task_id, *after_id, cx);
                cx.spawn(async move |this, cx| match add.await {
                    Ok(after_tasks) => {
                        this.update(cx, |this, cx| {
                            this.link_error = None;
                            this.set_after_tasks(after_tasks, cx);
                        })
                        .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to add after link: {e}");
                        this.update(cx, |this, cx| {
                            this.link_error = Some(format!("Couldn't add task: {e}"));
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            TaskPickerEvent::Dismissed => {
                this.close_after_picker();
                cx.notify();
            }
        });
        self.after_picker = Some(picker.clone());
        self._after_picker_subscription = Some(subscription);
        cx.notify();
        window.on_next_frame(move |window, cx| {
            picker.update(cx, |picker, cx| {
                picker.focus_filter(window, cx);
            });
        });
        let fetch = self.store.after_candidates(task_id, cx);
        cx.spawn(async move |this, cx| {
            let tasks = match fetch.await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::error!("Failed to fetch after candidates: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                if let Some(picker) = this.after_picker.clone() {
                    picker.update(cx, |picker, cx| picker.set_tasks(tasks, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_blocked_until(&mut self, until: u64, refocus_time: bool, cx: &mut Context<Self>) {
        self.write_blocked_until(Some(until), refocus_time, cx);
    }

    fn clear_blocked_until(&mut self, cx: &mut Context<Self>) {
        self.write_blocked_until(None, false, cx);
    }

    /// Optimistic local update, then persist and reload from the DB so both
    /// this panel and the task list converge on stored truth.
    fn write_blocked_until(
        &mut self,
        value: Option<u64>,
        refocus_time: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = &self.selected else {
            return;
        };
        let task_id = task.id;
        let store = self.store.clone();
        if let Some(selected) = &mut self.selected {
            selected.task.blocked_until = value;
        }
        self.close_until_panel();
        self.time_edit = None;
        self.focus_time_edit = refocus_time && self.until_today_tomorrow().is_some();
        let write = store.set_blocked_until(task_id, value, cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = write.await {
                tracing::error!(?e, "Failed set_blocked_until");
                return;
            }
            let reload = store.reload_task(task_id, cx);
            match reload.await {
                Ok(fresh) => {
                    this.update(cx, |this, cx| {
                        this.selected = Some(fresh.clone());
                        cx.emit(TaskDetailsEvent::TaskRefreshed(fresh));
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    tracing::error!(?e, "Failed to reload task after blocked_until write");
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Floating card below the "+ blocked until" button: menu-style rows
    /// separated by hairlines, floating above the content underneath.
    fn until_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.until_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.until_outside_closed_at = Some(std::time::Instant::now());
                    this.close_until_panel_and_notify(cx);
                }))
                .child(picker)
        } else {
            div()
        }
    }

    fn blocker_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.blocker_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.blocker_outside_closed_at = Some(std::time::Instant::now());
                    this.close_blocker_picker_and_notify(cx);
                }))
                .child(picker)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn after_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = self.after_picker.clone() {
            div()
                .absolute()
                .top(px(60.))
                .left(px(0.))
                .right(px(0.))
                .bg(rgb(CARD_BG))
                .border_1()
                .border_color(rgb(HAIRLINE))
                .rounded_md()
                .px_3()
                .py_2()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.after_outside_closed_at = Some(std::time::Instant::now());
                    this.close_after_picker_and_notify(cx);
                }))
                .child(picker)
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }

    fn remove_blocker(&mut self, blocker_id: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let remove = self.store.remove_blocker(task.id, blocker_id, cx);
        cx.spawn(async move |this, cx| match remove.await {
            Ok(blockers) => {
                this.update(cx, |this, cx| {
                    this.set_blockers(blockers, cx);
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to remove blocker: {e}");
            }
        })
        .detach();
    }

    fn remove_after(&mut self, after_id: u64, cx: &mut Context<Self>) {
        let Some(task) = &self.selected else {
            return;
        };
        let remove = self.store.remove_after(task.id, after_id, cx);
        cx.spawn(async move |this, cx| match remove.await {
            Ok(after_tasks) => {
                this.update(cx, |this, cx| {
                    this.set_after_tasks(after_tasks, cx);
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to remove after link: {e}");
            }
        })
        .detach();
    }

    /// "Blocked until" row. For today/tomorrow the right side is an
    /// editable HH:MM pair (created lazily, autofocused once after picking);
    /// other dates render as static text.
    fn until_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A stale time block (already past) shows nothing at all, mirroring
        // `computed_blocked` which stops treating it as blocking.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let fresh = self
            .selected
            .as_ref()
            .and_then(|task| task.blocked_until)
            .is_some_and(|until| until > now_secs);
        if !fresh {
            return div();
        }

        let clear_button = Button::new("clear-blocked-until")
            .ghost()
            .compact()
            .cursor_pointer()
            .label("×")
            .tooltip("Clear time block")
            .on_click(cx.listener(|this, _, _, cx| {
                this.clear_blocked_until(cx);
            }));

        if let Some((day_label, hour, minute)) = self.until_today_tomorrow() {
            if self.time_edit.is_none() {
                let make_cell = |value: String, window: &mut Window, cx: &mut Context<Self>| {
                    let input = cx.new(|cx| {
                        let mut state = InputState::new(window, cx);
                        state.set_value(&value, window, cx);
                        state
                    });
                    let subscription = cx.subscribe(&input, |this, _, event, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            this.commit_time_inputs(cx);
                        }
                    });
                    (input, subscription)
                };
                let (hour_input, hour_sub) = make_cell(format!("{hour:02}"), window, cx);
                let (minute_input, minute_sub) = make_cell(format!("{minute:02}"), window, cx);
                self.time_edit = Some(TimeEditInputs {
                    hour: hour_input.clone(),
                    minute: minute_input,
                    _subscriptions: vec![hour_sub, minute_sub],
                });
                if self.focus_time_edit {
                    self.focus_time_edit = false;
                    window.on_next_frame(move |window, cx| {
                        hour_input.update(cx, |state, cx| state.focus(window, cx));
                    });
                }
            }
            let time_cells = self.time_edit.clone().map(|edit| {
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .w(px(30.))
                            .child(Input::new(&edit.hour).small().appearance(false)),
                    )
                    .child(div().text_sm().text_color(rgb(0xa3a3a3)).child(":"))
                    .child(
                        div()
                            .w(px(30.))
                            .child(Input::new(&edit.minute).small().appearance(false)),
                    )
            });
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(0xe5e5e5))
                        .child(format!("Blocked until {day_label}")),
                )
                .children(time_cells)
                .child(clear_button)
        } else if let Some(until) = self.selected.as_ref().and_then(|t| t.blocked_until) {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(0xe5e5e5))
                        .child(format!("Blocked until {}", format_deadline(until))),
                )
                .child(clear_button)
        } else {
            div()
        }
    }

    fn relationships_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut section = div().v_flex().gap_2().mt_2().child(
            div()
                .text_base()
                .font_bold()
                .underline()
                .text_color(rgb(0xe5e5e5))
                .child("Linked to"),
        );

        // The lists stack in normal flow below the buttons row, so they sit
        // under it whatever the pane width. The floating picker cards paint
        // after both and cover (and take hits before) the content underneath.
        let mut lists = div().v_flex().gap_2();

        if self.adding_subtask
            && let Some(input) = self.subtask_input.clone()
        {
            lists = lists.child(div().ml_2().child(Input::new(&input)));
        }
        if self.adding_follow_up
            && let Some(input) = self.follow_up_input.clone()
        {
            lists = lists.child(div().ml_2().child(Input::new(&input)));
        }

        if self.computed_blocked() {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child("Blocked by"),
            );
        }

        let blocker_rows = self
            .blockers
            .clone()
            .into_iter()
            .map(|blocker| {
                let blocker_id = blocker.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("blocker-title", blocker_id))
                            .flex_1()
                            .text_sm()
                            .cursor_pointer()
                            .text_color(if blocker.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(blocker.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask {
                                    task_id: blocker_id,
                                });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if blocker.done { "done" } else { "" }.to_string()),
                    )
                    .child(
                        Button::new(("remove-blocker", blocker_id))
                            .ghost()
                            .compact()
                            .cursor_pointer()
                            .label("×")
                            .tooltip("Remove blocker")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_blocker(blocker_id, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        if !blocker_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(blocker_rows));
        }

        if self
            .selected
            .as_ref()
            .and_then(|t| t.blocked_until)
            .is_some()
        {
            lists = lists.child(self.until_row(window, cx));
        }

        if !self.after_tasks.is_empty() {
            lists = lists.child(div().text_xs().text_color(rgb(0xa3a3a3)).child("After"));
        }

        let after_rows = self
            .after_tasks
            .clone()
            .into_iter()
            .map(|after| {
                let after_id = after.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("after-title", after_id))
                            .flex_1()
                            .text_sm()
                            .cursor_pointer()
                            .text_color(if after.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(after.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: after_id });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if after.done { "done" } else { "" }.to_string()),
                    )
                    .child(
                        Button::new(("remove-after", after_id))
                            .ghost()
                            .compact()
                            .cursor_pointer()
                            .label("×")
                            .tooltip("Remove after task")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_after(after_id, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        if !after_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(after_rows));
        }

        // Subtasks live inside the lists so they paint before (under) the
        // floating picker cards, which are siblings added after `lists`.
        // Coding phase steps are subtasks too, but they read as the run's step
        // list at the bottom of the panel, so they are left out here.
        let plain_subtasks: Vec<storage::Task> = self
            .subtasks
            .iter()
            .filter(|subtask| subtask.node_id.is_none())
            .cloned()
            .collect();
        if !plain_subtasks.is_empty() {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child(format!("Subtasks ({})", plain_subtasks.len())),
            );
        }
        let subtask_rows = plain_subtasks
            .into_iter()
            .map(|subtask| {
                let subtask_id = subtask.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("subtask-title", subtask_id))
                            .flex_1()
                            .text_sm()
                            .cursor_pointer()
                            .text_color(if subtask.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(subtask.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask {
                                    task_id: subtask_id,
                                });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if subtask.done { "done" } else { "" }.to_string()),
                    )
            })
            .collect::<Vec<_>>();
        if !subtask_rows.is_empty() {
            lists = lists.child(div().v_flex().gap_1().ml_2().children(subtask_rows));
        }

        // Tasks blocked by this task, grouped with its follow-ups (which
        // are blocked by it too): everything that comes after this task.
        // Follow-ups first, then the remaining blocked tasks.
        let selected_id = self.selected.as_ref().map(|t| t.id);
        let (follow_ups, other_blocked): (Vec<_>, Vec<_>) = self
            .blocking
            .clone()
            .into_iter()
            .partition(|task| selected_id.is_some_and(|id| task.source_task_id == Some(id)));
        let after_this_rows = follow_ups
            .into_iter()
            .chain(other_blocked)
            .map(|linked| {
                let linked_id = linked.id;
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(("linked-title", linked_id))
                            .flex_1()
                            .text_sm()
                            .cursor_pointer()
                            .text_color(if linked.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xe5e5e5)
                            })
                            .child(linked.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: linked_id });
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if linked.done { "done" } else { "" }.to_string()),
                    )
                    .child(
                        Button::new(("remove-linked", linked_id))
                            .ghost()
                            .compact()
                            .cursor_pointer()
                            .label("×")
                            .tooltip("Unlink task")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_linked(linked_id, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        if !after_this_rows.is_empty() {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child("Following tasks"),
            );
            lists = lists.child(div().v_flex().gap_1().ml_2().children(after_this_rows));
        }

        if let Some(error) = &self.link_error {
            lists = lists.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xff6b6b))
                    .child(error.clone()),
            );
        }

        section = section.child(
            div()
                .relative()
                .child(
                    div()
                        .v_flex()
                        .gap_2()
                        .child(
                            div()
                                .h_flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(
                                    relation_button("add-blocker", "+ blocked by task")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if this.blocker_picker_open() {
                                                this.close_blocker_picker_and_notify(cx);
                                            } else if this.take_recent_blocker_outside_close() {
                                                // The mousedown before this click already
                                                // closed the picker; don't reopen it.
                                            } else {
                                                this.open_blocker_picker(window, cx);
                                            }
                                        })),
                                )
                                .child(
                                    relation_button("add-blocked-until", "+ blocked until")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if this.until_panel_open() {
                                                this.close_until_panel_and_notify(cx);
                                            } else if this.take_recent_until_outside_close() {
                                                // The mousedown before this click already
                                                // closed the card; don't reopen it.
                                            } else {
                                                this.open_until_panel(window, cx);
                                            }
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .h_flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(relation_button("add-after", "+ after task").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        if this.after_picker_open() {
                                            this.close_after_picker_and_notify(cx);
                                        } else if this.take_recent_after_outside_close() {
                                            // The mousedown before this click already
                                            // closed the picker; don't reopen it.
                                        } else {
                                            this.open_after_picker(window, cx);
                                        }
                                    }),
                                ))
                                .child(relation_button("add-subtask", "+ subtask").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.begin_subtask(window, cx);
                                    }),
                                ))
                                .child(
                                    relation_button("add-follow-up", "+ follow-up task")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.begin_follow_up(window, cx);
                                        })),
                                )
                                .child(
                                    // "repeat" button: an inline SVG icon left of the
                                    // label, tinted the same color as the text.
                                    div()
                                        .id("repeat-task")
                                        .h_flex()
                                        .items_center()
                                        .gap_1()
                                        .h_6()
                                        .px_1p5()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(HAIRLINE))
                                        .text_sm()
                                        .text_color(rgb(0xa3a3a3))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(rgb(0x2a2a2a)))
                                        .child(
                                            svg()
                                                .data(REFRESH_ICON_SVG)
                                                .size(px(14.))
                                                .text_color(rgb(0xa3a3a3)),
                                        )
                                        .child("repeat")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if this.repeat_picker_open() {
                                                this.close_repeat_picker_and_notify(cx);
                                            } else if this.take_recent_repeat_outside_close() {
                                                // The mousedown before this click already
                                                // closed the card; don't reopen it.
                                            } else {
                                                this.open_repeat_picker(window, cx);
                                            }
                                        })),
                                ),
                        ),
                )
                .child(lists)
                .when(self.until_panel_open(), |this| {
                    this.child(self.until_card(cx))
                })
                .child(self.blocker_card(cx))
                .child(self.after_card(cx))
                .child(self.repeat_card(cx)),
        );

        section
    }
}

/// Lucide `refresh-cw`: two half-circle arrows chasing each other. Drawn
/// with an opaque stroke; GPUI renders SVG data as an alpha mask tinted by
/// the element's text color, so the icon picks up the surrounding text
/// color automatically.
const REFRESH_ICON_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/></svg>"##;

/// Small transparent relationship button: gray text with a gray hairline
/// outline, shared by the buttons in the relationships section.
fn relation_button(id: &'static str, label: &str) -> Button {
    Button::new(id)
        .ghost()
        .compact()
        .with_size(gpui_component::Size::Small)
        .border_1()
        .border_color(rgb(HAIRLINE))
        .text_color(rgb(0xa3a3a3))
        .cursor_pointer()
        .label(label)
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

/// A small muted chip, used for run metadata (recipe name, round, phase).
fn chip(label: &str) -> impl IntoElement {
    div()
        .text_size(px(10.))
        .px(px(5.))
        .py(px(1.))
        .rounded(px(3.))
        .bg(rgb(0x2a2a2a))
        .text_color(rgb(0xa3a3a3))
        .child(label.to_string())
}

impl TaskDetails {
    // ─── Coding workflow actions ────────────────────────────────────────────

    fn close_coding_notes(&mut self) {
        self.coding_notes_open = false;
        self.coding_notes_step = None;
        self.coding_notes_input = None;
        self._coding_notes_subscription = None;
    }

    fn close_coding_spec(&mut self) {
        self.coding_spec_open = false;
        self.coding_spec_input = None;
        self._coding_spec_subscription = None;
    }

    /// Run a coding action, then reload the run and surface any failure inline.
    /// Every phase action funnels through here so the stepper always reflects
    /// what the engine actually did (or why it refused).
    fn run_coding_action(
        &mut self,
        action: gpui::Task<anyhow::Result<()>>,
        cx: &mut Context<Self>,
    ) {
        let store = self.store.clone();
        let target = self.selected.as_ref().map(|task| task.id);
        self._coding_fetch = Some(cx.spawn(async move |this, cx| {
            let error = action.await.err().map(|error| error.to_string());
            let view = match target {
                Some(task_id) => match store.coding_run_for_task(task_id, cx).await {
                    Ok(view) => view,
                    Err(fetch_error) => {
                        tracing::error!("Failed to reload coding run: {fetch_error}");
                        None
                    }
                },
                None => None,
            };
            let step_ids: Vec<u64> = view
                .as_ref()
                .map(|view| view.steps.iter().map(|step| step.task.id).collect())
                .unwrap_or_default();
            let step_subtasks = if step_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                match store.subtasks_map(step_ids, cx).await {
                    Ok(map) => map,
                    Err(fetch_error) => {
                        tracing::error!("Failed to fetch step sub-tasks: {fetch_error}");
                        std::collections::HashMap::new()
                    }
                }
            };
            this.update(cx, |this, cx| {
                this.coding = view;
                this.step_subtasks = step_subtasks;
                this.coding_error = error;
                this._coding_fetch = None;
                this.close_coding_notes();
                this.close_coding_spec();
                cx.emit(TaskDetailsEvent::CodingChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_coding_run(&mut self, task_id: u64, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let action =
            cx.spawn(async move |_, cx| store.start_coding_run(task_id, cx).await.map(|_| ()));
        self.run_coding_action(action, cx);
    }

    fn complete_coding_step(
        &mut self,
        step_id: u64,
        result: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let action = self.store.complete_workflow_step(step_id, result, cx);
        self.run_coding_action(action, cx);
    }

    fn approve_coding_spec(&mut self, task_id: u64, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let action =
            cx.spawn(async move |_, cx| store.approve_coding_spec(task_id, cx).await.map(|_| ()));
        self.run_coding_action(action, cx);
    }

    fn merge_coding_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let action = self.store.merge_coding_branch(run_id, cx);
        self.run_coding_action(action, cx);
    }

    fn cancel_coding_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let action = self.store.cancel_workflow_run(run_id, cx);
        self.run_coding_action(action, cx);
    }

    fn request_sub_task_interview(&mut self, sub_task_id: u64, cx: &mut Context<Self>) {
        let action = self.store.request_sub_task_interview(sub_task_id, cx);
        self.run_coding_action(action, cx);
    }

    /// Open the reject-notes box for the approval step `step_id`.
    fn open_coding_notes(&mut self, step_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.coding_notes_open && self.coding_notes_step == Some(step_id) {
            self.close_coding_notes();
            cx.notify();
            return;
        }
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("What needs to change?", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_coding_reject(cx);
            }
        });
        self.coding_notes_open = true;
        self.coding_notes_step = Some(step_id);
        self.coding_notes_input = Some(input);
        self._coding_notes_subscription = Some(subscription);
        cx.notify();
    }

    /// Reject the gate the notes box belongs to, carrying the notes as the
    /// step result and into the run log.
    fn commit_coding_reject(&mut self, cx: &mut Context<Self>) {
        let Some(step_id) = self.coding_notes_step else {
            return;
        };
        let notes = self
            .coding_notes_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let notes = notes.trim().to_string();
        let result = serde_json::json!({ "approved": false, "notes": notes });
        self.complete_coding_step(step_id, result, cx);
    }

    /// Open the manual-spec box (the fallback when no interview agent is
    /// wired into the pane).
    fn open_coding_spec(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.coding_spec_open {
            self.close_coding_spec();
            cx.notify();
            return;
        }
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Paste or write the spec, then Enter", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_coding_spec(cx);
            }
        });
        self.coding_spec_open = true;
        self.coding_spec_input = Some(input);
        self._coding_spec_subscription = Some(subscription);
        cx.notify();
    }

    /// Save the manual spec and let the engine advance the run to the spec
    /// gate.
    fn commit_coding_spec(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected.as_ref().map(|task| task.id) else {
            return;
        };
        let spec = self
            .coding_spec_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
            .trim()
            .to_string();
        if spec.is_empty() {
            return;
        }
        let action = self.store.save_coding_spec(task_id, spec, None, cx);
        self.run_coding_action(action, cx);
    }

    /// Compose a phase prompt and hand it to the layout, which inserts it into
    /// the agent pane and switches to it.
    fn launch_phase(
        &mut self,
        task: &TaskWithMeta,
        view: &RunView,
        phase: &str,
        cx: &mut Context<Self>,
    ) {
        let notes = run_notes(&view.run.step_results.0);
        let prompt = phase_prompt(task, &notes, view.run.branch.as_deref(), phase);
        cx.emit(TaskDetailsEvent::CodingLaunch {
            phase: phase.to_string(),
            prompt,
        });
    }

    // ─── Coding workflow rendering ─────────────────────────────────────────

    fn coding_section(
        &mut self,
        task: &TaskWithMeta,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(view) = self.coding.clone() else {
            // Only the feature task (a top-level task) gets a coding run; phase
            // steps and plain sub-tasks have nothing to start here.
            if task.node_id.is_some() || task.parent_id.is_some() || task.done {
                return div().into_any_element();
            }
            // Tasks outside a directory-backed project get no coding workflow
            // at all: there is nowhere to run the spec-implement cycle, so the
            // pane shows nothing rather than a start button. The directory
            // check is async, so while it is pending show nothing too.
            if !self.coding_directory_backed {
                return div().into_any_element();
            }
            // The run auto-starts on selection, so while the fetch is in
            // flight there is nothing to act on yet; a quiet row beats the
            // start button flashing in and out.
            if self._coding_fetch.is_some() {
                return div()
                    .mt_2()
                    .ml_2()
                    .px_2()
                    .py(px(3.))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child("Preparing the interview…"),
                    )
                    .into_any_element();
            }
            // The auto-start failed: surface the reason and offer the start
            // button as the retry affordance.
            let mut fallback = div().v_flex().gap_2();
            if let Some(error) = self.coding_error.clone() {
                fallback = fallback.child(div().text_xs().text_color(rgb(0xf87171)).child(error));
            }
            return fallback.child(self.coding_start_card(task, cx)).into_any_element();
        };
        let run_id = view.run.id;
        let task_id = task.id;
        let notes = run_notes(&view.run.step_results.0);
        let round = round_number(&notes);
        let current = current_step(&view.steps).cloned();
        let mut section = div()
            .id(("coding-section", task_id))
            .v_flex()
            .gap_2()
            .mt_2();

        // The steps first: they are the run, and the next one carries its
        // action on its own row.
        section = section.child(self.coding_steps(task, &view, window, cx));

        let mut meta = div().h_flex().items_center().gap_2().flex_wrap();
        meta = meta.child(chip(&format!("Round {round}")));
        match view.run.branch.clone() {
            Some(branch) => {
                let status = view.run.branch_status.clone().unwrap_or_default();
                let detail = match view.run.base_branch.clone() {
                    Some(base) if !base.is_empty() => format!("{status} · from {base}"),
                    _ => status,
                };
                meta = meta
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .child(format!("branch {branch}")),
                    )
                    .child(chip(&detail));
            }
            None => {
                if view.run.status != "active" {
                    meta = meta.child(chip(&view.run.status));
                }
            }
        }
        if view.run.status == "active" {
            meta = meta.child(
                Button::new(format!("coding-cancel-{run_id}"))
                    .ghost()
                    .compact()
                    .label("Cancel run")
                    .tooltip("Cancel the run; its branch stays for cleanup")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_coding_run(run_id, cx);
                    })),
            );
        }
        section = section.child(meta);
        section = section.child(self.coding_secondary_actions(
            task,
            &view,
            current.as_ref(),
            window,
            cx,
        ));

        if let Some(error) = self.coding_error.clone() {
            section = section.child(div().text_xs().text_color(rgb(0xf87171)).child(error));
        }
        section = section.child(self.coding_round_log(&notes));
        section = section.child(self.coding_spec_artifact(task, &view, cx));
        section.into_any_element()
    }

    /// The fallback affordance on a feature task whose run could not be
    /// auto-started: the start button, at the bottom of the panel where the
    /// step list will go once the run exists.
    fn coding_start_card(&mut self, task: &TaskWithMeta, cx: &mut Context<Self>) -> AnyElement {
        let task_id = task.id;
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .mt_2()
            .ml_2()
            .child(
                Button::new(format!("coding-start-{task_id}"))
                    .compact()
                    .label("Start coding workflow")
                    .tooltip("Turn this task into an interview → spec → implement → review → merge run")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_coding_run(task_id, cx);
                    })),
            )
            .into_any_element()
    }

    /// The run's phase steps, rendered as the sub-tasks they are: one row per
    /// phase in run order, the next pending one highlighted with the action
    /// that moves it, and the model's sub-tasks of that step nested below it.
    fn coding_steps(
        &mut self,
        task: &TaskWithMeta,
        view: &RunView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current_id = current_step(&view.steps).map(|step| step.task.id);
        let mut rows: Vec<AnyElement> = Vec::new();
        for step in view.steps.iter() {
            let step_id = step.task.id;
            let done = step.task.done;
            let is_current = Some(step_id) == current_id;
            let (glyph, color) = match phase_state(&view.steps, &step.node.id) {
                PhaseState::Done => ("☑", rgb(0x6b6b6b)),
                PhaseState::Active => ("◐", rgb(0xd4d4d4)),
                PhaseState::Pending => ("☐", rgb(0x737373)),
            };
            let mut row = div()
                .h_flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(3.))
                .rounded_md()
                .when(is_current, |this| this.bg(rgb(PANEL_HOVER)))
                .child(div().text_xs().text_color(color).child(glyph))
                .child(
                    div()
                        .id(("coding-step", step_id))
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(if done { rgb(0x666666) } else { rgb(0xe5e5e5) })
                        .child(step.task.title.clone())
                        .on_click(cx.listener(move |_this, _, _, cx| {
                            cx.emit(TaskDetailsEvent::SelectTask { task_id: step_id });
                        })),
                );
            if done {
                row = row.child(div().text_xs().text_color(rgb(0x737373)).child("done"));
            } else if is_current
                && let Some(action) = self.coding_primary_action(task, view, step, cx)
            {
                row = row.child(action);
            }
            rows.push(row.into_any_element());

            // The model can split a step further: those sub-tasks hang off the
            // step, so they nest under its row.
            if let Some(children) = self.step_subtasks.get(&step_id).cloned() {
                rows.push(self.coding_step_children(&children, cx));
            }
        }
        // The run's later phases do not exist as steps yet (they materialize
        // only when the previous one completes), so preview them as disabled
        // rows to show where the run is going.
        let present: std::collections::HashSet<&str> = view
            .steps
            .iter()
            .filter_map(|step| step.node.phase.as_deref())
            .collect();
        for phase in CODING_PHASES {
            if present.contains(phase) {
                continue;
            }
            rows.push(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(3.))
                    .rounded_md()
                    .child(div().text_xs().text_color(rgb(0x4f4f4f)).child("☐"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(0x7a7a7a))
                            .child(phase_roadmap_title(phase)),
                    )
                    .into_any_element(),
            );
        }
        div()
            .v_flex()
            .gap_1()
            .ml_2()
            .children(rows)
            .into_any_element()
    }

    /// The model's sub-tasks of one phase step, indented under its row.
    fn coding_step_children(&mut self, children: &[TaskWithMeta], cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = children
            .iter()
            .map(|child| {
                let child_id = child.id;
                let nested = child.workflow_run_id.is_some();
                let mut row = div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8a8a8a))
                            .child(if child.done { "☑" } else { "☐" }),
                    )
                    .child(
                        div()
                            .id(("coding-step-child", child_id))
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(if child.done {
                                rgb(0x666666)
                            } else {
                                rgb(0xd4d4d4)
                            })
                            .child(child.title.clone())
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TaskDetailsEvent::SelectTask { task_id: child_id });
                            })),
                    );
                if nested {
                    row = row.child(chip("nested run"));
                } else if !child.done {
                    row = row.child(
                        Button::new(format!("coding-child-interview-{child_id}"))
                            .ghost()
                            .compact()
                            .label("Interview")
                            .tooltip("Start a sub-task interview run")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.request_sub_task_interview(child_id, cx);
                            })),
                    );
                }
                row.into_any_element()
            })
            .collect();
        div()
            .v_flex()
            .gap_1()
            .pl_4()
            .children(rows)
            .into_any_element()
    }

    /// The forward action for the current step, sitting on its row: the
    /// phase's start (compose its prompt) or the gate's approve/merge.
    fn coding_primary_action(
        &mut self,
        task: &TaskWithMeta,
        view: &RunView,
        step: &RunStepView,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let task_id = task.id;
        let step_id = step.task.id;
        let run_id = view.run.id;
        let node_id = step.node.id.clone();
        let label = match node_id.as_str() {
            "interview" => "Start interview",
            "spec" => "Approve spec",
            "implement" => "Start implementation",
            "review" => "Approve review",
            "merge" => "Merge branch",
            _ => return None,
        };
        let tooltip = match node_id.as_str() {
            "interview" | "implement" => "Compose this phase's prompt in the agent pane",
            "spec" => "Cut the feature branch and start implementation",
            "review" => "Accept the implementation and queue the merge",
            _ => "Merge the feature branch into its base branch",
        };
        let task_for_prompt = task.clone();
        let view_for_prompt = view.clone();
        Some(
            Button::new(format!("coding-primary-{step_id}"))
                .compact()
                .label(label)
                .tooltip(tooltip)
                .on_click(cx.listener(move |this, _, _, cx| match node_id.as_str() {
                    "interview" => {
                        this.launch_phase(&task_for_prompt, &view_for_prompt, "interview", cx)
                    }
                    "implement" => {
                        this.launch_phase(&task_for_prompt, &view_for_prompt, "implement", cx)
                    }
                    "spec" => this.approve_coding_spec(task_id, cx),
                    "review" => this.complete_coding_step(
                        step_id,
                        serde_json::json!({ "approved": true }),
                        cx,
                    ),
                    "merge" => this.merge_coding_run(run_id, cx),
                    _ => {}
                }))
                .into_any_element(),
        )
    }

    /// The current step's secondary actions, below the step list, plus the
    /// reject and manual-spec boxes when they are open. The forward action
    /// sits on the step's own row instead.
    fn coding_secondary_actions(
        &mut self,
        task: &TaskWithMeta,
        view: &RunView,
        current: Option<&RunStepView>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let task_id = task.id;
        let mut actions = div().h_flex().items_center().gap_2().flex_wrap();
        match current.map(|step| step.node.id.as_str()) {
            Some("interview") => {
                actions = actions
                    .child(
                        Button::new(format!("coding-manual-spec-{task_id}"))
                            .ghost()
                            .compact()
                            .label("Write spec manually")
                            .tooltip("Skip the agent and write the spec yourself")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_coding_spec(window, cx);
                            })),
                    );
            }
            Some("spec") => {
                let step_id = current.map(|step| step.task.id).unwrap_or_default();
                actions = actions
                    .child(
                        Button::new(format!("coding-reject-spec-{task_id}"))
                            .ghost()
                            .compact()
                            .label("Reject…")
                            .tooltip("Send the run back to the interview with notes")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_coding_notes(step_id, window, cx);
                            })),
                    );
            }
            Some("implement") => {
                let step_id = current.map(|step| step.task.id).unwrap_or_default();
                actions = actions
                    .child(
                        Button::new(format!("coding-implement-done-{task_id}"))
                            .ghost()
                            .compact()
                            .label("Mark implemented")
                            .tooltip("Confirm the work is done and move to review")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.complete_coding_step(step_id, serde_json::json!({}), cx);
                            })),
                    );
            }
            Some("review") => {
                let step_id = current.map(|step| step.task.id).unwrap_or_default();
                let task_for_prompt = task.clone();
                let view_for_prompt = view.clone();
                actions = actions
                    .child(
                        Button::new(format!("coding-review-brief-{task_id}"))
                            .ghost()
                            .compact()
                            .label("Ask for a summary")
                            .tooltip("Compose a summary request in the agent pane")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.launch_phase(
                                    &task_for_prompt,
                                    &view_for_prompt,
                                    "review",
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(format!("coding-review-reject-{task_id}"))
                            .ghost()
                            .compact()
                            .label("Reject…")
                            .tooltip("Send the run back to the interview with notes")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_coding_notes(step_id, window, cx);
                            })),
                    );
            }
            _ => {}
        }

        if self.coding_notes_open
            && let Some(input) = self.coding_notes_input.clone()
        {
            actions = actions.child(
                div()
                    .v_flex()
                    .gap_1()
                    .w_full()
                    .child(field_label("Why is this being sent back?"))
                    .child(
                        Input::new(&input)
                            .small()
                            .appearance(false)
                            .bg(rgb(APP_BG))
                            .border_1()
                            .border_color(rgb(HAIRLINE))
                            .rounded_md(),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new(format!("coding-notes-cancel-{task_id}"))
                                    .ghost()
                                    .compact()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_coding_notes();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(format!("coding-notes-send-{task_id}"))
                                    .compact()
                                    .label("Reject with notes")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.commit_coding_reject(cx);
                                    })),
                            ),
                    ),
            );
        }
        if self.coding_spec_open
            && let Some(input) = self.coding_spec_input.clone()
        {
            actions = actions.child(
                div()
                    .v_flex()
                    .gap_1()
                    .w_full()
                    .child(field_label("Spec"))
                    .child(
                        Input::new(&input)
                            .small()
                            .appearance(false)
                            .bg(rgb(APP_BG))
                            .border_1()
                            .border_color(rgb(HAIRLINE))
                            .rounded_md(),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new(format!("coding-spec-cancel-{task_id}"))
                                    .ghost()
                                    .compact()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_coding_spec();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(format!("coding-spec-save-{task_id}"))
                                    .compact()
                                    .label("Save spec")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.commit_coding_spec(cx);
                                    })),
                            ),
                    ),
            );
        }
        actions.into_any_element()
    }

    fn coding_round_log(&self, log: &[RunNote]) -> AnyElement {
        let notes = notes_by_round(log);
        if notes.is_empty() {
            return div().into_any_element();
        }
        let rounds = notes.iter().map(|(round, _)| *round).max().unwrap_or(1);
        let rows: Vec<AnyElement> = notes
            .iter()
            .map(|(round, note)| {
                let when = jiff::Timestamp::from_second(note.at as i64)
                    .map(|at| at.to_zoned(jiff::tz::TimeZone::system()))
                    .map(|at| at.strftime("%b %-d %-I:%M%p").to_string())
                    .unwrap_or_default();
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .child(chip(&format!("Round {round}")))
                    .child(chip(&note.kind))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .child(format!("{} · {when}", note.body)),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .v_flex()
            .gap_1()
            .child(field_label(&format!("Round log ({rounds} cycles)")))
            .children(rows)
            .into_any_element()
    }

    fn coding_spec_artifact(
        &self,
        task: &TaskWithMeta,
        view: &RunView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(spec) = task.spec.clone().filter(|spec| !spec.is_empty()) else {
            return div().into_any_element();
        };
        let lines = spec.lines().count();
        let mut block = div()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("coding-spec-toggle")
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .cursor_pointer()
                            .child(format!(
                                "{} Spec ({lines} lines)",
                                if self.coding_spec_expanded { "▾" } else { "▸" }
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.coding_spec_expanded = !this.coding_spec_expanded;
                                cx.notify();
                            })),
                    )
                    .when_some(self.spec_path_hint(task), |this, path| {
                        this.child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(0x8a8a8a))
                                .child(path),
                        )
                    }),
            )
            .when(self.coding_spec_expanded, |this| {
                this.child(
                    div()
                        .max_h(px(240.))
                        .overflow_y_scrollbar()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(APP_BG))
                        .text_xs()
                        .text_color(rgb(0xd4d4d4))
                        .child(spec.clone()),
                )
            });
        // Keep the run in the signature: the artifact is part of the run, and
        // callers pass it so the render order matches the other sections.
        let _ = view;
        block = block.w_full();
        block.into_any_element()
    }

    /// The spec file path the agent reported, shown next to the spec header.
    fn spec_path_hint(&self, task: &TaskWithMeta) -> Option<String> {
        task.spec_path.clone().filter(|path| !path.is_empty())
    }
}

/// A tag chip, used both in the read-only tag list and inside the tag editor.
fn tag_chip(label: &str) -> Div {
    div()
        .text_size(px(10.))
        .px(px(4.))
        .py(px(1.))
        .rounded(px(2.))
        .bg(rgb(0x2a2a2a))
        .text_color(rgb(0xa3a3a3))
        .child(format!("#{label}"))
}

impl Render for TaskDetails {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.selected {
            None => div().flex_1().flex().items_center().justify_center().child(
                div()
                    .text_sm()
                    .text_color(rgb(0x737373))
                    .child("Select a task to see details"),
            ),
            Some(task) => {
                // Owned so the coding section (which needs `&mut self`) does not
                // conflict with the borrow of `self.selected`.
                let task = task.clone();
                let task_id = task.id;
                let done = task.done;
                let store = self.store.clone();
                let entity = cx.entity().clone();
                let blocked = self.computed_blocked();

                let mut details = div().v_flex().gap_3();
                let mut header = div().v_flex().gap_1();
                if self.editing_tags {
                    if self.needs_tag_input_clear {
                        self.reset_tag_input(window, cx);
                    }
                    let input = self.tags_input.clone();
                    header = header.child(
                        div()
                            .id(("details-tags-edit", task_id))
                            .flex_1()
                            .min_w_0()
                            .px(px(4.))
                            .py(px(0.))
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(HAIRLINE))
                            .bg(rgb(APP_BG))
                            .when_some(input, |this, input| {
                                this.h_flex()
                                    .items_center()
                                    .gap_1()
                                    .children(self.tag_draft.iter().enumerate().map(|(idx, tag)| {
                                        tag_chip(tag)
                                            .id(("tag-chip", idx))
                                            .h_flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .id(("tag-chip-remove", idx))
                                                    .text_size(px(10.))
                                                    .text_color(rgb(0x737373))
                                                    .cursor_pointer()
                                                    .hover(|this| this.text_color(rgb(0xe5e5e5)))
                                                    .child("×")
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.remove_tag_draft(idx, cx);
                                                    })),
                                            )
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(Input::new(&input).appearance(false)),
                                    )
                            }),
                    );
                } else {
                    header = header.child(
                        div()
                            .h_flex()
                            .gap_1()
                            .flex_wrap()
                            .items_center()
                            .children(task.leaf_tags.iter().enumerate().map(|(index, tag)| {
                                tag_chip(tag)
                                    .id(("details-tag", index))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(0x333333)))
                                    .on_click(cx.listener(|this, event, window, cx| {
                                        if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                        {
                                            this.begin_tags_edit(window, cx);
                                        }
                                    }))
                            }))
                            .child(
                                div()
                                    .id(("details-tag-add", task_id))
                                    .text_size(px(10.))
                                    .px(px(2.))
                                    .py(px(2.))
                                    .rounded(px(2.))
                                    .text_color(rgb(0xa3a3a3))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(0x333333)))
                                    .child("+")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.begin_tags_edit(window, cx);
                                    })),
                            ),
                    );
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
                                .disabled(blocked && !done)
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
                            if let Some(input) = self.title_input.clone() {
                                div()
                                    .id(("details-title-edit", task_id))
                                    .flex_1()
                                    .child(
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
                                    .id(("details-title", task_id))
                                    .flex_1()
                                    .text_xl()
                                    .font_bold()
                                    .cursor_pointer()
                                    .text_color(if done {
                                        rgb(0x666666)
                                    } else {
                                        rgb(0xe5e5e5)
                                    })
                                    .when(done, |this| this.line_through())
                                    .child(task.title.clone())
                                    .on_click(cx.listener(|this, event, window, cx| {
                                        if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                        {
                                            this.begin_title_edit(window, cx);
                                        }
                                    }))
                            },
                        ),
                );
                details = details.child(header);
                if let Some(parent) = self.parent.clone() {
                    let parent_id = parent.id;
                    details = details.child(
                        div().v_flex().gap_1().child(field_label("Parent")).child(
                            div()
                                .id(("parent-link", parent_id))
                                .text_sm()
                                .text_color(rgb(0x93c5fd))
                                .hover(|this| this.underline())
                                .child(parent.title.clone())
                                .on_click(cx.listener(move |_this, _, _, cx| {
                                    cx.emit(TaskDetailsEvent::SelectTask { task_id: parent_id });
                                })),
                        ),
                    );
                }
                if self.editing_description {
                    if let Some(input) = self.description_input.clone() {
                        details = details.child(
                            div()
                                .id(("details-description-edit", task_id))
                                .child(
                                    Input::new(&input)
                                        .small()
                                        .appearance(false)
                                        .bg(rgb(APP_BG))
                                        .border_1()
                                        .border_color(rgb(HAIRLINE))
                                        .rounded_md(),
                                ),
                        );
                    }
                } else if let Some(desc) = &task.description
                    && !desc.is_empty()
                {
                    details = details.child(
                        div()
                            .id(("details-description", task_id))
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(0xe5e5e5))
                            .cursor_pointer()
                            .child(desc.clone())
                            .on_click(cx.listener(|this, event, window, cx| {
                                if matches!(event, ClickEvent::Mouse(m) if m.up.click_count == 2)
                                {
                                    this.begin_description_edit(window, cx);
                                }
                            })),
                    );
                } else {
                    // No description yet: a grayed-out affordance row that
                    // opens the editor on click.
                    details = details.child(
                        div()
                            .id(("details-description-empty", task_id))
                            .w_full()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(0x737373))
                            .cursor_pointer()
                            .child(div().flex_none().child(IconName::FileText))
                            .child("Description")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_description_edit(window, cx);
                            })),
                    );
                }
                if let Some(deadline) = task.deadline {
                    details = details.child(field("Deadline", format_deadline(deadline)));
                }
                // Overdue time blocks show nowhere: same gate as `until_row`.
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if let Some(until) = task.blocked_until
                    && until > now_secs
                {
                    details = details.child(field("Blocked until", format_deadline(until)));
                }
                if let Some(template) = &self.repeat_template {
                    details = details.child(field(
                        "Repeats",
                        repeat_label(template.interval_days, template.time_of_day),
                    ));
                }
                if let Some(branch) = &task.branch_name
                    && !branch.is_empty()
                {
                    details = details.child(field("Branch", branch.clone()));
                }
                details = details.child(self.relationships_section(window, cx));
                // The coding steps are subtasks of the feature task, so they
                // close the panel, after the ordinary fields and links.
                details = details.child(self.coding_section(&task, window, cx));
                details
            }
        };

        div()
            .id("task-details")
            .relative()
            .size_full()
            .min_w_0()
            .v_flex()
            .p_4()
            .gap_4()
            .bg(rgb(APP_BG))
            // Soft dark edge fully outside the left side so the pane reads
            // as floating above the task list. No border: the shadow alone
            // defines the edge, so there is a single surface, not nested
            // boxes. Fully outside (offset beyond blur) means nothing
            // bleeds back inside the pane.
            .shadow(vec![
                BoxShadow::new(px(-12.), px(0.), hsla(0., 0., 0., 0.12))
                    .blur_radius(px(10.)),
            ])
            .on_click(cx.listener(|_, _, _, cx| {
                cx.stop_propagation();
            }))
            .child(body)
            .when(self.confirming, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .right(px(0.))
                        .bottom(px(0.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(0x000000cc))
                        .child(
                            div()
                                .v_flex()
                                .gap_3()
                                .w(px(260.))
                                .p_4()
                                .rounded_md()
                                .bg(rgb(0x2a2a2a))
                                .border_1()
                                .border_color(rgb(HAIRLINE))
                                .child(
                                    div()
                                        .v_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(rgb(0xe5e5e5))
                                                .child("Discard unsaved changes?"),
                                        )
                                        .child(div().text_xs().text_color(rgb(0xa3a3a3)).child(
                                            "Your title and description edits will be lost.",
                                        )),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("keep-editing")
                                                .ghost()
                                                .label("Keep editing")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cancel_pending(cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("discard-changes")
                                                .danger()
                                                .label("Discard")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.confirm_pending(cx);
                                                })),
                                        ),
                                ),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod coding_tests {
    use super::*;
    use storage::prelude::RecipeNode;

    fn feature(title: &str, description: Option<&str>) -> TaskWithMeta {
        let mut meta = phase_step_task(1, "interview", false);
        meta.task.title = title.to_string();
        meta.task.description = description.map(str::to_owned);
        meta
    }

    fn phase_step_view(node_id: &str, phase: &str, done: bool) -> RunStepView {
        RunStepView {
            task: phase_step_task(2, node_id, done),
            node: RecipeNode {
                id: node_id.to_string(),
                kind: "action".to_string(),
                title: node_id.to_string(),
                description: None,
                ai: true,
                approval: false,
                retrigger_on_reject: false,
                phase: Some(phase.to_string()),
                subtask: true,
                retrigger_node: None,
            },
            outgoing: Vec::new(),
            incoming_event: None,
        }
    }

    fn phase_step_task(id: u64, node_id: &str, done: bool) -> TaskWithMeta {
        TaskWithMeta {
            task: storage::task::Task {
                id,
                title: node_id.to_string(),
                description: None,
                branch_name: None,
                labels: None,
                deadline: None,
                blocked_until: None,
                importance_factor: 1.0,
                urgency_factor: 1.0,
                done,
                completed_at: None,
                created_at: jiff::Timestamp::now(),
                updated_at: jiff::Timestamp::now(),
                parent_id: None,
                source_task_id: None,
                deleted_at: None,
                timezone: None,
                comments: None,
                is_seed: false,
                workflow_run_id: None,
                node_id: Some(node_id.to_string()),
                spec: None,
                spec_path: None,
                subtasks: storage::prelude::Deferred::default(),
                parent: storage::prelude::Deferred::default(),
            },
            direct_tags: Vec::new(),
            inherited_tags: Vec::new(),
            inferred_tags: Vec::new(),
            leaf_tags: Vec::new(),
            blocked: false,
        }
    }

    fn note(kind: &str, body: &str) -> RunNote {
        RunNote::new(kind, "review", "review", body)
    }

    #[test]
    fn interview_prompt_is_a_full_prompt() {
        let task = feature("Add OAuth", Some("Sign in with Google."));
        let prompt = phase_prompt(&task, &[], None, "interview");
        assert!(
            !prompt.starts_with('/'),
            "a pre-filled prompt must not be an unknown slash command (opencode drops \
             unrecognised `/`-prefixed prompts silently): {prompt}"
        );
        assert!(
            prompt.starts_with("You are running an interview"),
            "the interview prompt is expanded in the app: {prompt}"
        );
        assert!(prompt.contains("Request to interview: Add OAuth"), "{prompt}");
        assert!(prompt.contains("Sign in with Google."), "{prompt}");
    }

    #[test]
    fn reopened_interview_carries_the_rejection_notes() {
        let task = feature("Add OAuth", None);
        let notes = vec![note("reject", "Needs a migration test.")];
        let prompt = phase_prompt(&task, &notes, None, "interview");
        assert!(prompt.contains("round 2"), "{prompt}");
        assert!(prompt.contains("Needs a migration test."), "{prompt}");
    }

    #[test]
    fn implement_prompt_names_the_branch_and_the_feedback() {
        let mut task = feature("Add OAuth", None);
        task.task.spec = Some("Spec body".to_string());
        let notes = vec![note("annotation", "Guard the empty-input case.")];
        let prompt = phase_prompt(&task, &notes, Some("feature/7-add-oauth"), "implement");
        assert!(prompt.contains("feature/7-add-oauth"), "{prompt}");
        assert!(prompt.contains("Spec body"), "{prompt}");
        assert!(prompt.contains("Guard the empty-input case."), "{prompt}");
    }

    #[test]
    fn phase_state_prefers_the_open_step() {
        let steps = vec![
            phase_step_view("interview", "interview", true),
            phase_step_view("interview", "interview", false),
        ];
        assert_eq!(phase_state(&steps, "interview"), PhaseState::Active);
        assert_eq!(phase_state(&steps, "merge"), PhaseState::Pending);
        assert_eq!(phase_state(&steps[..1], "interview"), PhaseState::Done);
    }

    #[test]
    fn notes_group_by_cycle_newest_first() {
        let notes = vec![
            note("spec", "first spec"),
            note("reject", "needs work"),
            note("spec", "second spec"),
        ];
        let grouped = notes_by_round(&notes);
        assert_eq!(grouped.len(), 3);
        // Newest first, and the second cycle starts after the rejection.
        assert_eq!(grouped[0].0, 2);
        assert_eq!(grouped[0].1.body, "second spec");
        assert_eq!(grouped[1].0, 1);
        assert_eq!(grouped[1].1.kind, "reject");
        assert_eq!(grouped[2].0, 1);
    }

    #[test]
    fn round_number_counts_rejections() {
        assert_eq!(round_number(&[]), 1);
        let notes = vec![note("reject", "a"), note("spec", "b"), note("reject", "c")];
        assert_eq!(round_number(&notes), 3);
    }

    #[test]
    fn current_step_is_the_open_one() {
        let steps = vec![
            phase_step_view("interview", "interview", true),
            phase_step_view("spec", "spec", true),
            phase_step_view("review", "review", false),
        ];
        assert_eq!(
            current_step(&steps).map(|step| step.node.id.as_str()),
            Some("review")
        );
    }
}

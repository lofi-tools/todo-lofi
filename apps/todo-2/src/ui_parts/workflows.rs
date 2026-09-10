//! Run header/banner: a collapsed strip above the task list showing every
//! active workflow run and its steps. Each step carries the action the
//! engine expects: plain completion, approve/reject (on_result branches),
//! or firing an event wait. Starting a run is one click per recipe.

use gpui::{
    Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Task, Window, div, rgb,
    prelude::FluentBuilder,
};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use storage::prelude::*;

use crate::store::Store;

#[derive(Clone)]
pub enum WorkflowPanelEvent {
    /// A workflow action ran; the task list should refresh (steps may have
    /// been spawned or completed).
    Changed,
}

pub struct WorkflowPanel {
    store: Store,
    recipes: Vec<RecipeMeta>,
    runs: Vec<RunView>,
    _fetch: Option<Task<()>>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "in 3d 4h" / "in 2h" for a future timestamp, "" when already due.
fn wait_label(until: u64) -> String {
    let now = now_secs();
    if until <= now {
        return String::new();
    }
    let secs = until - now;
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    if days > 0 {
        format!("in {days}d {hours}h")
    } else {
        format!("in {hours}h")
    }
}

/// Button label for an `on_result` condition value: "Approve"/"Reject"
/// for the familiar shape, the JSON otherwise.
fn result_label(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map)
            if map.get("approved").and_then(|v| v.as_bool()) == Some(true) =>
        {
            "Approve".to_string()
        }
        serde_json::Value::Object(map)
            if map.get("approved").and_then(|v| v.as_bool()) == Some(false) =>
        {
            "Reject".to_string()
        }
        serde_json::Value::Object(map) if map.is_empty() => "Complete".to_string(),
        other => other.to_string(),
    }
}

impl WorkflowPanel {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            store,
            recipes: Vec::new(),
            runs: Vec::new(),
            _fetch: None,
        };
        panel.refresh(cx);
        panel
    }

    /// Re-fetch recipes and active runs.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let recipes = store.list_recipe_metas(cx).await.unwrap_or_default();
            let runs = store.list_active_run_views(cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.recipes = recipes;
                this.runs = runs;
                this._fetch = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Run a workflow action, then refresh the panel and tell the task
    /// list to reload (steps may have appeared or completed).
    fn run_action(&mut self, action: Task<anyhow::Result<()>>, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = action.await {
                tracing::error!("workflow action failed: {e}");
            }
            let recipes = store.list_recipe_metas(cx).await.unwrap_or_default();
            let runs = store.list_active_run_views(cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.recipes = recipes;
                this.runs = runs;
                this._fetch = None;
                cx.emit(WorkflowPanelEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    fn start_run(&mut self, recipe_id: u64, cx: &mut Context<Self>) {
        let start = self.store.start_workflow_run(recipe_id, cx);
        self.run_action(start, cx);
    }

    fn complete_step(&mut self, task_id: u64, result: serde_json::Value, cx: &mut Context<Self>) {
        let complete = self.store.complete_workflow_step(task_id, result, cx);
        self.run_action(complete, cx);
    }

    fn fire_event(&mut self, run_id: u64, event_name: String, cx: &mut Context<Self>) {
        let fire = self.store.fire_workflow_event(run_id, event_name, cx);
        self.run_action(fire, cx);
    }

    fn cancel_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let cancel = self.store.cancel_workflow_run(run_id, cx);
        self.run_action(cancel, cx);
    }
}

impl EventEmitter<WorkflowPanelEvent> for WorkflowPanel {}

impl Render for WorkflowPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().v_flex().gap_2().child(self.header(cx)).children(
            self.runs
                .iter()
                .map(|view| self.run_card(view, window, cx)),
        )
    }
}

impl WorkflowPanel {
    fn header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let start_buttons: Vec<gpui::AnyElement> = self
            .recipes
            .iter()
            .map(|recipe| {
                let recipe_id = recipe.id;
                let name = recipe.name.clone();
                Button::new(format!("start-{}", recipe.id))
                    .ghost()
                    .compact()
                    .label(format!("▶ {name}"))
                    .tooltip(format!("Start a \"{name}\" run"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_run(recipe_id, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xe5e5e5))
                    .child("Workflows"),
            )
            .children(start_buttons)
            .into_any_element()
    }

    fn run_card(
        &self,
        view: &RunView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let run_id = view.run.id;
        let cancel = Button::new(format!("cancel-run-{run_id}"))
            .ghost()
            .compact()
            .label("Cancel run")
            .tooltip("Cancel this run and hide its steps")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cancel_run(run_id, cx);
            }));
        let steps: Vec<gpui::AnyElement> = view
            .steps
            .iter()
            .map(|step| self.step_row(view, step, window, cx))
            .collect();
        div()
            .border_1()
            .border_color(rgb(0x2f2f2f))
            .rounded_md()
            .p_2()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xe5e5e5))
                            .child(view.recipe_name.clone()),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x8a8a8a))
                                    .child(view.run.status.clone()),
                            )
                            .child(cancel),
                    ),
            )
            .children(steps)
            .into_any_element()
    }

    fn step_row(
        &self,
        view: &RunView,
        step: &RunStepView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let done = step.task.done;
        let waiting = step
            .task
            .blocked_until
            .is_some_and(|until| until > now_secs());

        let title = if done {
            format!("✓ {}", step.task.title)
        } else if step.node.kind == "event" {
            format!("⏳ {}", step.task.title)
        } else {
            step.task.title.clone()
        };
        let mut row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(if done { rgb(0x6b6b6b) } else { rgb(0xd4d4d4) })
                    .child(title),
            )
            .when_some(
                step.task.blocked_until.and_then(|until| {
                    waiting.then(|| wait_label(until))
                }),
                |this, label| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8a8a8a))
                            .child(format!("({label})")),
                    )
                },
            );

        if !done {
            if let Some(event_edge) = &step.incoming_event
                && let Some(event_name) = event_edge
                    .condition_value
                    .as_ref()
                    .and_then(|v| v.as_str())
            {
                let run_id = view.run.id;
                let event_name = event_name.to_string();
                row = row.child(
                    Button::new(format!("fire-{}-{}", run_id, step.task.id))
                        .ghost()
                        .compact()
                        .label(format!("Fire: {event_name}"))
                        .tooltip("Resolve this wait (webhook or button)")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.fire_event(run_id, event_name.clone(), cx);
                        })),
                );
            } else if step.outgoing.iter().any(|e| e.condition_type == "on_result") {
                let values: Vec<serde_json::Value> = step
                    .outgoing
                    .iter()
                    .filter(|e| e.condition_type == "on_result")
                    .filter_map(|e| e.condition_value.clone())
                    .collect();
                let any_approval = values.iter().any(|v| {
                    v.as_object().is_some_and(|m| m.contains_key("approved"))
                });
                if any_approval {
                    for value in values {
                        let label = result_label(&value);
                        let task_id = step.task.id;
                        row = row.child(
                            Button::new(format!("result-{}-{}-{}", view.run.id, task_id, label))
                                .ghost()
                                .compact()
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.complete_step(task_id, value.clone(), cx);
                                })),
                        );
                    }
                } else {
                    // "Continue" edges with an empty condition: plain tick.
                    let task_id = step.task.id;
                    row = row.child(
                        Button::new(format!("continue-{}-{}", view.run.id, task_id))
                            .ghost()
                            .compact()
                            .label("Complete")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.complete_step(task_id, serde_json::json!({}), cx);
                            })),
                    );
                }
            } else if !waiting {
                let task_id = step.task.id;
                row = row.child(
                    Button::new(format!("tick-{}-{}", view.run.id, task_id))
                        .ghost()
                        .compact()
                        .label("Complete")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.complete_step(task_id, serde_json::Value::Null, cx);
                        })),
                );
            }
        }
        row.into_any_element()
    }
}
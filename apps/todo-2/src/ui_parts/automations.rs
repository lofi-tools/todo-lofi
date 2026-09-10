//! Automations catalog: every workflow recipe presented as an automation
//! the user can enable (start a run) or disable (cancel its active runs).
//! Enabling spawns the recipe's first steps as ordinary tasks; disabling
//! tombstones them and hides the run from the Workflows panel.

use gpui::{
    Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Task, Window, div, rgb,
    prelude::FluentBuilder,
};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use storage::prelude::*;

use crate::store::Store;

#[derive(Clone)]
pub enum AutomationsEvent {
    /// An automation was enabled or disabled; the task list should refresh
    /// (steps may have been spawned or tombstoned).
    Changed,
}

pub struct AutomationsPanel {
    store: Store,
    automations: Vec<RecipeMeta>,
    _fetch: Option<Task<()>>,
}

impl AutomationsPanel {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            store,
            automations: Vec::new(),
            _fetch: None,
        };
        panel.refresh(cx);
        panel
    }

    /// Re-fetch the automation list (name, description, active runs).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let automations = store.list_recipe_metas(cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.automations = automations;
                this._fetch = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Run an action that yields (), then re-fetch and tell the task list
    /// to reload (enabling spawns steps, disabling tombstones them).
    fn run_action(&mut self, action: Task<anyhow::Result<()>>, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = action.await {
                tracing::error!("automation action failed: {e}");
            }
            let automations = store.list_recipe_metas(cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.automations = automations;
                this._fetch = None;
                cx.emit(AutomationsEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    fn enable(&mut self, recipe_id: u64, cx: &mut Context<Self>) {
        let start = self.store.start_workflow_run(recipe_id, cx);
        self.run_action(start, cx);
    }

    fn disable(&mut self, recipe_id: u64, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let disable = self.store.disable_automation(recipe_id, cx);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            match disable.await {
                Ok(0) => tracing::warn!(recipe_id, "disable: no active runs"),
                Ok(cancelled) => tracing::info!(recipe_id, cancelled, "automation disabled"),
                Err(e) => tracing::error!("failed to disable automation: {e}"),
            }
            let automations = store.list_recipe_metas(cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.automations = automations;
                this._fetch = None;
                cx.emit(AutomationsEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }
}

impl EventEmitter<AutomationsEvent> for AutomationsPanel {}

impl Render for AutomationsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            // Long catalogs scroll; flex-1 gives the scrollable a definite
            // height inside the panel column.
            .overflow_y_scrollbar()
            .v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xe5e5e5))
                    .child("Automations"),
            )
            .children(self.automations.iter().map(|meta| self.automation_card(meta, cx)))
    }
}

impl AutomationsPanel {
    fn automation_card(
        &self,
        meta: &RecipeMeta,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let recipe_id = meta.id;
        let running = meta.active_runs > 0;
        let toggle: gpui::AnyElement = if running {
            Button::new(format!("disable-{}", recipe_id))
                .ghost()
                .compact()
                .label("Disable")
                .tooltip("Cancel this automation's active runs and hide their steps")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.disable(recipe_id, cx);
                }))
                .into_any_element()
        } else {
            Button::new(format!("enable-{}", recipe_id))
                .ghost()
                .compact()
                .label("Enable")
                .tooltip("Start a run: its steps appear in the task list")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enable(recipe_id, cx);
                }))
                .into_any_element()
        };
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
                            .child(meta.name.clone()),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .when(running, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x6b9e6b))
                                        .child("Running"),
                                )
                            })
                            .child(toggle),
                    ),
            )
            .when_some(meta.description.clone(), |this, description| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x8a8a8a))
                        .child(description),
                )
            })
            .into_any_element()
    }
}
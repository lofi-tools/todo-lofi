//! Automations catalog: every workflow recipe presented as an automation
//! the user can enable (start a run) or disable (cancel its active runs).
//! Enabling spawns the recipe's first steps as ordinary tasks; disabling
//! tombstones them and hides the run from the Workflows panel.

use gpui::{
    Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Task, Window, div, px, rgb,
    prelude::FluentBuilder,
};
use gpui_component::{Sizable, Size, StyledExt};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use storage::prelude::*;

use crate::store::Store;
use crate::theme::APP_BG;

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

    /// Run an action, then re-fetch and tell the task list to reload
    /// (enabling spawns steps, disabling tombstones them). The action's
    /// success value (if any) is discarded.
    fn run_action<T: Send + 'static>(
        &mut self,
        action: Task<anyhow::Result<T>>,
        cx: &mut Context<Self>,
    ) {
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

    fn enable(&mut self, recipe_id: u64, managed: bool, cx: &mut Context<Self>) {
        // Managed-tag automations enable by creating their tag (their
        // panel generates content on demand), not by starting a run.
        if managed {
            let enable = self.store.enable_managed_recipe(recipe_id, cx);
            self.run_action(enable, cx);
        } else {
            let start = self.store.start_workflow_run(recipe_id, cx);
            self.run_action(start, cx);
        }
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
            .h_full()
            // Same surface as the task list and integrations panels.
            .bg(rgb(APP_BG))
            // Long catalogs scroll; flex-1 gives the scrollable a definite
            // height inside the panel column.
            .overflow_y_scrollbar()
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().text_xl().font_semibold().child("Automations"))
                    .children(self.automations.iter().map(|meta| self.automation_card(meta, cx))),
            )
    }
}

impl AutomationsPanel {
    /// Subtle card shell for one automation, matching the integrations
    /// panel's card treatment.
    fn automation_card(
        &self,
        meta: &RecipeMeta,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let recipe_id = meta.id;
        let running = meta.active_runs > 0;
        let managed = meta.managed_tag.is_some();
        let enabled = if managed { meta.managed_enabled } else { running };
        let toggle: gpui::AnyElement = if enabled && managed {
            Button::new(format!("disable-{}", recipe_id))
                .ghost()
                .compact()
                .label("Disable")
                .tooltip("Remove this automation's tag and its content")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.disable(recipe_id, cx);
                }))
                .into_any_element()
        } else if managed {
            Button::new(format!("enable-{}", recipe_id))
                .ghost()
                .compact()
                .label("Enable")
                .tooltip("Create the tag with its special panel")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enable(recipe_id, true, cx);
                }))
                .into_any_element()
        } else if running {
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
                    this.enable(recipe_id, false, cx);
                }))
                .into_any_element()
        };
        div()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x2e2e2e))
            .bg(rgb(0x232323))
            .p_4()
            .h_flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(40.))
                    .h(px(40.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgb(0x1e1e1e))
                    .child(
                        gpui_component::Icon::new(gpui_component_assets::IconName::Bot)
                            .with_size(Size::Large),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .gap_0p5()
                    .child(div().font_semibold().child(meta.name.clone()))
                    .when_some(meta.description.clone(), |this, description| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xa3a3a3))
                                .child(description),
                        )
                    }),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .when(enabled, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x4ade80))
                                .child(if managed { "Enabled" } else { "Running" }),
                        )
                    })
                    .child(toggle),
            )
            .into_any_element()
    }
}
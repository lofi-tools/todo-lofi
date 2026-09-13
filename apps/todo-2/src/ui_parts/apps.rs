//! Per-app settings, embedded in the Automations and Integrations panels.
//!
//! An *app* is anything that manages content on the user's behalf: a workflow
//! automation or an integration such as Todoist. There is no standalone Apps
//! page: each app's settings render inside its own card, revealed by the gear
//! button to the right of the app's title.
//!
//! The block is a list of settings, one per line:
//!
//! - **Tags** it manages: one line per binding, with a capture toggle (does a
//!   new task in that tag propagate to the app?) and a detach action.
//! - **Add a tag**: a compact tag list, so the app can be pointed at a tag the
//!   user already keeps, plus what to do with new tasks there.
//! - **Active runs**: stop an automation's runs.
//! - **Disable**: uninstall the app everywhere, asking whether its unfinished
//!   items should go too. Completed and user-edited items always survive.

use gpui::{
    AnyElement, Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, Size, StyledExt};
use std::collections::HashMap;
use storage::prelude::*;

use crate::store::Store;
use crate::theme::{CARD_BG, DANGER, HAIRLINE, SUCCESS, TEXT_FAINT, TEXT_MUTED};

#[derive(Clone)]
pub enum AppSettingsEvent {
    /// An app was attached, detached, or disabled: the tag tree and the task
    /// lists may both have changed.
    Changed,
}

/// One app with the tags it is bound to.
struct InstalledApp {
    app: App,
    bindings: Vec<(AppTagBinding, Tag)>,
}

pub struct AppSettings {
    store: Store,
    apps: Vec<InstalledApp>,
    /// Every known tag, for the attach picker.
    tags: Vec<Tag>,
    /// Recipe summaries, used to map a recipe app back to its recipe.
    recipes: Vec<RecipeMeta>,
    /// Bindings keyed by tag id as `(app_id, role)`, for the picker's
    /// eligibility rules.
    bindings_by_tag: HashMap<u64, Vec<(u64, BindingRole)>>,
    app_labels: HashMap<u64, String>,
    /// The app whose settings block is expanded (at most one at a time).
    expanded: Option<u64>,
    /// The app whose tag list is open.
    picking: Option<u64>,
    /// Whether an attachment made from the open tag list captures new tasks.
    attach_capture: bool,
    status: Option<String>,
    _load: Option<Task<()>>,
}

impl AppSettings {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let mut settings = Self {
            store,
            apps: Vec::new(),
            tags: Vec::new(),
            recipes: Vec::new(),
            bindings_by_tag: HashMap::new(),
            app_labels: HashMap::new(),
            expanded: None,
            picking: None,
            attach_capture: false,
            status: None,
            _load: None,
        };
        settings.refresh(cx);
        settings
    }

    /// Reload the app list, the tag list, and the recipe summaries.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let apps = store.list_apps(cx).await;
            let tags = store.list_tags(cx).await;
            let recipes = store.list_recipe_metas(cx).await;
            this.update(cx, |this, cx| {
                match (apps, tags, recipes) {
                    (Ok(apps), Ok(tags), Ok(recipes)) => this.install(apps, tags, recipes),
                    (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                        this.status = Some(format!("Could not load app settings: {error}"));
                    }
                }
                this._load = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn install(
        &mut self,
        apps: Vec<(App, Vec<(AppTagBinding, Tag)>)>,
        tags: Vec<Tag>,
        recipes: Vec<RecipeMeta>,
    ) {
        self.bindings_by_tag.clear();
        self.app_labels.clear();
        let mut installed = Vec::with_capacity(apps.len());
        for (app, bindings) in apps {
            self.app_labels.insert(app.id, app.label.clone());
            for (binding, _) in &bindings {
                self.bindings_by_tag
                    .entry(binding.tag_id)
                    .or_default()
                    .push((app.id, binding.role));
            }
            installed.push(InstalledApp { app, bindings });
        }
        self.apps = installed;
        self.tags = tags;
        self.recipes = recipes;
    }

    // -- lookups used by the panels that embed these settings --------------

    pub fn app(&self, app_id: u64) -> Option<&App> {
        self.apps
            .iter()
            .find(|installed| installed.app.id == app_id)
            .map(|installed| &installed.app)
    }

    pub fn app_for_recipe_slug(&self, slug: &str) -> Option<&App> {
        self.apps
            .iter()
            .find(|installed| installed.app.kind == "recipe" && installed.app.slug == slug)
            .map(|installed| &installed.app)
    }

    /// Whether this app can own tags. Integrations capture the tags they are
    /// linked to; recipes can own tags when they declare one or when the user
    /// has already attached them somewhere. The builtin demo content is
    /// shipped data, not something an app manages, so it never has settings.
    pub fn manages_tags(&self, app_id: u64) -> bool {
        let Some(app) = self.app(app_id) else {
            return false;
        };
        match app.kind.as_str() {
            "integration" => true,
            "recipe" => {
                self.binding_count(app_id) > 0
                    || self
                        .recipes
                        .iter()
                        .find(|meta| meta.slug == app.slug)
                        .is_some_and(|meta| meta.managed_tag.is_some())
            }
            _ => false,
        }
    }

    /// How many tags the app currently holds.
    pub fn binding_count(&self, app_id: u64) -> usize {
        self.apps
            .iter()
            .find(|installed| installed.app.id == app_id)
            .map(|installed| installed.bindings.len())
            .unwrap_or(0)
    }

    /// The recipe id behind a recipe app, for the "create its own tag" offer.
    fn recipe_id_for(&self, app_id: u64) -> Option<u64> {
        let app = self.app(app_id)?;
        self.recipes
            .iter()
            .find(|meta| meta.slug == app.slug)
            .map(|meta| meta.id)
    }

    pub fn is_expanded(&self, app_id: u64) -> bool {
        self.expanded == Some(app_id)
    }

    /// Open or close one app's settings. Only one is open at a time, and the
    /// tag list closes with it.
    pub fn toggle_expanded(&mut self, app_id: u64, cx: &mut Context<Self>) {
        self.expanded = if self.expanded == Some(app_id) {
            None
        } else {
            Some(app_id)
        };
        self.picking = None;
        cx.notify();
    }

    /// Enable flow for an app that manages tags: open its settings with the
    /// tag list already out, because choosing the tag is the mandatory
    /// setting before the app can do anything.
    pub fn begin_enable(&mut self, app_id: u64, cx: &mut Context<Self>) {
        self.expanded = Some(app_id);
        self.picking = Some(app_id);
        self.attach_capture = self
            .app(app_id)
            .is_some_and(|app| app.kind == "integration");
        cx.notify();
    }

    // -- actions -----------------------------------------------------------

    /// Run an ownership action, then reload and tell the rest of the app
    /// that managed content may have changed.
    fn run_action<T: Send + 'static>(
        &mut self,
        action: Task<anyhow::Result<T>>,
        on_success: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let outcome = action.await;
            let apps = store.list_apps(cx).await;
            let tags = store.list_tags(cx).await;
            let recipes = store.list_recipe_metas(cx).await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(_) => this.status = on_success,
                    Err(error) => this.status = Some(format!("Failed: {error}")),
                }
                match (apps, tags, recipes) {
                    (Ok(apps), Ok(tags), Ok(recipes)) => this.install(apps, tags, recipes),
                    (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                        this.status = Some(format!("Could not refresh app settings: {error}"));
                    }
                }
                this._load = None;
                cx.emit(AppSettingsEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    fn toggle_picker(&mut self, app_id: u64, cx: &mut Context<Self>) {
        self.picking = if self.picking == Some(app_id) {
            None
        } else {
            Some(app_id)
        };
        // Integrations propagate tasks; automations generate them, so
        // capture defaults off for recipes.
        self.attach_capture = self
            .app(app_id)
            .is_some_and(|app| app.kind == "integration");
        cx.notify();
    }

    fn set_capture(&mut self, app_id: u64, tag_id: u64, capture: bool, cx: &mut Context<Self>) {
        let action = self.store.set_binding_capture(app_id, tag_id, capture, cx);
        let note = if capture {
            "New tasks in that tag will be captured."
        } else {
            "New tasks in that tag stay local."
        };
        self.run_action(action, Some(note.to_string()), cx);
    }

    fn attach(&mut self, app_id: u64, tag_id: u64, cx: &mut Context<Self>) {
        let capture = self.attach_capture;
        self.picking = None;
        let action =
            self.store
                .attach_app_to_tag(app_id, tag_id, BindingRole::Partial, capture, cx);
        self.run_action(action, Some("Attached to the tag.".to_string()), cx);
    }

    /// Give an automation its own tag (the tag the recipe declares): create
    /// it, provision the app's sections there, and install the app.
    fn enable_with_own_tag(&mut self, recipe_id: u64, cx: &mut Context<Self>) {
        self.picking = None;
        let action = self.store.enable_managed_recipe(recipe_id, cx);
        self.run_action(action, Some("Enabled.".to_string()), cx);
    }

    fn detach(&mut self, app_id: u64, tag_id: u64, cx: &mut Context<Self>) {
        let action = self.store.detach_app_from_tag(app_id, tag_id, cx);
        self.run_action(action, Some("Detached from the tag.".to_string()), cx);
    }

    /// Stop a plain automation: cancel its active runs and hide their steps.
    pub fn stop_runs(&mut self, recipe_id: u64, cx: &mut Context<Self>) {
        let action = self.store.disable_automation(recipe_id, cx);
        self.run_action(action, Some("Stopped.".to_string()), cx);
    }

    fn disable(&mut self, app_id: u64, remove_owned_items: bool, cx: &mut Context<Self>) {
        let action = self.store.remove_app(app_id, remove_owned_items, cx);
        let note = if remove_owned_items {
            "Disabled the app and deleted its unfinished items."
        } else {
            "Disabled the app; its items are yours now."
        };
        self.run_action(action, Some(note.to_string()), cx);
    }

    /// Why `app_id` may not take `tag_id`, or `None` when it may.
    fn attach_blocker(&self, app_id: u64, tag_id: u64) -> Option<String> {
        let tag = self.tags.iter().find(|tag| tag.id == tag_id)?;
        if tag.is_project() {
            return Some("Backs a local folder".to_string());
        }
        let Some(bindings) = self.bindings_by_tag.get(&tag_id) else {
            return None;
        };
        if bindings.iter().any(|(app, _)| *app == app_id) {
            return Some("Already attached".to_string());
        }
        for (other, role) in bindings {
            if *role == BindingRole::FullTag {
                let label = self
                    .app_labels
                    .get(other)
                    .cloned()
                    .unwrap_or_else(|| "another app".to_string());
                return Some(format!("Owned by {label}"));
            }
        }
        None
    }

    /// Ask whether the app's unfinished items should go with it. Both choices
    /// run in one transaction; completed and edited items always survive.
    fn confirm_disable(
        &mut self,
        app_id: u64,
        label: String,
        bound: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let settings = settings.clone();
            let label = label.clone();
            let bound = bound.clone();
            dialog
                .title(format!("Disable {label}?"))
                .content(move |content, _window, _cx| {
                    let summary = if bound.is_empty() {
                        "It is not managing any tags right now.".to_string()
                    } else {
                        format!(
                            "It manages {}.",
                            bound
                                .iter()
                                .map(|tag| format!("#{tag}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let keep = settings.clone();
                    let with_items = settings.clone();
                    content.child(
                        div()
                            .v_flex()
                            .gap_3()
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(format!(
                                "{summary} It stops managing everything, and completed or \
                                 edited items are always kept."
                            )))
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("disable-keep-items")
                                            .compact()
                                            .label("Disable, keep its items")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                if let Err(error) = keep.update(cx, |this, cx| {
                                                    this.disable(app_id, false, cx)
                                                }) {
                                                    tracing::error!(
                                                        %error,
                                                        "could not disable the app"
                                                    );
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("disable-with-items")
                                            .compact()
                                            .label("Disable and delete unfinished items")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                if let Err(error) = with_items.update(cx, |this, cx| {
                                                    this.disable(app_id, true, cx)
                                                }) {
                                                    tracing::error!(
                                                        %error,
                                                        "could not disable the app"
                                                    );
                                                }
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    /// The gear that expands one app's settings. The embedding panel renders
    /// it to the right of the app's title.
    pub fn gear_button(
        &self,
        app_id: u64,
        caption: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.is_expanded(app_id);
        Button::new(format!("app-settings-{app_id}"))
            .ghost()
            .compact()
            .with_size(Size::Small)
            .icon(gpui_component_assets::IconName::Settings)
            .tooltip(if expanded {
                "Hide settings".to_string()
            } else {
                format!("Settings for {caption}")
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_expanded(app_id, cx);
            }))
            .into_any_element()
    }
}

impl EventEmitter<AppSettingsEvent> for AppSettings {}

impl Render for AppSettings {
    /// Settings are rendered through `settings_block` by the panels that
    /// embed them; a bare render shows nothing.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl AppSettings {
    /// The settings block for one app, or nothing when it is collapsed.
    /// `active_runs` drives the only setting a plain automation has.
    pub fn settings_block(
        &mut self,
        app_id: u64,
        active_runs: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.is_expanded(app_id) {
            return div().into_any_element();
        }
        let manages_tags = self.manages_tags(app_id);
        let Some(app) = self.app(app_id).cloned() else {
            return div().into_any_element();
        };
        if !manages_tags && active_runs == 0 {
            return div().into_any_element();
        }
        let mut lines: Vec<AnyElement> = Vec::new();

        if manages_tags {
            let bindings: Vec<(u64, String, bool)> = self
                .apps
                .iter()
                .find(|installed| installed.app.id == app_id)
                .map(|installed| {
                    installed
                        .bindings
                        .iter()
                        .map(|(binding, tag)| (tag.id, tag.label(), binding.capture_new_tasks))
                        .collect()
                })
                .unwrap_or_default();
            if bindings.is_empty() {
                lines.push(self.static_line("Tags", "None yet"));
            }
            for (tag_id, label, capture) in bindings {
                lines.push(self.binding_line(app_id, tag_id, label, capture, cx));
            }
            lines.push(self.add_tag_line(app_id, &app.label, cx));
        }

        if active_runs > 0 {
            let recipe_id = self.recipe_id_for(app_id);
            if let Some(recipe_id) = recipe_id {
                let label = if active_runs == 1 {
                    "1 active run".to_string()
                } else {
                    format!("{active_runs} active runs")
                };
                lines.push(self.line(
                    "Active runs",
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(SUCCESS))
                                .child(label),
                        )
                        .child(
                            Button::new(format!("stop-runs-{app_id}"))
                                .ghost()
                                .compact()
                                .with_size(Size::Small)
                                .label("Stop")
                                .tooltip("Cancel this automation's active runs and hide its steps")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.stop_runs(recipe_id, cx);
                                })),
                        )
                        .into_any_element(),
                ));
            }
        }

        if manages_tags {
            let bound: Vec<String> = self
                .apps
                .iter()
                .find(|installed| installed.app.id == app_id)
                .map(|installed| {
                    installed
                        .bindings
                        .iter()
                        .map(|(_, tag)| tag.label())
                        .collect()
                })
                .unwrap_or_default();
            let label = app.label.clone();
            lines.push(self.line(
                "This app",
                Button::new(format!("disable-app-{app_id}"))
                    .ghost()
                    .compact()
                    .with_size(Size::Small)
                    .text_color(rgb(DANGER))
                    .label("Disable…")
                    .tooltip("Stop managing everything this app owns")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let label = label.clone();
                        let bound = bound.clone();
                        this.confirm_disable(app_id, label, bound, window, cx);
                    }))
                    .into_any_element(),
            ));
        }

        if lines.is_empty() {
            return div().into_any_element();
        }

        let mut block = div()
            .v_flex()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(HAIRLINE))
            .bg(rgb(CARD_BG))
            .p_3()
            .children(lines);
        if let Some(status) = self.status.clone() {
            block = block.child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(status));
        }
        block.into_any_element()
    }

    /// One settings line: a label, then the control on the right.
    fn line(&self, label: &str, control: AnyElement) -> AnyElement {
        div()
            .h_flex()
            .items_center()
            .gap_3()
            .min_h(px(28.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(0xd4d4d4))
                    .child(label.to_string()),
            )
            .child(control)
            .into_any_element()
    }

    /// A settings line whose right side is plain text rather than a control.
    fn static_line(&self, label: &str, value: &str) -> AnyElement {
        self.line(
            label,
            div()
                .text_xs()
                .text_color(rgb(TEXT_FAINT))
                .child(value.to_string())
                .into_any_element(),
        )
    }

    /// One managed tag: its name, a capture toggle and a detach action.
    fn binding_line(
        &mut self,
        app_id: u64,
        tag_id: u64,
        label: String,
        capture: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let controls = div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(self.toggle_chip(
                format!("capture-{app_id}-{tag_id}"),
                if capture {
                    "New tasks: in the app too"
                } else {
                    "New tasks: here only"
                },
                capture,
                cx.listener(move |this, _, _, cx| {
                    this.set_capture(app_id, tag_id, !capture, cx);
                }),
            ))
            .child(
                Button::new(format!("detach-{app_id}-{tag_id}"))
                    .ghost()
                    .compact()
                    .with_size(Size::Small)
                    .label("Detach")
                    .tooltip("Stop managing this tag here")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.detach(app_id, tag_id, cx);
                    })),
            );
        self.line(format!("#{label}").as_str(), controls.into_any_element())
    }

    /// The "add a tag" line, with the tag list open when picked.
    fn add_tag_line(&mut self, app_id: u64, app_label: &str, cx: &mut Context<Self>) -> AnyElement {
        let picking = self.picking == Some(app_id);
        let picker = div()
            .v_flex()
            .items_end()
            .gap_1()
            .child(
                Button::new(format!("pick-tag-{app_id}"))
                    .ghost()
                    .compact()
                    .with_size(Size::Small)
                    .label(if picking { "Cancel" } else { "Choose a tag…" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_picker(app_id, cx);
                    })),
            )
            .when(picking, |this| this.child(self.tag_list(app_id, app_label, cx)));
        self.line("Add a tag", picker.into_any_element())
    }

    /// The list of eligible tags, under the picker button.
    fn tag_list(&mut self, app_id: u64, app_label: &str, cx: &mut Context<Self>) -> AnyElement {
        let own_tag = self.recipe_id_for(app_id);
        let mut tags: Vec<(u64, String)> = self
            .tags
            .iter()
            .map(|tag| (tag.id, tag.label()))
            .collect();
        tags.sort_by_key(|(_, label)| label.to_lowercase());

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(recipe_id) = own_tag {
            rows.push(
                div()
                    .id(format!("new-tag-{app_id}"))
                    .w_full()
                    .h_flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .text_color(rgb(0xdbe6f5))
                    .hover(|this| this.bg(rgb(0x2a2a2a)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.enable_with_own_tag(recipe_id, cx);
                    }))
                    .child(format!("Create a new \"{app_label}\" tag"))
                    .into_any_element(),
            );
        }
        for (tag_id, label) in tags {
            let blocker = self.attach_blocker(app_id, tag_id);
            let eligible = blocker.is_none();
            let row = div()
                .id(format!("pick-tag-{app_id}-{tag_id}"))
                .w_full()
                .h_flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .when(!eligible, |this| this.opacity(0.5))
                .when(eligible, |this| {
                    this.hover(|this| this.bg(rgb(0x2a2a2a)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.attach(app_id, tag_id, cx);
                        }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("#{label}")),
                )
                .when_some(blocker, |this, reason| {
                    this.child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(reason))
                })
                .into_any_element();
            rows.push(row);
        }

        let capture = self.attach_capture;
        div()
            .w(px(280.))
            .max_h(px(240.))
            .overflow_y_scrollbar()
            .rounded_md()
            .border_1()
            .border_color(rgb(HAIRLINE))
            .bg(rgb(CARD_BG))
            .p_2()
            .v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child("New tasks in that tag"),
            )
            .child(self.toggle_chip(
                format!("new-capture-{app_id}"),
                if capture {
                    "Created in the app too"
                } else {
                    "Stay here only"
                },
                capture,
                cx.listener(|this, _, _, cx| {
                    this.attach_capture = !this.attach_capture;
                    cx.notify();
                }),
            ))
            .child(div().text_xs().text_color(rgb(TEXT_FAINT)).child("Add to"))
            .children(rows)
            .into_any_element()
    }

    /// A small on/off chip: the capture toggles.
    fn toggle_chip(
        &self,
        id: String,
        label: &str,
        on: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(if on { rgb(0x4a6fa5) } else { rgb(HAIRLINE) })
            .bg(if on { rgb(0x2f4057) } else { rgb(0x242424) })
            .text_xs()
            .text_color(if on { rgb(0xdbe6f5) } else { rgb(TEXT_MUTED) })
            .hover(|this| this.bg(rgb(0x2a2a2a)))
            .on_click(on_click)
            .child(label.to_string())
            .into_any_element()
    }
}

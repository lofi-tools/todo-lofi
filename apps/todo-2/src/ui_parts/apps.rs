//! Apps settings panel.
//!
//! An *app* is anything that manages content on the user's behalf: a
//! workflow automation, an integration such as Todoist, or the built-in
//! demo data. This panel lists every app with the tags it manages and
//! offers the three ownership actions:
//!
//! - **Attach** an app to a tag the user already keeps (the "I have a
//!   travel checklist in Todoist already" case), which provisions the
//!   app's sections there and can opt the tag into capture;
//! - **Detach** one tag, whose sections are downgraded back to ordinary
//!   sections;
//! - **Remove** the app everywhere, asking whether its unfinished items
//!   go too. Completed and user-edited items always survive.

use gpui::{
    Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, Size, StyledExt};
use std::collections::HashMap;
use storage::prelude::*;

use crate::store::Store;
use crate::theme::{APP_BG, CARD_BG, DANGER, HAIRLINE, SUCCESS, TEXT_FAINT, TEXT_MUTED};

#[derive(Clone)]
pub enum AppsEvent {
    /// An app was attached, detached, or removed: the tag tree and the task
    /// lists may both have changed.
    Changed,
}

/// One app with the tags it is bound to, as the panel renders it.
struct AppCard {
    app: App,
    bindings: Vec<(AppTagBinding, Tag)>,
}

pub struct AppsPanel {
    store: Store,
    apps: Vec<AppCard>,
    /// Every known tag, for the attach picker.
    tags: Vec<Tag>,
    /// Recipe summaries, used to tell which recipe apps manage tags at all.
    recipes: Vec<RecipeMeta>,
    /// All bindings keyed by tag id as `(app_id, role)`, for the attach
    /// picker's eligibility rules.
    bindings_by_tag: HashMap<u64, Vec<(u64, BindingRole)>>,
    app_labels: HashMap<u64, String>,
    /// The app whose attach picker is open.
    attaching: Option<u64>,
    /// Whether the open picker also captures new tasks in the tag.
    attach_capture: bool,
    status: Option<String>,
    _load: Option<Task<()>>,
}

impl AppsPanel {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            store,
            apps: Vec::new(),
            tags: Vec::new(),
            recipes: Vec::new(),
            bindings_by_tag: HashMap::new(),
            app_labels: HashMap::new(),
            attaching: None,
            attach_capture: false,
            status: None,
            _load: None,
        };
        panel.refresh(cx);
        panel
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
                        this.status = Some(format!("Could not load apps: {error}"));
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
        let mut cards = Vec::with_capacity(apps.len());
        for (app, bindings) in apps {
            self.app_labels.insert(app.id, app.label.clone());
            for (binding, _) in &bindings {
                self.bindings_by_tag
                    .entry(binding.tag_id)
                    .or_default()
                    .push((app.id, binding.role));
            }
            cards.push(AppCard { app, bindings });
        }
        self.apps = cards;
        self.tags = tags;
        self.recipes = recipes;
    }

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
                        this.status = Some(format!("Could not refresh the app list: {error}"));
                    }
                }
                this._load = None;
                cx.emit(AppsEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Whether the app can take over tags at all. Integrations capture the
    /// projects they are linked to; recipes only count when they declare a
    /// managed tag.
    fn app_manages_tags(&self, app: &App) -> bool {
        match app.kind.as_str() {
            "integration" => true,
            "recipe" => self
                .recipes
                .iter()
                .find(|meta| meta.slug == app.slug)
                .is_some_and(|meta| meta.managed_tag.is_some()),
            _ => false,
        }
    }

    /// Why `app_id` may not take `tag_id`, or `None` when it may.
    fn attach_blocker(&self, app_id: u64, tag_id: u64) -> Option<String> {
        let tag = self.tags.iter().find(|tag| tag.id == tag_id)?;
        if tag.is_project() {
            return Some("Backs a local folder; apps may not manage it".to_string());
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
                return Some(format!("Owned by {label}; detach it first"));
            }
        }
        None
    }

    fn toggle_attach(&mut self, app_id: u64, cx: &mut Context<Self>) {
        self.attaching = if self.attaching == Some(app_id) {
            None
        } else {
            Some(app_id)
        };
        // Integrations are attached to propagate tasks; automations are
        // attached to generate them, so capture defaults off for recipes.
        self.attach_capture = self
            .apps
            .iter()
            .find(|card| card.app.id == app_id)
            .is_some_and(|card| card.app.kind == "integration");
        cx.notify();
    }

    fn attach(&mut self, app_id: u64, tag_id: u64, cx: &mut Context<Self>) {
        let capture = self.attach_capture;
        self.attaching = None;
        let action =
            self.store
                .attach_app_to_tag(app_id, tag_id, BindingRole::Partial, capture, cx);
        self.run_action(action, Some("Attached to the tag.".to_string()), cx);
    }

    fn detach(&mut self, app_id: u64, tag_id: u64, cx: &mut Context<Self>) {
        let action = self.store.detach_app_from_tag(app_id, tag_id, cx);
        self.run_action(action, Some("Detached from the tag.".to_string()), cx);
    }

    fn remove(&mut self, app_id: u64, remove_owned_items: bool, cx: &mut Context<Self>) {
        let action = self.store.remove_app(app_id, remove_owned_items, cx);
        let note = if remove_owned_items {
            "Removed the app and its unfinished items."
        } else {
            "Removed the app; its items are yours now."
        };
        self.run_action(action, Some(note.to_string()), cx);
    }

    /// Ask whether the app's items should go with it. Both choices run in
    /// one transaction; completed and user-edited items always survive.
    fn confirm_remove(
        &mut self,
        app_id: u64,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bound: Vec<String> = self
            .apps
            .iter()
            .find(|card| card.app.id == app_id)
            .map(|card| card.bindings.iter().map(|(_, tag)| tag.label()).collect())
            .unwrap_or_default();
        let panel = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let panel = panel.clone();
            let label = label.clone();
            let bound = bound.clone();
            dialog
                .title(format!("Remove {label}?"))
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
                    let keep = panel.clone();
                    let with_items = panel.clone();
                    content.child(
                        div()
                            .v_flex()
                            .gap_3()
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(format!(
                                "{summary} Completed and edited items are always kept."
                            )))
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("remove-keep-items")
                                            .compact()
                                            .label("Remove, keep its items")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                if let Err(error) = keep.update(cx, |this, cx| {
                                                    this.remove(app_id, false, cx)
                                                }) {
                                                    tracing::error!(
                                                        %error,
                                                        "could not open the app removal"
                                                    );
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("remove-with-items")
                                            .compact()
                                            .label("Remove and delete unfinished items")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                if let Err(error) = with_items
                                                    .update(cx, |this, cx| {
                                                        this.remove(app_id, true, cx)
                                                    })
                                                {
                                                    tracing::error!(
                                                        %error,
                                                        "could not open the app removal"
                                                    );
                                                }
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }
}

impl EventEmitter<AppsEvent> for AppsPanel {}

impl Render for AppsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .h_full()
            // Same surface as the task list and the other settings panels.
            .bg(rgb(APP_BG))
            .overflow_y_scrollbar()
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().text_xl().font_semibold().child("Apps"))
                    .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(
                        "Automations and integrations can manage tags, sections, and \
                                 tasks for you. Attach one to a tag you already keep, or detach \
                                 it to take that tag back.",
                    ))
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(status))
                    })
                    .children(
                        self.apps
                            .iter()
                            .map(|card| self.app_card(card, cx))
                            .collect::<Vec<_>>(),
                    ),
            )
    }
}

impl AppsPanel {
    /// One app: header, its bindings, and the attach / remove actions.
    fn app_card(&self, card: &AppCard, cx: &mut Context<Self>) -> gpui::AnyElement {
        let app_id = card.app.id;
        let label = card.app.label.clone();
        let can_manage = self.app_manages_tags(&card.app);
        let attaching = self.attaching == Some(app_id);

        let binding_rows: Vec<gpui::AnyElement> =
            card.bindings
                .iter()
                .map(|(binding, tag)| {
                    let tag_id = tag.id;
                    let capture = binding.capture_new_tasks;
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(0x1e1e1e))
                        .child(div().text_sm().child(format!("#{}", tag.label())))
                        .child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(
                            match binding.role {
                                BindingRole::FullTag => "Owns the whole tag",
                                BindingRole::Partial => "Manages part of the tag",
                            },
                        ))
                        .when(capture, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(SUCCESS))
                                    .child("New tasks are captured"),
                            )
                        })
                        .child(div().flex_1())
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
                        )
                        .into_any_element()
                })
                .collect();

        div()
            .rounded_lg()
            .border_1()
            .border_color(rgb(HAIRLINE))
            .bg(rgb(CARD_BG))
            .p_4()
            .v_flex()
            .gap_3()
            .child(
                div()
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
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().font_semibold().child(label.clone()))
                                    .child(kind_badge(&card.app.kind)),
                            )
                            .when_some(card.app.description.clone(), |this, description| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(TEXT_MUTED))
                                        .child(description),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(if card.app.enabled {
                                rgb(SUCCESS)
                            } else {
                                rgb(TEXT_FAINT)
                            })
                            .child(if card.app.enabled {
                                "Installed"
                            } else {
                                "Not installed"
                            }),
                    ),
            )
            .when(!card.bindings.is_empty(), |this| {
                this.child(div().v_flex().gap_1().children(binding_rows))
            })
            .when(card.bindings.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT_FAINT))
                        .child("Not managing any tags yet."),
                )
            })
            .when(can_manage, |this| {
                this.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new(format!("attach-{app_id}"))
                                .ghost()
                                .compact()
                                .with_size(Size::Small)
                                .label(if attaching {
                                    "Cancel"
                                } else {
                                    "Attach to a tag…"
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_attach(app_id, cx);
                                })),
                        )
                        .child(
                            Button::new(format!("remove-{app_id}"))
                                .ghost()
                                .compact()
                                .with_size(Size::Small)
                                .text_color(rgb(DANGER))
                                .label("Remove app")
                                .tooltip("Stop managing everything this app owns")
                                .on_click(cx.listener({
                                    let label = label.clone();
                                    move |this, _, window, cx| {
                                        this.confirm_remove(app_id, label.clone(), window, cx);
                                    }
                                })),
                        ),
                )
            })
            .when(attaching, |this| this.child(self.attach_picker(card, cx)))
            .into_any_element()
    }

    /// The "manage an existing tag" list: every ordinary tag, with the ones
    /// that cannot be taken (project folders, or tags another app owns
    /// outright) shown dimmed with the reason.
    fn attach_picker(&self, card: &AppCard, cx: &mut Context<Self>) -> gpui::AnyElement {
        let app_id = card.app.id;
        let mut tags: Vec<&Tag> = self.tags.iter().collect();
        tags.sort_by_key(|tag| tag.label().to_lowercase());

        let rows: Vec<gpui::AnyElement> = tags
            .into_iter()
            .map(|tag| {
                let tag_id = tag.id;
                let blocker = self.attach_blocker(app_id, tag_id);
                let eligible = blocker.is_none();
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .when(!eligible, |this| this.opacity(0.5))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("#{}", tag.label())),
                    )
                    .when_some(blocker, |this, reason| {
                        this.child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(reason))
                    })
                    .when(eligible, |this| {
                        this.child(
                            Button::new(format!("attach-to-{app_id}-{tag_id}"))
                                .compact()
                                .with_size(Size::Small)
                                .label("Attach")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.attach(app_id, tag_id, cx);
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let selected = self.attach_capture;
        let capture_label = format!("Also create new tasks here in {}", card.app.label);
        div()
            .v_flex()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(HAIRLINE))
            .bg(rgb(0x1e1e1e))
            .p_3()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(TEXT_MUTED))
                    .child("Manage an existing tag"),
            )
            .children(rows)
            .child(
                div().h_flex().items_center().gap_2().pt_1().child(
                    div()
                        .id("attach-capture")
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(if selected {
                            rgb(0x4a6fa5)
                        } else {
                            rgb(HAIRLINE)
                        })
                        .bg(if selected {
                            rgb(0x2f4057)
                        } else {
                            rgb(0x242424)
                        })
                        .text_sm()
                        .text_color(if selected {
                            rgb(0xdbe6f5)
                        } else {
                            rgb(TEXT_MUTED)
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.attach_capture = !this.attach_capture;
                            cx.notify();
                        }))
                        .child(capture_label),
                ),
            )
            .into_any_element()
    }
}

/// Small pill naming an app's kind.
fn kind_badge(kind: &str) -> gpui::Div {
    let label = match kind {
        "recipe" => "Automation",
        "integration" => "Integration",
        "builtin" => "Built-in",
        _ => "App",
    };
    div()
        .rounded_full()
        .bg(rgb(0x2a2a2a))
        .border_1()
        .border_color(rgb(0x3a3a3a))
        .px_2()
        .py_0p5()
        .text_xs()
        .text_color(rgb(TEXT_MUTED))
        .child(label)
}

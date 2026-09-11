use gpui::{
    App, AppContext, AsyncApp, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::WindowExt;
use gpui_component::StyledExt;
use gpui_component::input::*;
use gpui_component::{Disableable, Theme, ThemeMode, TitleBar};
use gpui_component::button::{Button, ButtonVariants};
use storage::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use projects::Project;
use store::Store;
use theme::APP_BG;
use ui_parts::navbar::{NavBar, NavBarEvent, NavPanel};
use ui_parts::settings::SettingsView;
use ui_parts::automations::{AutomationsEvent, AutomationsPanel};
use ui_parts::integrations::{IntegrationsEvent, IntegrationsView};
use ui_parts::project_picker::{ProjectPicker, ProjectPickerEvent};
use ui_parts::task_details::{TaskDetails, TaskDetailsEvent};
use ui_parts::task_list::{TaskListEvent, TaskListView};
use ui_parts::travel::{TravelPanel, TravelPanelEvent};
use ui_parts::workflows::{WorkflowPanel, WorkflowPanelEvent};

mod components;
mod projects;
mod store;
mod theme;
mod todoist_auth;
mod ui_parts {
    pub mod automations;
    pub mod integrations;
    pub mod navbar;
    pub mod settings;
    pub mod project_picker;
    pub mod repeat_picker;
    pub mod task_details;
    pub mod task_list;
    pub mod task_picker;
    pub mod task_row;
    pub mod travel;
    pub mod workflows;
}

/// The selected tag that is owned by an automation: the Layout swaps the
/// task list for the automation's special panel while it is selected.
#[derive(Clone)]
struct ManagedTag {
    tag_id: u64,
    recipe_id: u64,
    label: String,
}

struct Layout {
    pub task_list: Entity<TaskListView>,
    nav_bar: Entity<NavBar>,
    details: Entity<TaskDetails>,
    workflows: Entity<WorkflowPanel>,
    /// Special panel for the managed tag in `managed_tag`, if any.
    travel_panel: Entity<TravelPanel>,
    /// The managed tag currently selected (if the selected tag is owned
    /// by an automation).
    managed_tag: Option<ManagedTag>,
    integrations: Entity<IntegrationsView>,
    automations: Entity<AutomationsPanel>,
    settings: Entity<SettingsView>,
    /// Main panel shown next to the navbar (task list by default).
    panel: NavPanel,
    store: Store,
    /// Repos found by the home-directory scan, shown in the project picker
    /// modal opened by the + button.
    _projects: Vec<Project>,
    _project_subscription: Subscription,
    /// Subscription to the open project-picker modal, if one is open.
    _picker_subscription: Option<Subscription>,
    /// Window-wide Escape observer (focus-independent deselect).
    _escape_observer: Subscription,
}

impl Layout {
    fn new(
        input: Entity<InputState>,
        store: Store,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));
        // The managed-tag panel is created up front (it needs a window for
        // its input) and reconfigured when a managed tag is selected.
        let travel_panel = cx.new(|cx| {
            TravelPanel::new(store.clone(), 0, String::new(), window, cx)
        });
        // Captured by the nav subscription below: managed-tag detection is
        // async (a DB lookup), so it spawns on the app executor and
        // updates this entity when the lookup lands.
        let layout_weak = cx.weak_entity();

        // Kick off the home-directory repo scan in the background; the nav
        // repo list for the picker modal when it lands.
        let scan = projects::scan(cx);
        cx.spawn(async move |this, cx| {
            let projects = match scan.await {
                Ok(projects) => projects,
                Err(e) => {
                    tracing::error!("Failed to scan for git repos: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this._projects = projects;
                cx.notify();
            })
            .ok();
        })
        .detach();

        // The + button opens the project-picker modal; picking one creates
        // the folder's tag.
        let project_subscription = cx.subscribe_in(
            &nav_bar,
            window,
            move |this, _nav, event, window, cx| match event {
                NavBarEvent::TagSelected(path) => {
                    // Tag navigation is handled by the TaskListView's own
                    // subscription; make sure the task panel is visible.
                    this.show_panel(NavPanel::Tasks, cx);
                    this.check_managed_tag(path, &layout_weak, cx);
                }
                NavBarEvent::AllTasks => {
                    this.managed_tag = None;
                    this.show_panel(NavPanel::Tasks, cx);
                    this.task_list.update(cx, |list, cx| {
                        list.set_empty_action(None, cx);
                    });
                }
                NavBarEvent::OpenProjectPicker => {
                    let projects = this._projects.clone();
                    let store = this.store.clone();
                    let picker = cx.new(|cx| ProjectPicker::new(projects, store, window, cx));
                    // The input is only in the focus tree after the dialog
                    // renders, so defer the autofocus one frame.
                    let picker_for_focus = picker.clone();
                    window.on_next_frame(move |window, cx| {
                        picker_for_focus.update(cx, |picker, cx| {
                            picker.focus_filter(window, cx);
                        });
                    });
                    // Subscribe before opening the dialog so the very first
                    // selection is not missed.
                    this._picker_subscription = Some(cx.subscribe_in(
                        &picker,
                        window,
                        |this, _picker, event, window, cx| match event {
                            ProjectPickerEvent::Selected(project) => {
                                this.handle_pick_project(project.clone(), window, cx);
                            }
                            ProjectPickerEvent::TagName(name) => {
                                this.handle_create_tag(name.clone(), window, cx);
                            }
                            ProjectPickerEvent::TodoistProject { id, name } => {
                                this.handle_pick_todoist_project(
                                    id.clone(),
                                    name.clone(),
                                    window,
                                    cx,
                                );
                            }
                            ProjectPickerEvent::Dismissed => {
                                window.close_dialog(cx);
                            }
                        },
                    ));
                    let picker_for_dialog = picker.clone();
                    window.open_dialog(cx, move |dialog, _, _| {
                        let picker = picker_for_dialog.clone();
                        dialog
                            .title("Add a tag")
                            .content(move |content, _, _| content.child(picker.clone()))
                    });
                }
                NavBarEvent::OpenIntegrations => {
                    this.show_panel(NavPanel::Integrations, cx);
                }
                NavBarEvent::OpenAutomations => {
                    this.show_panel(NavPanel::Automations, cx);
                }
                NavBarEvent::OpenWorkflows => {
                    this.show_panel(NavPanel::Workflows, cx);
                }
                NavBarEvent::OpenSettings => {
                    this.show_panel(NavPanel::Settings, cx);
                }
            },
        );
        let task_list = cx.new(|cx| TaskListView::new(input, store.clone(), nav_bar.clone(), cx));
        let details = cx.new(|cx| TaskDetails::new(store.clone(), cx));
        let automations = cx.new(|cx| AutomationsPanel::new(store.clone(), cx));
        let workflows = cx.new(|cx| WorkflowPanel::new(store.clone(), cx));
        // Enabling/disabling an automation spawns or tombstones tasks:
        // reload the task list.
        let list_for_automations = task_list.clone();
        let nav_for_automations = nav_bar.clone();
        cx.subscribe(&automations, move |_this, _panel, event, cx| match event {
            AutomationsEvent::Changed => {
                list_for_automations.update(cx, |list, cx| list.refresh(cx));
                // Enabling creates the managed tag, disabling removes it:
                // the tag tree must reflect that.
                nav_for_automations.update(cx, |nav, cx| nav.refresh_tags(cx));
            }
        })
        .detach();
        // A workflow action (start run, approve/reject, fire event) can
        // spawn or complete tasks: reload the task list.
        let list_for_workflow = task_list.clone();
        cx.subscribe(&workflows, move |_this, _panel, event, cx| match event {
            WorkflowPanelEvent::Changed => {
                list_for_workflow.update(cx, |list, cx| list.refresh(cx));
            }
        })
        .detach();
        // A new trip spawns checklist items under the managed tag: reload
        // the task list so the Pack / Before leaving sections appear.
        let list_for_trip = task_list.clone();
        cx.subscribe(&travel_panel, move |_this, _panel, event, cx| match event {
            TravelPanelEvent::TripAdded => {
                list_for_trip.update(cx, |list, cx| list.refresh(cx));
            }
        })
        .detach();
        let integrations =
            cx.new(|cx| IntegrationsView::new(store.clone(), cx));
        let settings = cx.new(|cx| SettingsView::new(cx));
        // Syncs create tags (sections, labels): refresh the tag tree.
        cx.subscribe_in(
            &integrations,
            window,
            |this, _view, event, _window, cx| match event {
                IntegrationsEvent::Changed => {
                    this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
                }
            },
        )
        .detach();
        let details_for_list = details.clone();
        let list_for_deselect = task_list.clone();
        let travel_for_empty_action = travel_panel.clone();
        cx.subscribe(&task_list, move |this, _list, event, cx| match event {
            TaskListEvent::Selected(task) => {
                let deferred = details_for_list.update(cx, |details, cx| {
                    details.request_select(task.clone(), cx)
                });
                if !deferred {
                    cx.notify();
                }
            }
            TaskListEvent::Deselected => {
                let deferred =
                    details_for_list.update(cx, |details, cx| details.request_clear(cx));
                if !deferred {
                    list_for_deselect.update(cx, |list, cx| list.clear_selection(cx));
                    cx.notify();
                }
            }
            TaskListEvent::TitleCommitted { task_id, title } => {
                details_for_list.update(cx, |details, cx| {
                    details.update_title(*task_id, title.clone(), cx)
                });
            }
            // Empty managed tag: open the travel panel's add-trip popover.
            TaskListEvent::EmptyActionRequested => {
                travel_for_empty_action.update(cx, |panel, cx| {
                    panel.open_add();
                    cx.notify();
                });
                this.task_list.update(cx, |list, cx| list.refresh(cx));
            }
        })
        .detach();
        let list_for_toggle = task_list.clone();
        let list_for_pending = task_list.clone();
        let details_for_pending = details.clone();
        let workflows_for_toggle = workflows.clone();
        cx.subscribe_in(
            &details,
            window,
            move |_this, _details, event, _window, cx| match event {
                TaskDetailsEvent::Toggled { task_id, done } => {
                    list_for_toggle.update(cx, |list, cx| {
                        list.on_task_done_toggled(*task_id, *done, cx)
                    });
                    // Steps can be ticked from the task list too; keep the
                    // run cards in sync.
                    workflows_for_toggle.update(cx, |panel, cx| panel.refresh(cx));
                }
                TaskDetailsEvent::TitleCommitted { task_id, title } => {
                    list_for_toggle.update(cx, |list, cx| {
                        list.set_task_title(*task_id, title.clone(), cx)
                    });
                }
                TaskDetailsEvent::PendingConfirmed { selected } => {
                    if selected.is_none() {
                        list_for_pending.update(cx, |list, cx| list.clear_selection(cx));
                        cx.notify();
                    }
                }
                TaskDetailsEvent::PendingCancelled => {
                    let current = details_for_pending.read(cx).selected_task();
                    list_for_pending.update(cx, |list, cx| list.restore_selection(current, cx));
                }
                TaskDetailsEvent::SelectTask { task_id } => {
                    list_for_pending.update(cx, |list, cx| list.select_task_by_id(*task_id, cx));
                }
                TaskDetailsEvent::TaskRefreshed(task) => {
                    list_for_pending.update(cx, |list, cx| list.refresh_task_data(task, cx));
                }
                TaskDetailsEvent::SubtaskCreated | TaskDetailsEvent::FollowUpCreated => {
                    list_for_pending.update(cx, |list, cx| list.refresh(cx));
                }
            },
        )
        .detach();

        // Escape deselects wherever focus is: keystroke observers fire
        // window-wide, unlike `on_key_down` listeners which only run along
        // the focus path. Skipped while the project picker modal is open so
        // Esc there only dismisses the dialog.
        let escape_observer = cx.observe_keystrokes(
            move |layout: &mut Layout, event, _window, cx| {
                if event.keystroke.key == "escape" {
                    if layout._picker_subscription.is_some() {
                        return;
                    }
                    if layout.travel_panel.read(cx).is_adding() {
                        layout
                            .travel_panel
                            .update(cx, |panel, cx| panel.close_add(cx));
                        return;
                    }
                    if layout.panel != NavPanel::Tasks {
                        layout.show_panel(NavPanel::Tasks, cx);
                        return;
                    }
                    if layout.details.read(cx).adding_subtask() {
                        layout
                            .details
                            .update(cx, |details, cx| details.cancel_subtask(cx));
                        return;
                    }
                    if layout.details.read(cx).adding_follow_up() {
                        layout
                            .details
                            .update(cx, |details, cx| details.cancel_follow_up(cx));
                        return;
                    }
                    if layout.details.read(cx).blocker_picker_open() {
                        layout.details.update(cx, |details, cx| {
                            details.close_blocker_picker_and_notify(cx)
                        });
                        return;
                    }
                    if layout.details.read(cx).after_picker_open() {
                        layout.details.update(cx, |details, cx| {
                            details.close_after_picker_and_notify(cx)
                        });
                        return;
                    }
                    if layout.details.read(cx).until_panel_open() {
                        layout.details.update(cx, |details, cx| {
                            details.close_until_panel_and_notify(cx)
                        });
                        return;
                    }
                    if layout.details.read(cx).repeat_picker_open() {
                        layout.details.update(cx, |details, cx| {
                            details.close_repeat_picker_and_notify(cx)
                        });
                        return;
                    }
                    if layout.task_list.read(cx).is_editing() {
                        layout
                            .task_list
                            .update(cx, |list, cx| list.cancel_editing(cx));
                        return;
                    }
                    if layout.details.read(cx).is_editing() {
                        layout
                            .details
                            .update(cx, |details, cx| details.request_clear(cx));
                        return;
                    }
                    layout.details.update(cx, |details, cx| details.clear(cx));
                    layout
                        .task_list
                        .update(cx, |list, cx| list.clear_selection(cx));
                    cx.notify();
                }
            },
        );

        Self {
            task_list,
            nav_bar,
            details,
            workflows,
            travel_panel,
            managed_tag: None,
            integrations,
            automations,
            settings,
            panel: NavPanel::Tasks,
            store: store.clone(),
            _projects: Vec::new(),
            _project_subscription: project_subscription,
            _picker_subscription: None,
            _escape_observer: escape_observer,
        }
    }

    /// Look up the selected tag; if an automation owns it, show its
    /// special panel instead of the task list. Async so the navbar keeps
    /// working while the lookup runs.
    fn check_managed_tag(
        &self,
        path: &[String],
        layout_weak: &gpui::WeakEntity<Self>,
        cx: &mut App,
    ) {
        let Some(tag_name) = path.last().cloned() else {
            return;
        };
        let store = self.store.clone();
        let layout = layout_weak.clone();
        cx.spawn(async move |cx| {
            let managed = async {
                let Ok(Some(tag)) = store.get_tag_by_name(tag_name, cx).await else {
                    return None;
                };
                let recipe = store.managed_recipe_for_tag(tag.id, cx).await.ok().flatten();
                recipe.map(|recipe_id| ManagedTag {
                    tag_id: tag.id,
                    recipe_id,
                    label: tag.label(),
                })
            }
            .await;
            layout.update(cx, |this, cx| {
                let changed = this.managed_tag.as_ref().map(|m| m.tag_id)
                    != managed.as_ref().map(|m| m.tag_id);
                this.managed_tag = managed;
                if let Some(managed) = &this.managed_tag {
                    this.travel_panel.update(cx, |panel, cx| {
                        panel.set_tag(managed.recipe_id, managed.label.clone(), cx);
                    });
                    // Managed tag: the "+ New trip" button moves into the
                    // task list's title row, and the empty list offers it
                    // where the checklist rows would appear.
                    this.task_list.update(cx, |list, cx| {
                        list.set_travel_panel(Some(this.travel_panel.clone()), cx);
                        list.set_empty_action(Some("+ New trip".to_string()), cx);
                    });
                } else {
                    this.task_list.update(cx, |list, cx| {
                        list.set_travel_panel(None, cx);
                        list.set_empty_action(None, cx);
                    });
                }
                if changed {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Swap the main panel, keeping the navbar footer highlight in sync.
    fn show_panel(&mut self, panel: NavPanel, cx: &mut Context<Self>) {
        self.panel = panel;
        self.nav_bar.update(cx, |nav, cx| nav.set_panel(panel, cx));
        cx.notify();
    }

    fn handle_pick_project(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.close_dialog(cx);
        self._picker_subscription = None;
        let create = project.tag(&self.store, cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = create.await {
                tracing::error!("Failed to create project tag: {e}");
                return;
            }
            this.update(cx, |this, cx| {
                this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
            })
            .ok();
        })
        .detach();
    }

    fn handle_create_tag(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self._picker_subscription = None;
        let create = self.store.create_tag(name, cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = create.await {
                tracing::error!("Failed to create tag: {e}");
                return;
            }
            this.update(cx, |this, cx| {
                this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
            })
            .ok();
        })
        .detach();
    }

    /// Create the tag backing a Todoist project: the project name as-is,
    /// or a `todoist/<name>` namespaced copy on collision (spec §4.5),
    /// linked to the remote project so re-syncs reuse it.
    fn handle_pick_todoist_project(
        &mut self,
        id: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.close_dialog(cx);
        self._picker_subscription = None;
        let store = self.store.clone();
        // Link the tag, then pull the project's sections and tasks right
        // away so the new tag is populated without waiting for a manual
        // sync. One Tokio hop: token fetch and network must never run on
        // GPUI's executor.
        let flow = gpui_tokio::Tokio::spawn_result(cx, async move {
            let integration_id = {
                let mut backend = store.0.lock().await;
                let integration = backend
                    .list_integrations()
                    .await?
                    .into_iter()
                    .find(|i| i.provider == "todoist")
                    .ok_or_else(|| anyhow::anyhow!("Todoist is not connected"))?;
                let (tag, namespaced) = match backend.get_tag_by_name(&name).await? {
                    Some(_) => {
                        let scoped = format!("todoist/{name}");
                        let tag = match backend.get_tag_by_name(&scoped).await? {
                            Some(tag) => tag,
                            None => backend.create_tag(&scoped).await?,
                        };
                        (tag, true)
                    }
                    None => (backend.create_tag(&name).await?, false),
                };
                backend
                    .link_tag(integration.id, &id, tag.id, "project", namespaced)
                    .await?;
                integration.id
            };
            let token = crate::todoist_auth::access_token().await?;
            let summary = {
                let mut backend = store.0.lock().await;
                backend
                    .sync_todoist_integration(&token, integration_id)
                    .await?
            };
            Ok::<_, anyhow::Error>(summary)
        });
        cx.spawn(async move |this, cx| {
            match flow.await {
                Ok(summary) => tracing::info!(
                    "Todoist project synced: {} task(s), {} section(s)",
                    summary.tasks_upserted,
                    summary.sections,
                ),
                Err(e) => tracing::error!("Todoist sync after project pick failed: {e}"),
            }
            this.update(cx, |this, cx| {
                this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
            })
            .ok();
        })
        .detach();
    }
}

impl Render for Layout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The gpui-component Root only paints its main view; overlays like
        // dialogs must be layered on top by the app (same composition as
        // gpui-component's story app).
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);

        div()
            .relative()
            .size_full()
            .v_flex()
            .child(
                TitleBar::new().bg(rgb(APP_BG)).child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("history-back")
                                .ghost()
                                .compact()
                                .label("<")
                                .disabled(!self.task_list.read(cx).can_go_back())
                                .tooltip("Previous task")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.task_list.update(cx, |list, cx| list.go_back(cx));
                                })),
                        )
                        .child(
                            Button::new("history-forward")
                                .ghost()
                                .compact()
                                .label(">")
                                .disabled(!self.task_list.read(cx).can_go_forward())
                                .tooltip("Next task")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.task_list.update(cx, |list, cx| list.go_forward(cx));
                                })),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    // A flex item refuses to shrink below its content
                    // height, so the row would grow past the window and
                    // push the navbar footer out of view; min_h_0 lets it
                    // clamp to the remaining window height instead.
                    .min_h_0()
                    .child(div().w(px(256.)).flex_none().child(self.nav_bar.clone()))
                    .child(match self.panel {
                        NavPanel::Tasks if self.managed_tag.is_some() => div()
                            .id("managed-panel")
                            .relative()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .min_h_0()
                            // The checklist items use the normal task list
                            // with its sections; its title row carries the
                            // "+ New trip" button at its end. The popover
                            // is the LAST child so GPUI paints it above the
                            // task list (paint order follows tree order).
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_col()
                                    .child(self.task_list.clone()),
                            )
                            .child(
                                self.travel_panel
                                    .update(cx, |panel, cx| panel.popover(window, cx)),
                            )
                            .into_any_element(),
                        NavPanel::Tasks => div()
                            .id("right-column")
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .on_click(cx.listener(|this, _, _, cx| {
                                let deferred = this
                                    .details
                                    .update(cx, |details, cx| details.request_clear(cx));
                                if !deferred {
                                    this.task_list
                                        .update(cx, |list, cx| list.clear_selection(cx));
                                    cx.notify();
                                }
                            }))
                            .child(div().flex_1().min_h_0().flex().flex_col().child(self.task_list.clone()))
                            .when(self.details.read(cx).has_selection(), |this| {
                                this.child(div().flex_1().min_h_0().child(self.details.clone()))
                            })
                            .into_any_element(),
                        NavPanel::Integrations => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.integrations.clone()))
                            .into_any_element(),
                        NavPanel::Automations => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.automations.clone()))
                            .into_any_element(),
                        NavPanel::Workflows => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.workflows.clone()))
                            .into_any_element(),
                        NavPanel::Settings => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.settings.clone()))
                            .into_any_element(),
                    }),
            )
            .children(dialog_layer)
    }
}

fn main() {
    init_logging();

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        gpui_tokio::init(cx);
        gpui_component::init(cx);
        ui_parts::project_picker::init(cx);
        ui_parts::task_picker::init(cx);

        let init_store = gpui_tokio::Tokio::spawn_result(cx, async move {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = TodoStore::new(&config).await?;
            store.seed().await?;
            // The database is in-memory, so re-register the Todoist
            // connection row when tokens survived in ~/.config/my-todo.
            if todoist_auth::has_stored_credentials()
                && !store
                    .list_integrations()
                    .await?
                    .into_iter()
                    .any(|i| i.provider == "todoist")
            {
                store.create_integration("todoist", None).await?;
            }
            let tasks = store.list_tasks_by_priority().await.unwrap_or_default();
            // Occurrences startable or due in the next 2 days exist from
            // here on; the daily timer keeps them coming.
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Err(e) = store.materialize_daily_occurrences(now_secs).await {
                tracing::error!("Failed to materialize repeat occurrences: {e}");
            }
            Ok::<_, anyhow::Error>((Store::new(store), tasks))
        });

        cx.spawn(|cx: &mut AsyncApp| {
            let cx = cx.clone();
            async move {
                match init_store.await {
                    Ok((store, tasks)) => {
                        cx.open_window(TitleBar::window_options(), |window, cx| {
                            Theme::change(ThemeMode::Dark, Some(window), cx);

                            let input = cx.new(|cx| {
                                let mut input_state = InputState::new(window, cx);
                                input_state.set_placeholder("New task...", window, cx);
                                input_state
                            });

                            let mini = cx.new(|cx| Layout::new(input, store, window, cx));

                            let entity = mini.clone();
                            cx.spawn(move |cx: &mut AsyncApp| {
                                let mut cx = cx.clone();
                                let entity = entity.clone();
                                async move {
                                    entity.update(&mut cx, |mini, cx| {
                                        mini.task_list
                                            .update(cx, |list, cx| list.set_tasks(tasks, cx));
                                    });
                                }
                            })
                            .detach();

                            cx.new(|cx| {
                                gpui_component::Root::new(mini, window, cx).bg(rgb(APP_BG))
                            })
                        })
                        .expect("Failed to open window");
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize store: {e}");
                    }
                }
            }
        })
        .detach();
    });
}

fn init_logging() {
    let debug = std::env::args().any(|arg| arg == "--debug" || arg == "-d");
    let filter = if debug {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .with(filter)
        .init();
}

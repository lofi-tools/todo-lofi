use gpui::{
    AppContext, AsyncApp, Context, Entity, InteractiveElement, IntoElement, ParentElement,
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
use ui_parts::integrations::IntegrationsView;
use ui_parts::project_picker::{ProjectPicker, ProjectPickerEvent};
use ui_parts::task_details::{TaskDetails, TaskDetailsEvent};
use ui_parts::task_list::{TaskListEvent, TaskListView};

mod components;
mod projects;
mod store;
mod theme;
mod todoist_auth;
mod ui_parts {
    pub mod integrations;
    pub mod navbar;
    pub mod settings;
    pub mod project_picker;
    pub mod repeat_picker;
    pub mod task_details;
    pub mod task_list;
    pub mod task_picker;
    pub mod task_row;
}

struct Layout {
    pub task_list: Entity<TaskListView>,
    nav_bar: Entity<NavBar>,
    details: Entity<TaskDetails>,
    integrations: Entity<IntegrationsView>,
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
            |this, _nav, event, window, cx| match event {
                NavBarEvent::TagSelected(_) | NavBarEvent::AllTasks => {
                    // Tag navigation is handled by the TaskListView's own
                    // subscription; make sure the task panel is visible.
                    this.show_panel(NavPanel::Tasks, cx);
                }
                NavBarEvent::OpenProjectPicker => {
                    let projects = this._projects.clone();
                    let picker = cx.new(|cx| ProjectPicker::new(projects, window, cx));
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
                            ProjectPickerEvent::Dismissed => {
                                window.close_dialog(cx);
                            }
                        },
                    ));
                    let picker_for_dialog = picker.clone();
                    window.open_dialog(cx, move |dialog, _, _| {
                        let picker = picker_for_dialog.clone();
                        dialog
                            .title("Add a project")
                            .content(move |content, _, _| content.child(picker.clone()))
                    });
                }
                NavBarEvent::OpenIntegrations => {
                    this.show_panel(NavPanel::Integrations, cx);
                }
                NavBarEvent::OpenSettings => {
                    this.show_panel(NavPanel::Settings, cx);
                }
            },
        );
        let task_list = cx.new(|cx| TaskListView::new(input, store.clone(), nav_bar.clone(), cx));
        let details = cx.new(|cx| TaskDetails::new(store.clone(), cx));
        let integrations =
            cx.new(|cx| IntegrationsView::new(store.clone(), cx));
        let settings = cx.new(|cx| SettingsView::new(cx));
        let details_for_list = details.clone();
        let list_for_deselect = task_list.clone();
        cx.subscribe(&task_list, move |_this, _list, event, cx| match event {
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
        })
        .detach();
        let list_for_toggle = task_list.clone();
        let list_for_pending = task_list.clone();
        let details_for_pending = details.clone();
        cx.subscribe_in(
            &details,
            window,
            move |_this, _details, event, _window, cx| match event {
                TaskDetailsEvent::Toggled { task_id, done } => {
                    list_for_toggle.update(cx, |list, cx| {
                        list.on_task_done_toggled(*task_id, *done, cx)
                    });
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
            integrations,
            settings,
            panel: NavPanel::Tasks,
            store: store.clone(),
            _projects: Vec::new(),
            _project_subscription: project_subscription,
            _picker_subscription: None,
            _escape_observer: escape_observer,
        }
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
                    .child(div().w(px(256.)).flex_none().child(self.nav_bar.clone()))
                    .child(match self.panel {
                        NavPanel::Tasks => div()
                            .id("right-column")
                            .flex_1()
                            .flex()
                            .flex_row()
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
                            .child(div().flex_1().child(self.task_list.clone()))
                            .when(self.details.read(cx).has_selection(), |this| {
                                this.child(div().flex_1().child(self.details.clone()))
                            })
                            .into_any_element(),
                        NavPanel::Integrations => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .child(div().flex_1().child(self.integrations.clone()))
                            .into_any_element(),
                        NavPanel::Settings => div()
                            .flex_1()
                            .flex()
                            .flex_row()
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
            let tasks = store.list_tasks_by_priority().await.unwrap_or_default();
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

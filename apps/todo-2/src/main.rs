use gpui::{
    Anchor, AnyElement, App, AppContext, AsyncApp, Context, ElementId, Entity, InteractiveElement,
    IntoElement, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Render,
    StatefulInteractiveElement, Styled, Subscription, Svg, Transformation, Window, div,
    prelude::FluentBuilder, px, radians, rgb, svg,
};
use gpui_component::WindowExt;
use gpui_component::StyledExt;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_component::input::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextView;
use gpui_component::{
    Disableable, IconName, ResizableState, Selectable, Sizable, Size, Theme, ThemeMode, TitleBar,
    h_resizable, resizable_panel,
};
use storage::prelude::*;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use acp_client::SessionStore;
use projects::Project;
use store::Store;
use theme::APP_BG;
use ui_parts::agent_pane::{AgentPane, AgentPaneEvent, AgentProject, build_task_context};
use ui_parts::navbar::{NavBar, NavBarEvent, NavDestination};
use ui_parts::settings::SettingsView;
use ui_parts::automations::{AutomationsEvent, AutomationsPanel};
use ui_parts::apps::{AppSettings, AppSettingsEvent};
use ui_parts::integrations::{IntegrationsEvent, IntegrationsView, since_label};
use ui_parts::notifications::{
    Notice, NoticeFeed, NoticeFilter, NoticeLayer, NoticeLevel, NoticeLog, NoticeSink, error_toast,
};
use ui_parts::project_picker::{ProjectPicker, ProjectPickerEvent};
use ui_parts::task_details::{TaskDetails, TaskDetailsEvent};
use ui_parts::task_list::{TaskListEvent, TaskListView};
use ui_parts::tag_settings::{TagSettingsEvent, TagSettingsPanel};
use ui_parts::travel::{TravelPanel, TravelPanelEvent};

mod coding_git;
mod coding_mcp;
mod components;
mod github_auth;
mod projects;
mod store;
mod theme;
mod todoist_auth;
mod ui_parts {
    pub mod agent_pane;
    pub mod apps;
    pub mod automations;
    pub mod integrations;
    pub mod navbar;
    pub mod notifications;
    pub mod settings;
    pub mod project_picker;
    pub mod repeat_picker;
    pub mod task_details;
    pub mod task_list;
    pub mod task_picker;
    pub mod tag_settings;
    pub mod task_row;
    pub mod todoist_sync;
    pub mod travel;
}

/// Which pane fills the right-hand column of the Tasks panel.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum RightPane {
    #[default]
    Details,
    Agent,
}

/// Details pane geometry: 1.2x the original 560px default, at most 60% of
/// the app width. The agent split keeps its own fixed maximum.
const DETAILS_PANE_WIDTH: f32 = 672.;
const DETAILS_PANE_MIN_WIDTH: f32 = 320.;
const DETAILS_PANE_MAX_FRACTION: f32 = 0.60;
const SPLIT_RIGHT_PANE_MAX_WIDTH: f32 = 1500.;

/// Height of the window-wide footer strip. The notifications pane anchors
/// its own top edge to the footer's, so both share the number.
const FOOTER_HEIGHT: f32 = 28.;

/// Fraction of the space above the footer used by the notifications pane.
/// The pane renders a fixed pixel height calculated from the live viewport,
/// so opening it shows a tall, scrollable history without moving the layout.
const NOTIFICATION_PANE_HEIGHT_FRACTION: f32 = 0.80;

/// Width cap for a notification card. Narrow enough that a card reads as a
/// floating note rather than a band across the window.
const NOTIFICATION_WIDTH: f32 = 360.;

/// Gap between a notification card and the window edges.
const NOTIFICATION_MARGIN: f32 = 12.;

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
    /// The agent pane for the selected directory-backed project.
    agent_pane: Entity<AgentPane>,
    /// Which right-hand pane is showing (Details or Agent).
    right_pane: RightPane,
    /// Whether the selected tag is a directory-backed project, i.e. whether
    /// the Agent switcher is enabled.
    agent_available: bool,
    /// Split geometry for the task list / right pane row. Keyed state would
    /// also work; holding it makes the type nameable and keeps the width for
    /// the app run without writing anything to disk.
    split_state: Entity<ResizableState>,
    /// Current details overlay width and an in-progress left-edge drag, if any.
    right_pane_width: Pixels,
    details_resize_grab: Option<(Pixels, Pixels)>,
    _agent_events: Subscription,
    /// Special panel for the managed tag in `managed_tag`, if any.
    travel_panel: Entity<TravelPanel>,
    /// The tag settings popover: placements, directories, sections, and app
    /// bindings for the selected tag. Created up front (its section input
    /// needs a window) and retargeted when it is opened for another tag.
    tag_settings: Entity<TagSettingsPanel>,
    /// The managed tag currently selected (if the selected tag is owned
    /// by an automation).
    managed_tag: Option<ManagedTag>,
    integrations: Entity<IntegrationsView>,
    automations: Entity<AutomationsPanel>,
    /// Ownership settings for every app, embedded by the Automations and
    /// Integrations panels. Held so the panels share one instance.
    _apps: Entity<AppSettings>,
    settings: Entity<SettingsView>,
    /// Everything the app reported: the footer's indicator and the pane.
    notices: NoticeLog,
    /// Whether the notifications pane is expanded above the footer.
    notices_open: bool,
    /// Which severities the pane lists.
    notice_filter: NoticeFilter,
    store: Store,
    /// Repos found by the home-directory scan, shown in the project picker
    /// modal opened by the + button.
    _projects: Vec<Project>,
    _project_subscription: Subscription,
    /// Subscription to the open project-picker modal, if one is open.
    _picker_subscription: Option<Subscription>,
    /// Re-render when toasts come and go, so the notification layer below
    /// is only mounted while one is actually showing.
    _toast_layer_refresh: Option<Subscription>,
    /// Window-wide Escape observer (focus-independent deselect).
    _escape_observer: Subscription,
    /// The loopback MCP endpoint the coding agent attaches to, held for the
    /// app run (the listener thread lives until the process exits).
    _coding_mcp: Option<coding_mcp::CodingMcpServer>,
}

impl Layout {
    fn new(
        input: Entity<InputState>,
        store: Store,
        notices: UnboundedReceiver<Notice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The notification feed reaches here from `main`: every recorded
        // notification is listed in the pane, and an error or warning also
        // pops up as a card. The window handle lets a failure logged on a
        // background thread raise its card too.
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let mut notices = notices;
            while let Some(notice) = notices.recv().await {
                if this
                    .update(cx, |this, cx| {
                        this.log_notice(notice.level, notice.message.clone(), cx)
                    })
                    .is_err()
                {
                    // The layout is gone; nothing is left to notify.
                    return;
                }
                // Errors and warnings pop up; informational messages stay in
                // the pane and the footer's indicator. A repeat refreshes the
                // card it already raised, because a card is keyed by its message.
                if let Some(toast) = notice.toast() {
                    // A closed window has nowhere to show the card; the entry
                    // stays in the pane either way.
                    if let Err(error) = window_handle
                        .update(cx, |_, window, cx| window.push_notification(toast, cx))
                    {
                        tracing::debug!("toast dropped: {error}");
                    }
                }
            }
        })
        .detach();
        // The agent's tool surface: a loopback MCP endpoint over the same
        // store the UI uses. Every mutating tool call signals this channel so
        // the panels reload when the agent attaches a spec or spawns a
        // sub-task.
        let (coding_notify, mut coding_changes) = tokio::sync::mpsc::unbounded_channel::<()>();
        let coding_endpoint = match coding_mcp::start(
            store.clone(),
            gpui_tokio::Tokio::handle(cx),
            Some(coding_notify),
        ) {
            Ok(server) => {
                tracing::info!(
                    url = server.url(),
                    token = server.token(),
                    "coding MCP endpoint ready"
                );
                Some(server)
            }
            Err(error) => {
                tracing::error!("Failed to start the coding MCP endpoint: {error}");
                None
            }
        };
        cx.spawn(async move |this, cx| {
            while coding_changes.recv().await.is_some() {
                this.update(cx, |this, cx| {
                    this.details
                        .update(cx, |details, cx| details.refresh_coding(cx));
                    this.task_list.update(cx, |list, cx| list.refresh(cx));
                    this.automations.update(cx, |panel, cx| panel.refresh(cx));
                })
                .ok();
            }
        })
        .detach();
        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));
        // The agent pane keeps one session per project, so it is created once
        // with the layout and merely stops being rendered when the details
        // pane is showing.
        let agent_pane = cx.new(|cx| AgentPane::new(SessionStore::default(), window, cx));
        let split_state = cx.new(|_| ResizableState::default());
        // A turn starting or ending flips the project's busy dot in the
        // navbar; the pane does not know about the navbar itself.
        let nav_for_agent = nav_bar.clone();
        let _agent_events = cx.subscribe_in(
            &agent_pane,
            window,
            move |this, _pane, event, window, cx| match event {
                AgentPaneEvent::BusyChanged { tag_name, busy } => {
                    let tag_name = tag_name.clone();
                    let busy = *busy;
                    nav_for_agent.update(cx, |nav, cx| {
                        nav.set_agent_busy(&tag_name, busy, cx)
                    });
                }
                // "Attach task" fills the prompt box; sending stays manual so
                // the message can be edited first.
                AgentPaneEvent::AttachTaskRequested => {
                    if let Some(context) = this.task_context(cx) {
                        this.agent_pane.update(cx, |pane, cx| {
                            pane.insert_prompt_text(context, window, cx)
                        });
                    }
                }
            },
        );
        // The managed-tag panel is created up front (it needs a window for
        // its input) and reconfigured when a managed tag is selected.
        let travel_panel = cx.new(|cx| {
            TravelPanel::new(store.clone(), 0, 0, String::new(), window, cx)
        });
        let tag_settings = cx.new(|cx| TagSettingsPanel::new(store.clone(), window, cx));
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
                // The navbar owns the navigation stack, so the panel follows
                // the destination it reports: no panel state to keep in
                // sync here, and a menu panel is left the same way a tag is
                // entered (and the same way back).
                NavBarEvent::Navigated(destination) => match destination {
                    NavDestination::Tag(path) => {
                        this.check_managed_tag(path, &layout_weak, cx);
                        this.sync_agent_project(path, window, cx);
                    }
                    NavDestination::AllTasks => {
                        this.managed_tag = None;
                        this.agent_available = false;
                        let agent_pane = this.agent_pane.clone();
                        agent_pane.update(cx, |pane, cx| pane.set_project(None, cx));
                        this.task_list.update(cx, |list, cx| {
                            list.set_empty_action(None, cx);
                        });
                    }
                    // A menu panel is the whole destination: the task list
                    // is not rendered and keeps the tag it was pointed at,
                    // so navigating back restores the view that was on
                    // screen without a reload.
                    NavDestination::Integrations
                    | NavDestination::Automations
                    | NavDestination::Settings => {}
                },
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
                // A tag row's context menu: show the tag settings popover for
                // that tag, wherever the user is.
                NavBarEvent::OpenTagSettings(name) => {
                    let candidates = this
                        ._projects
                        .iter()
                        .map(|project| project.path.clone())
                        .collect();
                    this.tag_settings
                        .update(cx, |panel, cx| panel.open(name.clone(), candidates, cx));
                    cx.notify();
                }
            },
        );
        let task_list = cx.new(|cx| TaskListView::new(input, store.clone(), nav_bar.clone(), cx));
        let details = cx.new(|cx| TaskDetails::new(store.clone(), cx));
        // Whether the phase's agent turn is running is a pane fact (decisions
        // #27/#28): the details pane reads it to disable the rewind and to put
        // Stop on the active step row.
        let details_for_busy = details.clone();
        let _busy_subscription = cx.subscribe(&agent_pane, move |_this, pane, event, cx| {
            if matches!(event, AgentPaneEvent::BusyChanged { .. }) {
                let busy = pane.read(cx).is_busy();
                details_for_busy.update(cx, |details, cx| details.set_coding_phase_running(busy, cx));
            }
        });
        // One ownership-settings entity, embedded by the panels below.
        let apps = cx.new(|cx| AppSettings::new(store.clone(), cx));
        let automations = cx.new(|cx| AutomationsPanel::new(store.clone(), apps.clone(), cx));
        // Enabling/disabling an automation spawns or tombstones tasks, and a
        // run action can spawn or complete them: reload the task list and
        // pick the change up in the details stepper.
        let list_for_automations = task_list.clone();
        let nav_for_automations = nav_bar.clone();
        let details_for_automations = details.clone();
        cx.subscribe(&automations, move |_this, _panel, event, cx| match event {
            AutomationsEvent::Changed => {
                list_for_automations.update(cx, |list, cx| list.refresh(cx));
                // Enabling creates the managed tag, disabling removes it:
                // the tag tree must reflect that.
                nav_for_automations.update(cx, |nav, cx| nav.refresh_tags(cx));
                details_for_automations.update(cx, |details, cx| details.refresh_coding(cx));
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
        // Tag settings writes move the tag tree (placements), change which
        // tags are projects (directories), and add sections (child tags), so
        // every write reloads the nav and the list.
        let list_for_tag_settings = task_list.clone();
        cx.subscribe(&tag_settings, move |this, _panel, event, cx| match event {
            TagSettingsEvent::Changed => {
                this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
                list_for_tag_settings.update(cx, |list, cx| list.refresh(cx));
            }
        })
        .detach();
        let integrations =
            cx.new(|cx| IntegrationsView::new(store.clone(), apps.clone(), cx));
        // Attaching, detaching, or removing an app changes tag ownership
        // (and can delete tags outright): refresh the tree, the lists, and
        // the panels that show ownership.
        let list_for_apps = task_list.clone();
        let nav_for_apps = nav_bar.clone();
        let automations_for_apps = automations.clone();
        let integrations_for_apps = integrations.clone();
        cx.subscribe(&apps, move |_this, _settings, event, cx| match event {
            AppSettingsEvent::Changed => {
                list_for_apps.update(cx, |list, cx| list.refresh(cx));
                nav_for_apps.update(cx, |nav, cx| nav.refresh_tags(cx));
                automations_for_apps.update(cx, |panel, cx| panel.refresh(cx));
                integrations_for_apps.update(cx, |view, cx| view.refresh(cx));
            }
        })
        .detach();
        let settings = cx.new(|cx| SettingsView::new(store.clone(), apps.clone(), cx));
        let settings_for_apps = settings.clone();
        cx.subscribe(&apps, move |_this, _settings, event, cx| match event {
            AppSettingsEvent::Changed => {
                settings_for_apps.update(cx, |view, cx| view.refresh(cx));
            }
        })
        .detach();
        // Syncs create tags (sections, labels): refresh the tag tree.
        cx.subscribe_in(
            &integrations,
            window,
            |this, _view, event, window, cx| match event {
                IntegrationsEvent::Changed => {
                    this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
                    // A sync or a merged pull request changes both the task list
                    // and the selected run: a merge completes the run and
                    // removes its worktrees, which moves the agent pane too.
                    this.task_list.update(cx, |list, cx| list.refresh(cx));
                    this.details.update(cx, |details, cx| details.refresh_coding(cx));
                    this.sync_agent_checkout_for_selection(cx);
                }
                IntegrationsEvent::Notice(message) => {
                    // The integrations view has already logged this and set its
                    // own status line, which is what the pane lists. Raising
                    // the toast here, with the window in hand, puts the message
                    // on screen the moment the failure is reported instead of
                    // when its log record has been round tripped through the
                    // feed. Both pushes carry the same text, and a card is
                    // keyed by its message, so one failure is one card.
                    window.push_notification(error_toast(message.clone()), cx);
                }
            },
        )
        .detach();
        let details_for_list = details.clone();
        let list_for_deselect = task_list.clone();
        let travel_for_empty_action = travel_panel.clone();
        cx.subscribe(&task_list, move |this, _list, event, cx| match event {
            TaskListEvent::Selected(task) => {
                let task_id = task.id;
                let deferred = details_for_list
                    .update(cx, |details, cx| details.request_select(task.clone(), cx));
                // The pane follows the selected task's run checkout (§6.3).
                this.sync_agent_checkout(task_id, cx);
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
            // The gear beside the tag title: show the tag settings popover.
            TaskListEvent::OpenTagSettings(name) => {
                let candidates = this
                    ._projects
                    .iter()
                    .map(|project| project.path.clone())
                    .collect();
                this.tag_settings
                    .update(cx, |panel, cx| panel.open(name.clone(), candidates, cx));
                cx.notify();
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
        let automations_for_toggle = automations.clone();
        cx.subscribe_in(
            &details,
            window,
            move |this, _details, event, window, cx| match event {
                TaskDetailsEvent::Toggled { task_id, done } => {
                    list_for_toggle.update(cx, |list, cx| {
                        list.on_task_done_toggled(*task_id, *done, cx)
                    });
                    // Steps can be ticked from the task list too; keep the
                    // run cards in sync.
                    automations_for_toggle.update(cx, |panel, cx| panel.refresh(cx));
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
                // The pane's close button: deselect the current task, which
                // closes the pane. Defers while edits would be lost.
                TaskDetailsEvent::Deselected => {
                    let deferred =
                        details_for_pending.update(cx, |details, cx| details.request_clear(cx));
                    if !deferred {
                        list_for_pending.update(cx, |list, cx| list.clear_selection(cx));
                        cx.notify();
                    }
                }
                TaskDetailsEvent::SelectTask { task_id } => {
                    list_for_pending.update(cx, |list, cx| list.select_task_by_id(*task_id, cx));
                }
                TaskDetailsEvent::TaskRefreshed(task) => {
                    list_for_pending.update(cx, |list, cx| list.refresh_task_data(task, cx));
                    this.sync_agent_checkout(task.id, cx);
                }
                TaskDetailsEvent::SubtaskCreated | TaskDetailsEvent::FollowUpCreated => {
                    list_for_pending.update(cx, |list, cx| list.refresh(cx));
                }
                // A phase action spawned or completed steps: reload the task
                // list and the run cards.
                TaskDetailsEvent::CodingChanged => {
                    list_for_pending.update(cx, |list, cx| list.refresh(cx));
                    automations_for_toggle.update(cx, |panel, cx| panel.refresh(cx));
                    // Spec approval creates the worktrees and completion removes
                    // them, so the pane's checkout follows the run's state.
                    this.sync_agent_checkout_for_selection(cx);
                }
                // A phase prompt is ready: drop it into the agent pane and
                // show the pane. Sending stays manual so it can be edited.
                TaskDetailsEvent::CodingLaunch { phase, prompt } => {
                    tracing::info!(phase, "coding phase prompt ready in the agent pane");
                    let prompt = prompt.clone();
                    this.agent_pane
                        .update(cx, |pane, cx| pane.insert_prompt_text(prompt, window, cx));
                    this.show_right_pane(RightPane::Agent, window, cx);
                }
                // Stop on a run step delegates to the agent pane's stop-turn:
                // the step stays open and the branch keeps what the turn wrote
                // (decision #27).
                TaskDetailsEvent::CodingStopPhase => {
                    this.agent_pane.update(cx, |pane, cx| pane.stop_turn(cx));
                    let busy = this.agent_pane.read(cx).is_busy();
                    this.details
                        .update(cx, |details, cx| details.set_coding_phase_running(busy, cx));
                }
            },
        )
        .detach();

        // Escape deselects wherever focus is: keystroke observers fire
        // window-wide, unlike `on_key_down` listeners which only run along
        // the focus path. Skipped while the project picker modal is open so
        // Esc there only dismisses the dialog.
        let escape_observer = cx.observe_keystrokes(
            move |layout: &mut Layout, event, window, cx| {
                if event.keystroke.key == "escape" {
                    if layout._picker_subscription.is_some() {
                        return;
                    }
                    // An open dropdown in the agent pane swallows the first
                    // Escape; the next one deselects as usual.
                    if layout.right_pane == RightPane::Agent
                        && layout
                            .agent_pane
                            .update(cx, |pane, cx| pane.dismiss_overlay(cx))
                    {
                        return;
                    }
                    if layout.tag_settings.read(cx).is_open() {
                        layout
                            .tag_settings
                            .update(cx, |panel, cx| panel.close(cx));
                        return;
                    }
                    if layout.travel_panel.read(cx).is_adding() {
                        layout
                            .travel_panel
                            .update(cx, |panel, cx| panel.close_add(cx));
                        return;
                    }
                    // An expanded integration's settings collapse first; the
                    // next Escape leaves the panel as usual.
                    if layout.nav_bar.read(cx).destination() == &NavDestination::Integrations
                        && layout
                            .integrations
                            .update(cx, |view, cx| view.collapse_settings(cx))
                    {
                        return;
                    }
                    // A menu panel is one navigation step; Escape takes it
                    // back to the destination it was opened from (the tag
                    // pane, normally) rather than to a fixed panel.
                    if !layout.nav_bar.read(cx).destination().is_tasks() {
                        let went_back = layout
                            .nav_bar
                            .update(cx, |nav, cx| nav.navigate_back(cx));
                        if !went_back {
                            layout.nav_bar.update(cx, |nav, cx| {
                                nav.navigate_to(NavDestination::AllTasks, cx)
                            });
                        }
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
                    if layout.details.read(cx).is_editing() || layout.details.read(cx).confirming()
                    {
                        let consumed = layout
                            .details
                            .update(cx, |details, cx| details.escape_details(window, cx));
                        if consumed {
                            return;
                        }
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
            agent_pane,
            right_pane: RightPane::Details,
            agent_available: false,
            split_state,
            right_pane_width: px(DETAILS_PANE_WIDTH),
            details_resize_grab: None,
            _agent_events,
            travel_panel,
            tag_settings,
            managed_tag: None,
            integrations,
            automations,
            _apps: apps,
            settings,
            notices: NoticeLog::default(),
            notices_open: false,
            notice_filter: NoticeFilter::default(),
            store: store.clone(),
            _projects: Vec::new(),
            _project_subscription: project_subscription,
            _picker_subscription: None,
            _toast_layer_refresh: None,
            _escape_observer: escape_observer,
            _coding_mcp: coding_endpoint,
        }
    }

/// Resolve the selected tag to its managing automation, if any.
async fn lookup_managed_tag(
    store: &Store,
    tag_name: &str,
    cx: &mut AsyncApp,
) -> Option<ManagedTag> {
    let Ok(Some(tag)) = store.get_tag_by_name(tag_name.to_string(), cx).await else {
        return None;
    };
    let recipe = store
        .managed_recipe_for_tag(tag.id, cx)
        .await
        .ok()
        .flatten()?;
    Some(ManagedTag {
        tag_id: tag.id,
        recipe_id: recipe,
        label: tag.label(),
    })
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
        let path = path.to_vec();
        let store = self.store.clone();
        let layout = layout_weak.clone();
        cx.spawn(async move |cx| {
            let mut managed = Self::lookup_managed_tag(&store, &tag_name, cx).await;
            let mut retried = false;
            if managed.is_none() {
                // The tag may be mid-creation: enabling an automation then
                // immediately opening its tag can lose the spawn-order race
                // to the enable transaction. Resolve once more before
                // settling on the plain task list.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(350))
                    .await;
                managed = Self::lookup_managed_tag(&store, &tag_name, cx).await;
                retried = true;
            }
            layout
                .update(cx, |this, cx| {
                    // Drop a retried result if the user navigated elsewhere
                    // while waiting.
                    if retried
                        && this.nav_bar.read(cx).destination()
                            != &NavDestination::Tag(path.clone())
                    {
                        return;
                    }
                    let changed = this.managed_tag.as_ref().map(|m| m.tag_id)
                        != managed.as_ref().map(|m| m.tag_id);
                    this.managed_tag = managed;
                if let Some(managed) = &this.managed_tag {
                    this.travel_panel.update(cx, |panel, cx| {
                        panel.set_tag(
                            managed.recipe_id,
                            managed.tag_id,
                            managed.label.clone(),
                            cx,
                        );
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

    /// Details pane maximum width, tracking the live app width.
    fn details_max_width(window: &Window) -> Pixels {
        (window.viewport_size().width * DETAILS_PANE_MAX_FRACTION).max(px(DETAILS_PANE_MIN_WIDTH))
    }

    /// Notifications pane height, tracking the live app height.
    fn notifications_pane_height(window: &Window) -> Pixels {
        let available_height = window.viewport_size().height.as_f32() - FOOTER_HEIGHT;
        px(available_height.max(0.) * NOTIFICATION_PANE_HEIGHT_FRACTION)
    }

    /// Begin a left-edge details resize drag.
    fn begin_details_resize(&mut self, x: Pixels, cx: &mut Context<Self>) {
        self.details_resize_grab = Some((x, self.right_pane_width));
        cx.notify();
    }

    /// Continue a left-edge details resize drag, clamped to the pane limits.
    fn update_details_resize(&mut self, x: Pixels, window: &Window, cx: &mut Context<Self>) {
        if let Some((grab_x, grab_width)) = self.details_resize_grab {
            let width = (grab_width + (grab_x - x)).as_f32().clamp(
                DETAILS_PANE_MIN_WIDTH,
                Self::details_max_width(window).as_f32(),
            );
            self.right_pane_width = px(width);
            cx.notify();
        }
    }

    /// End a left-edge details resize drag.
    fn end_details_resize(&mut self, cx: &mut Context<Self>) {
        if self.details_resize_grab.is_some() {
            self.details_resize_grab = None;
            cx.notify();
        }
    }

    /// Task list at full row width, used when the details pane overlays it.
    fn render_full_task_list(&self) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(self.task_list.clone())
            .into_any_element()
    }

    /// Details pane as a right-anchored overlay. It can be resized over the
    /// task list instead of shrinking it.
    fn render_details_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("details-overlay")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(self.right_pane_width)
            .min_w(px(DETAILS_PANE_MIN_WIDTH))
            .max_w(Self::details_max_width(window))
            .child(self.details.clone())
            .child(
                div()
                    .id("details-resize-handle")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(-4.))
                    .w(px(8.))
                    .cursor_col_resize()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.begin_details_resize(event.position.x, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                    })),
            )
            .into_any_element()
    }

    /// Show one of the two right-hand panes and focus the agent when it is
    /// the one being opened.
    fn show_right_pane(&mut self, pane: RightPane, window: &mut Window, cx: &mut Context<Self>) {
        self.right_pane = pane;
        if pane == RightPane::Agent {
            // The prompt box is not in the focus tree until the pane has
            // rendered, so defer the focus one frame.
            let agent_pane = self.agent_pane.clone();
            window.on_next_frame(move |window, cx| {
                agent_pane.update(cx, |pane, cx| pane.focus_prompt(window, cx));
            });
        }
        cx.notify();
    }

    /// Whether the right-hand panel needs to be laid out: the details pane
    /// has something to show, or the agent pane is the active one.
    fn right_pane_open(&self, cx: &App) -> bool {
        self.right_pane == RightPane::Agent || self.details.read(cx).has_selection()
    }

    /// Resolve the selected tag's launch directories and hand them to the
    /// agent pane. Nothing is spawned here: the pane only starts a process
    /// when its own state says a project is selected.
    fn sync_agent_project(&self, path: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag_name) = path.last().cloned() else {
            return;
        };
        let store = self.store.clone();
        let agent_pane = self.agent_pane.clone();
        let busy = self.nav_bar.read(cx).is_agent_busy(&tag_name);
        let layout = cx.weak_entity();
        let tag_lookup = store.get_tag_by_name(tag_name.clone(), cx);
        cx.spawn_in(window, async move |_this, cx| {
            let tag = match tag_lookup.await {
                Ok(Some(tag)) => tag,
                Ok(None) => return,
                Err(error) => {
                    tracing::error!("Failed to resolve the selected tag: {error}");
                    return;
                }
            };
            let dirs = store.tag_dirs(tag.id, cx).await.unwrap_or_default();
            let mut candidates: Vec<std::path::PathBuf> = Vec::new();
            // Directory settings are the source of truth once set: the path
            // encoded in a legacy `project:` name only counts while the tag has
            // none, so a directory removed in tag settings is not offered again
            // by the name.
            if dirs.is_empty()
                && let Some(path) = tag.name.strip_prefix("project:")
            {
                candidates.push(std::path::PathBuf::from(path));
            }
            candidates.extend(dirs.iter().map(std::path::PathBuf::from));
            let directory_backed = tag.is_project() || !dirs.is_empty();
            let project = directory_backed.then(|| AgentProject {
                tag_id: tag.id,
                tag_name: tag.name.clone(),
                label: tag.label(),
                candidates,
                checkout: None,
            });
            agent_pane.update(cx, |pane, cx| pane.set_project(project, cx));
            // The project description carries no checkout, so re-apply the
            // selected task's run: re-selecting a tag must not drop the pane
            // back onto the project directory mid-run (§6.3).
            layout
                .update(cx, |layout, cx| layout.sync_agent_checkout_for_selection(cx))
                .ok();
            // Clicking a busy project's row opens it with the agent focused.
            if busy {
                layout
                    .update_in(cx, |layout, window, cx| {
                        layout.agent_available = true;
                        layout.show_right_pane(RightPane::Agent, window, cx);
                    })
                    .ok();
            } else {
                layout
                    .update(cx, |layout, cx| {
                        layout.agent_available = directory_backed;
                        if !directory_backed && layout.right_pane == RightPane::Agent {
                            layout.right_pane = RightPane::Details;
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// Point the agent pane at the selected task's coding-run worktree, or back
    /// at the project directory when the task has no active run (§6.3). The
    /// session key is the cwd, so a worktree-backed run is a new agent session
    /// rather than a resumed interview.
    fn sync_agent_checkout(&self, task_id: u64, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let agent_pane = self.agent_pane.clone();
        let checkout = store.coding_checkout_for_task(task_id, cx);
        cx.spawn(async move |_this, cx| {
            let checkout = match checkout.await {
                Ok(checkout) => checkout,
                Err(error) => {
                    tracing::warn!("could not resolve the run's checkout: {error}");
                    return;
                }
            };
            agent_pane.update(cx, |pane, cx| pane.set_checkout(checkout, cx));
        })
        .detach();
    }

    /// Re-resolve the checkout for whatever the details panel is showing.
    fn sync_agent_checkout_for_selection(&self, cx: &mut Context<Self>) {
        if let Some(task) = self.details.read(cx).selected_task() {
            self.sync_agent_checkout(task.id, cx);
        }
    }

    /// The selected task as prompt text, or `None` when nothing is selected.
    fn task_context(&self, cx: &App) -> Option<String> {
        self.details
            .read(cx)
            .selected_task()
            .map(|task| build_task_context(&task))
    }

    /// The right-hand pane's contents. Switching panes does not tear a
    /// session down: the agent pane simply stops being rendered, and its
    /// tasks live in its own fields.
    fn render_right_pane(&self, cx: &App) -> AnyElement {
        let _ = cx;
        match self.right_pane {
            RightPane::Details => self.details.clone().into_any_element(),
            RightPane::Agent => self.agent_pane.clone().into_any_element(),
        }
    }

    /// Window-wide footer: the notifications indicator on the left, the two
    /// right-pane switchers on the right.
    fn render_pane_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let agent_enabled = self.agent_available;
        div()
            .flex_none()
            .h(px(FOOTER_HEIGHT))
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .px_2()
            .border_t_1()
            .border_color(rgb(theme::HAIRLINE))
            .bg(rgb(theme::PANEL_BG))
            .child(self.render_notifications_button(cx))
            // The indicator owns the left end; the pane switchers stay right.
            .child(div().flex_1())
            .child(
                Button::new("pane-switch-details")
                    .ghost()
                    .compact()
                    .icon(IconName::PanelRight)
                    .toggled(self.right_pane == RightPane::Details)
                    .tooltip("Show details pane")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_right_pane(RightPane::Details, window, cx)
                    })),
            )
            .child(
                Button::new("pane-switch-agent")
                    .ghost()
                    .compact()
                    .icon(IconName::Bot)
                    .toggled(self.right_pane == RightPane::Agent)
                    .disabled(!agent_enabled)
                    .tooltip(if agent_enabled {
                        "Show agent pane".to_string()
                    } else {
                        "Select a project with a directory to use the agent".to_string()
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_right_pane(RightPane::Agent, window, cx)
                    })),
            )
            .into_any_element()
    }

    /// The footer's notifications indicator: a red error icon once anything has
    /// failed, amber for warnings alone, and a plain bell while the log is
    /// clean. Pressing it toggles the pane.
    fn render_notifications_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let errors = self.notices.count(NoticeLevel::Error);
        let warnings = self.notices.count(NoticeLevel::Warning);
        let (icon, color) = if errors > 0 {
            (IconName::CircleX, theme::DANGER)
        } else if warnings > 0 {
            (IconName::TriangleAlert, theme::WARNING)
        } else {
            (IconName::Bell, theme::TEXT_MUTED)
        };
        let problems = errors + warnings;
        Button::new("footer-notifications")
            .ghost()
            .compact()
            .icon(icon)
            .toggled(self.notices_open)
            .text_color(rgb(color))
            .when(problems > 0, |this| this.label(problems.to_string()))
            .tooltip(self.notifications_tooltip())
            .on_click(cx.listener(|this, _, _, cx| {
                this.notices_open = !this.notices_open;
                cx.notify();
            }))
            .into_any_element()
    }

    /// What the footer indicator says on hover: the counts, worst first.
    fn notifications_tooltip(&self) -> String {
        let errors = self.notices.count(NoticeLevel::Error);
        let warnings = self.notices.count(NoticeLevel::Warning);
        if errors == 0 && warnings == 0 {
            return "Notifications".to_string();
        }
        let mut parts = Vec::new();
        for (count, level) in [
            (errors, NoticeLevel::Error),
            (warnings, NoticeLevel::Warning),
        ] {
            if count > 0 {
                parts.push(format!("{count} {}", level.noun()));
            }
        }
        format!("Notifications — {}", parts.join(", "))
    }

    /// One notifications filter pill. Selection is explicit: exactly one of
    /// the two options is marked selected and pressed at a time.
    fn notice_filter_pill(
        option: NoticeFilter,
        selected: bool,
        on_select: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Button {
        // Buttons install their own hover style; adding another one panics
        // at render time.
        Button::new(format!("notifications-filter-{}", option.label()))
            .ghost()
            .compact()
            .with_size(Size::Small)
            .rounded(ButtonRounded::Small)
            .label(option.label())
            .selected(selected)
            .toggled(selected)
            .tooltip(match (option, selected) {
                (NoticeFilter::Problems, true) => "Showing errors and warnings",
                (NoticeFilter::Problems, false) => "Show errors and warnings only",
                (NoticeFilter::All, true) => "Showing every message",
                (NoticeFilter::All, false) => "Show every message, informational ones included",
            })
            .cursor_pointer()
            .when(selected, |this| {
                this.bg(rgb(theme::PANEL_HOVER))
                    .border_1()
                    .border_color(rgb(theme::HAIRLINE))
                    .text_color(rgb(theme::TEXT_STRONG))
            })
            .when(!selected, |this| {
                this.border_1()
                    .border_color(rgb(theme::HAIRLINE))
                    .text_color(rgb(theme::TEXT_MUTED))
            })
            .on_click(on_select)
    }

    /// The notifications pane, expanded above the footer: everything the app
    /// reported, newest first, filtered to failures unless asked for more. It
    /// uses a fixed pixel height derived from the current viewport, with its
    /// entries scrolling underneath the header.
    fn render_notifications_pane(
        &self,
        pane_height: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let filter = self.notice_filter;
        let now = jiff::Timestamp::now();
        let shown: Vec<(usize, &Notice)> = self
            .notices
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, notice)| filter.shows(notice.level))
            .collect();
        let hidden = self.notices.entries().len() - shown.len();
        let mut list = div().v_flex().gap_1();
        if shown.is_empty() {
            list = list.child(
                div()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(self.empty_notifications_label(hidden)),
            );
        }
        for (index, notice) in shown.iter().rev() {
            list = list.child(self.notice_row(*index, notice, now));
        }
        div()
            .id("notifications-pane")
            // Floating over the content, so clicks land here and not on
            // whatever the pane covers.
            .occlude()
            .flex_none()
            .v_flex()
            .h(pane_height)
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(rgb(theme::HAIRLINE))
            .bg(rgb(theme::PANEL_BG))
            .shadow_md()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_semibold()
                            .text_color(rgb(theme::TEXT_STRONG))
                            .child("Notifications"),
                    )
                    .child(
                        Button::new("notifications-clear")
                            .ghost()
                            .compact()
                            .label("Clear")
                            .disabled(self.notices.entries().is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.notices.clear();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("notifications-close")
                            .ghost()
                            .compact()
                            .icon(IconName::Close)
                            .tooltip("Hide the notifications pane")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.notices_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .p(px(2.))
                    .bg(rgb(theme::PANEL_BG))
                    .children(NoticeFilter::options().map(|option| {
                        Self::notice_filter_pill(
                            option,
                            filter == option,
                            cx.listener(move |this, _, _, cx| {
                                this.notice_filter = option;
                                cx.notify();
                            }),
                        )
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(list),
            )
            .into_any_element()
    }

    /// What the pane says when it has nothing to show, and why.
    fn empty_notifications_label(&self, hidden: usize) -> String {
        if hidden > 0 {
            return format!(
                "No errors or warnings. {hidden} informational message{} hidden.",
                if hidden == 1 { "" } else { "s" }
            );
        }
        "Nothing reported yet.".to_string()
    }

    /// One notification row: severity, message and age.
    fn notice_row(&self, index: usize, notice: &Notice, now: jiff::Timestamp) -> AnyElement {
        div()
            .id(("notice-row", index))
            .h_flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .pt_0p5()
                    .text_color(rgb(notice.level.color()))
                    .child(notice.level.icon()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(
                        TextView::markdown(("notice-message", index), notice.message.clone())
                            .selectable(true),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(since_label(notice.at, now)),
            )
            .into_any_element()
    }

    /// Record one notification in the pane's log.
    fn log_notice(&mut self, level: NoticeLevel, message: String, cx: &mut Context<Self>) {
        self.notices.push(level, message, jiff::Timestamp::now());
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
        let store = self.store.clone();
        let create = project.tag(&self.store, cx);
        cx.spawn(async move |this, cx| {
            if let Err(e) = create.await {
                tracing::error!("Failed to create project tag: {e}");
                return;
            }
            // The tag now carries its directory, so bind its GitHub remote
            // right away when connected instead of waiting for the next
            // sync pass; the integrations card then names the repo.
            if let Err(e) = store.bind_detected_github_repos(cx).await {
                tracing::warn!("GitHub repo detection failed: {e}");
            }
            this.update(cx, |this, cx| {
                this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
                this.integrations.update(cx, |view, cx| view.refresh(cx));
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

    /// Create the tag backing a Todoist project and link it so re-syncs
    /// reuse it. When a tag with the project name already exists and another
    /// app manages it (e.g. the travel app's "Travel checklists"), a dialog
    /// offers to sync into the same tag or remap to a new `todoist/<name>`
    /// tag; unmanaged collisions keep the old namespaced copy.
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
        let layout = cx.weak_entity();
        let lookup_name = name.clone();
        let lookup = gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut backend = store.0.lock().await;
            let integration = backend
                .list_integrations()
                .await?
                .into_iter()
                .find(|i| i.provider == "todoist")
                .ok_or_else(|| anyhow::anyhow!("Todoist is not connected"))?;
            let existing = backend.get_tag_by_name(&lookup_name).await?;
            let owners = match &existing {
                Some(tag) => {
                    let mut labels = Vec::new();
                    for binding in backend.bindings_for_tag(tag.id).await? {
                        if let Some(app) = backend.app_by_id(binding.app_id).await? {
                            labels.push(app.label.clone());
                        }
                    }
                    labels
                }
                None => Vec::new(),
            };
            Ok::<_, anyhow::Error>((integration.id, existing.map(|tag| tag.id), owners))
        });
        cx.spawn_in(window, async move |_this, cx| {
            let (integration_id, existing, owners) = match lookup.await {
                Ok(found) => found,
                Err(error) => {
                    tracing::error!("Todoist sync after project pick failed: {error}");
                    return;
                }
            };
            if let (Some(tag_id), owners) = (existing, owners)
                && !owners.is_empty()
            {
                layout
                    .update_in(cx, |layout, window, cx| {
                        let weak = cx.weak_entity();
                        let store = layout.store.clone();
                        Self::open_todoist_collision_dialog(
                            weak,
                            store,
                            integration_id,
                            id.clone(),
                            name.clone(),
                            tag_id,
                            owners,
                            window,
                            cx,
                        );
                    })
                    .ok();
                return;
            }
            layout
                .update(cx, |layout, cx| {
                    let weak = cx.weak_entity();
                    let store = layout.store.clone();
                    Self::start_todoist_link_sync(
                        store,
                        weak,
                        integration_id,
                        id,
                        name,
                        existing,
                        cx,
                    );
                })
                .ok();
        })
        .detach();
    }

    /// Collision dialog: sync the Todoist project into the existing managed
    /// tag, or remap it to a fresh `todoist/<name>` tag.
    fn open_todoist_collision_dialog(
        layout: gpui::WeakEntity<Self>,
        store: Store,
        integration_id: u64,
        external_id: String,
        name: String,
        tag_id: u64,
        owners: Vec<String>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let owned_by = owners.join(", ");
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let owned_by = owned_by.clone();
            let (layout, store) = (layout.clone(), store.clone());
            let (external_id, name) = (external_id.clone(), name.clone());
            dialog
                .title(format!("\"{name}\" already exists"))
                .content(move |content, _window, _cx| {
                    let (same_layout, remap_layout) = (layout.clone(), layout.clone());
                    let (same_store, remap_store) = (store.clone(), store.clone());
                    let (same_id, remap_id) = (external_id.clone(), external_id.clone());
                    let (same_name, remap_name) = (name.clone(), name.clone());
                    content.child(
                        div()
                            .v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child(format!(
                                        "#{same_name} is managed by {owned_by}. Sync the Todoist \
                                         project into the same tag, or remap it to a new \
                                         \"todoist/{same_name}\" tag."
                                    )),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("todoist-collision-same")
                                            .compact()
                                            .label(format!("Sync into #{same_name}"))
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                Self::start_todoist_link_sync(
                                                    same_store.clone(),
                                                    same_layout.clone(),
                                                    integration_id,
                                                    same_id.clone(),
                                                    same_name.clone(),
                                                    Some(tag_id),
                                                    cx,
                                                );
                                            }),
                                    )
                                    .child(
                                        Button::new("todoist-collision-remap")
                                            .compact()
                                            .label("Use a new tag")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                Self::start_todoist_link_sync(
                                                    remap_store.clone(),
                                                    remap_layout.clone(),
                                                    integration_id,
                                                    remap_id.clone(),
                                                    remap_name.clone(),
                                                    None,
                                                    cx,
                                                );
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    /// Link the Todoist project to a tag (existing, or a fresh namespaced
    /// one on collision) and pull its sections and tasks right away.
    /// One Tokio hop: token fetch and network must never run on GPUI's
    /// executor.
    fn start_todoist_link_sync(
        store: Store,
        layout: gpui::WeakEntity<Self>,
        integration_id: u64,
        external_id: String,
        name: String,
        existing: Option<u64>,
        cx: &mut App,
    ) {
        let flow = gpui_tokio::Tokio::spawn_result(&*cx, async move {
            let tag_id = {
                let mut backend = store.0.lock().await;
                match existing {
                    Some(tag_id) => {
                        backend
                            .link_tag(integration_id, &external_id, tag_id, "project", false)
                            .await?;
                        tag_id
                    }
                    None => match backend.get_tag_by_name(&name).await? {
                        Some(_) => {
                            let scoped = format!("todoist/{name}");
                            let tag = match backend.get_tag_by_name(&scoped).await? {
                                Some(tag) => tag,
                                None => backend.create_tag(&scoped).await?,
                            };
                            backend
                                .link_tag(integration_id, &external_id, tag.id, "project", true)
                                .await?;
                            tag.id
                        }
                        None => {
                            let tag = backend.create_tag(&name).await?;
                            backend
                                .link_tag(integration_id, &external_id, tag.id, "project", false)
                                .await?;
                            tag.id
                        }
                    },
                }
            };
            let token = crate::todoist_auth::access_token().await?;
            let summary = {
                let mut backend = store.0.lock().await;
                backend
                    .sync_todoist_integration(&token, integration_id)
                    .await?
            };
            Ok::<_, anyhow::Error>((tag_id, summary))
        });
        cx.spawn(async move |cx| {
            match flow.await {
                Ok(_) => {
                    layout
                        .update(cx, |this, cx| {
                            this.nav_bar.update(cx, |nav, cx| nav.refresh_tags(cx));
                        })
                        .ok();
                }
                Err(error) => tracing::error!("Todoist sync after project pick failed: {error}"),
            }
        })
        .detach();
    }
}

impl Render for Layout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The gpui-component Root only paints its main view; overlays like
        // dialogs and notification toasts must be layered on top by the app
        // (same composition as gpui-component's story app).
        //
        // The notification layer is a full-window transparent div, so it is
        // only mounted while a toast is actually showing: otherwise it
        // would sit above the app forever (e.g. blocking the GPUI
        // inspector's element picker). The subscription re-renders this
        // view on push and on auto-dismiss, so toasts still appear and
        // vanish exactly as before.
        if self._toast_layer_refresh.is_none()
            && let Some(root) = window.root::<gpui_component::Root>().flatten()
        {
            let list = root.read(cx).notification.clone();
            self._toast_layer_refresh = Some(cx.observe(&list, |_, _, cx| cx.notify()));
        }
        let notification_layer = window
            .root::<gpui_component::Root>()
            .flatten()
            .and_then(|root| {
                if root.read(cx).notification.read(cx).notifications().is_empty() {
                    None
                } else {
                    gpui_component::Root::render_notification_layer(window, cx)
                }
            });
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        let details_open =
            self.right_pane == RightPane::Details && self.details.read(cx).has_selection();
        let can_go_back = self.task_list.read(cx).can_go_back();
        let can_go_forward = self.task_list.read(cx).can_go_forward();

        // The app's own column: title bar, content, footer. It is the only
        // in-flow child of the frame below, so the overlaid layers cannot move
        // it.
        let app = div()
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
                                // The one arrow glyph, turned around for
                                // "back".
                                .child(
                                    history_arrow(can_go_back, cx).with_transformation(
                                        Transformation::rotate(radians(std::f32::consts::PI)),
                                    ),
                                )
                                .disabled(!can_go_back)
                                .tooltip("Previous task")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.task_list.update(cx, |list, cx| list.go_back(cx));
                                })),
                        )
                        .child(
                            Button::new("history-forward")
                                .ghost()
                                .compact()
                                .child(history_arrow(can_go_forward, cx))
                                .disabled(!can_go_forward)
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
                    .child(div().w_auto().max_w(px(256.)).flex_none().child(self.nav_bar.clone()))
                    .child(match self.nav_bar.read(cx).destination().clone() {
                        // Only a tag can be automation-managed, so the special
                        // panel is reached through the tag destination alone.
                        NavDestination::Tag(_) if self.managed_tag.is_some() => div()
                            .id("managed-panel")
                            .relative()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            // The checklist items use the normal task list
                            // with its sections; its title row carries the
                            // "+ New trip" button at its end. The split
                            // docks the details pane on the right so a
                            // clicked checklist row opens it like any other.
                            .child(if self.right_pane == RightPane::Agent {
                                h_resizable(ElementId::Name("tasks-split".into()))
                                    .with_state(&self.split_state)
                                    .child(
                                        resizable_panel()
                                            .size(px(640.))
                                            .size_range(px(320.)..px(1200.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_h_0()
                                                    .min_w_0()
                                                    .flex()
                                                    .flex_col()
                                                    .child(self.task_list.clone()),
                                            ),
                                    )
                                    .child(
                                        resizable_panel()
                                            .visible(self.right_pane_open(cx))
                                            .size(px(DETAILS_PANE_WIDTH))
                                            .size_range(px(320.)..px(SPLIT_RIGHT_PANE_MAX_WIDTH))
                                            .flex_none()
                                            .child(self.render_right_pane(cx)),
                                    )
                                    .into_any_element()
                            } else {
                                self.render_full_task_list()
                            })
                            .when(details_open, |this| {
                                this.child(self.render_details_overlay(window, cx))
                            })
                            // The popover is the LAST child so GPUI paints it
                            // above the task list (paint order follows tree
                            // order).
                            .child(
                                self.travel_panel
                                    .update(cx, |panel, cx| panel.popover(window, cx)),
                            )
                            .child(
                                self.tag_settings
                                    .update(cx, |panel, cx| panel.popover(window, cx)),
                            )
                            .into_any_element(),
                        NavDestination::AllTasks | NavDestination::Tag(_) => div()
                            .id("right-column")
                            .relative()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .on_click(cx.listener(|this, _, _, cx| {
                                // Clicks inside the agent pane belong to it.
                                if this.right_pane != RightPane::Details {
                                    return;
                                }
                                let deferred = this
                                    .details
                                    .update(cx, |details, cx| details.request_clear(cx));
                                if !deferred {
                                    this.task_list
                                        .update(cx, |list, cx| list.clear_selection(cx));
                                    cx.notify();
                                }
                            }))
                            // Always two panels: the right one collapses with
                            // `.visible(false)` so the group's keyed state stays
                            // coherent (and the width survives pane switching).
                            .child(if self.right_pane == RightPane::Agent {
                                h_resizable(ElementId::Name("tasks-split".into()))
                                    .with_state(&self.split_state)
                                    .child(
                                        resizable_panel()
                                            .size(px(640.))
                                            .size_range(px(320.)..px(1200.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_h_0()
                                                    .min_w_0()
                                                    .flex()
                                                    .flex_col()
                                                    .child(self.task_list.clone()),
                                            ),
                                    )
                                    .child(
                                        resizable_panel()
                                            .visible(self.right_pane_open(cx))
                                            .size(px(DETAILS_PANE_WIDTH))
                                            .size_range(px(320.)..px(SPLIT_RIGHT_PANE_MAX_WIDTH))
                                            .flex_none()
                                            .child(self.render_right_pane(cx)),
                                    )
                                    .into_any_element()
                            } else {
                                self.render_full_task_list()
                            })
                            .when(details_open, |this| {
                                this.child(self.render_details_overlay(window, cx))
                            })
                            // The popover is the LAST child so GPUI paints it
                            // above the task list (paint order follows tree
                            // order).
                            .child(
                                self.travel_panel
                                    .update(cx, |panel, cx| panel.popover(window, cx)),
                            )
                            .child(
                                self.tag_settings
                                    .update(cx, |panel, cx| panel.popover(window, cx)),
                            )
                            .into_any_element(),
                        NavDestination::Integrations => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.integrations.clone()))
                            .into_any_element(),
                        NavDestination::Automations => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.automations.clone()))
                            .into_any_element(),
                        NavDestination::Settings => div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .min_h_0()
                            .child(div().flex_1().child(self.settings.clone()))
                            .into_any_element(),
                    }),
            )
            // The footer is window-wide chrome: it stays pinned to the
            // bottom on every panel, including Integrations, Automations
            // and Settings.
            .child(self.render_pane_footer(cx));
        let notifications_pane_height = Self::notifications_pane_height(&*window);

        div()
            .relative()
            .size_full()
            .child(app)
            // The notifications pane hangs off the footer's top edge and
            // floats over the layout: expanding it must not move anything
            // underneath.
            .when(self.notices_open, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(FOOTER_HEIGHT))
                        .child(self.render_notifications_pane(notifications_pane_height, cx)),
                )
            })
            // Toasts sit above the app but below dialogs; keep the dialog
            // layer last so it paints above everything.
            .children(notification_layer)
            .children(dialog_layer)
            .when(self.details_resize_grab.is_some(), |this| {
                this.child(
                    div()
                        .id("details-resize-capture")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .cursor_col_resize()
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                            this.update_details_resize(event.position.x, window, cx);
                        }))
                        .on_mouse_up(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.end_details_resize(cx);
                            }),
                        )
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                        })),
                )
            })
    }
}

/// The app header's history arrow: the task row's arrow glyph, tinted like
/// the ghost button that holds it. An alpha-mask SVG only paints with a
/// text color of its own, so the button's foreground — including the
/// dimmer tone it takes once navigation that way is impossible — has to be
/// repeated on the icon.
fn history_arrow(enabled: bool, cx: &App) -> Svg {
    svg()
        .size(px(14.))
        .data(ui_parts::task_row::ARROW_SVG)
        .text_color(if enabled {
            cx.theme().secondary_foreground
        } else {
            cx.theme().muted_foreground.opacity(0.5)
        })
}

fn main() {
    // The feed exists before logging so the tracing layer can publish into it.
    let notices = NoticeFeed::new();
    init_logging(notices.sink());

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        // Every view reaches the notification log through this global.
        cx.set_global(notices.sink());
        let notices = notices.into_receiver();
        gpui_tokio::init(cx);
        gpui_component::init(cx);
        ui_parts::project_picker::init(cx);
        ui_parts::task_picker::init(cx);
        ui_parts::task_details::init_tag_editor_keys(cx);
        ui_parts::agent_pane::init(cx);

        let init_store = gpui_tokio::Tokio::spawn_result(cx, async move {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = TodoStore::new(&config).await?;
            store.seed().await?;
            // The coding recipes ship with the app rather than with the demo
            // seed data, so they exist for every project.
            store.ensure_coding_recipes().await?;
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
            // Same for GitHub, whose account label comes from the token file.
            // A file with only dead secrets does not create a row: without a
            // usable token the next sync would just fail again.
            if let Some(credentials) = github_auth::stored_credentials()
                && github_auth::has_usable_credentials()
                && !store
                    .list_integrations()
                    .await?
                    .into_iter()
                    .any(|i| i.provider == "github")
            {
                store
                    .create_integration("github", credentials.login)
                    .await?;
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

        cx.spawn(move |cx: &mut AsyncApp| {
            let cx = cx.clone();
            async move {
                match init_store.await {
                    Ok((store, tasks)) => {
                        cx.open_window(TitleBar::window_options(), |window, cx| {
                            Theme::change(ThemeMode::Dark, Some(window), cx);
                            configure_notifications(cx);

                            let input = cx.new(|cx| {
                                let mut input_state = InputState::new(window, cx);
                                input_state.set_placeholder("New task...", window, cx);
                                input_state
                            });

                            let mini = cx.new(|cx| Layout::new(input, store, notices, window, cx));

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

/// Notification cards float in the bottom-right corner: out of the reading
/// line, clear of the footer's chrome, and never the full window width. The
/// slide-and-fade in and out is the component's own animation.
fn configure_notifications(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.notification.placement = Anchor::BottomRight;
    theme.notification.width = px(NOTIFICATION_WIDTH);
    theme.notification.margins.right = px(NOTIFICATION_MARGIN);
    theme.notification.margins.bottom = px(FOOTER_HEIGHT + NOTIFICATION_MARGIN);
}

/// Install logging: the fmt layer, and a layer that routes the app's own
/// warnings and errors into the notification log.
fn init_logging(notices: NoticeSink) {
    let debug = std::env::args().any(|arg| arg == "--debug" || arg == "-d");
    let filter = if debug {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .with(NoticeLayer::new(notices))
        .with(filter)
        .init();
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use gpui::TestAppContext;

    struct NoticeFilterPills;

    impl Render for NoticeFilterPills {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().h_flex().children(NoticeFilter::options().map(|option| {
                Layout::notice_filter_pill(option, option == NoticeFilter::All, |_, _, _| {})
            }))
        }
    }

    /// Both pill states must render: an unselected pill with its own hover
    /// style panics inside the button component.
    #[gpui::test]
    fn notification_filter_pills_render_without_panicking(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| NoticeFilterPills);
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

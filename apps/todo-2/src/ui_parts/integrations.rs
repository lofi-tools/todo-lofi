//! Integrations dialog: lists connectable providers (Todoist today).
//! Clicking Connect runs the provider's OAuth flow, then records the
//! connection as an `integrations` row so syncs can scope links per
//! integration.

use gpui::{
    AppContext, Context, Entity, EventEmitter, IntoElement, InteractiveElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div, px, rgb,
    prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Sizable, Size, StyledExt};

use crate::github_auth;
use crate::store::Store;
use crate::theme::{APP_BG, HAIRLINE};
use crate::todoist_auth;
use crate::ui_parts::apps::AppSettings;
use crate::ui_parts::todoist_sync::{TodoistSyncEvent, TodoistSyncPicker};

/// Demand-driven polling (decision 23): short while a PR or a run is live,
/// idle otherwise, so a quiet app barely talks to GitHub.
const GITHUB_POLL_ACTIVE: std::time::Duration = std::time::Duration::from_secs(15);
const GITHUB_POLL_IDLE: std::time::Duration = std::time::Duration::from_secs(300);

pub enum IntegrationsEvent {
    Changed,
    /// A failure the user should see wherever they are: the layout shows it
    /// as a persistent notice until dismissed.
    Notice(String),
}

pub struct IntegrationsView {
    store: Store,
    connected: Vec<storage::Integration>,
    /// The app a connected provider manages content through, so its card can
    /// carry the ownership settings (which tags it captures into).
    todoist_app_id: Option<u64>,
    settings: Entity<AppSettings>,
    todoist_sync: Option<Entity<TodoistSyncPicker>>,
    connecting: bool,
    syncing: bool,
    status: Option<String>,
    /// The device code the user is typing into GitHub while we poll for the
    /// token; `None` when no connect is in flight.
    github_code: Option<github_auth::DeviceLogin>,
    /// When the shown code stops working. GitHub's own expiry decides when the
    /// flow is over, so the card counts down instead of waiting on a poll
    /// result that may never come (§5.8).
    github_code_expires: Option<std::time::Instant>,
    /// When the integration last synced successfully, shown on the card.
    github_last_sync: Option<jiff::Timestamp>,
    github_connecting: bool,
    github_syncing: bool,
    /// Whether the GitHub card's settings (the personal token) are expanded.
    github_settings_expanded: bool,
    /// Whether the connection file holds a personal access token, which
    /// authenticates every request instead of the device-flow token.
    github_pat_saved: bool,
    /// The token field on the GitHub card, created on first render like the
    /// tag pickers below. The token itself is never echoed back into it.
    github_pat_input: Option<Entity<InputState>>,
    _github_pat_sub: Option<Subscription>,
    _load: Option<gpui::Task<()>>,
    _connect: Option<gpui::Task<()>>,
    _sync: Option<gpui::Task<()>>,
    _github_poll: Option<gpui::Task<()>>,
    _github_sync: Option<gpui::Task<()>>,
    _github_pr_poll: Option<gpui::Task<()>>,
    _github_poller: Option<gpui::Task<()>>,
    _github_tick: Option<gpui::Task<()>>,
}

impl IntegrationsView {
    pub fn new(store: Store, settings: Entity<AppSettings>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            store,
            connected: Vec::new(),
            todoist_app_id: None,
            settings,
            todoist_sync: None,
            connecting: false,
            syncing: false,
            status: None,
            github_code: None,
            github_code_expires: None,
            github_last_sync: None,
            github_connecting: false,
            github_syncing: false,
            github_settings_expanded: false,
            github_pat_saved: github_auth::has_personal_token(),
            github_pat_input: None,
            _github_pat_sub: None,
            _load: None,
            _connect: None,
            _sync: None,
            _github_poll: None,
            _github_sync: None,
            _github_pr_poll: None,
            _github_poller: None,
            _github_tick: None,
        };
        this.reload(cx);
        this.start_github_poller(cx);
        this
    }

    /// Sync in the background for the life of the window: frequent while
    /// something is pending, backing off to idle otherwise.
    fn start_github_poller(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._github_poller = Some(cx.spawn(async move |this, cx| loop {
            let pending = store.github_work_pending(cx).await.ok().unwrap_or(false);
            cx.background_executor()
                .timer(if pending {
                    GITHUB_POLL_ACTIVE
                } else {
                    GITHUB_POLL_IDLE
                })
                .await;
            let ready = this
                .read_with(cx, |this, _| this.github_connected() && !this.github_syncing)
                .unwrap_or(false);
            if !ready {
                continue;
            }
            this.update(cx, |this, cx| {
                this.start_github_sync(false, cx);
                this.poll_pull_requests(cx);
            })
            .ok();
        }));
    }

    /// Check the open pull requests of every active run on the same tick as
    /// the issue sync, so a merge completes its run without the user asking
    /// (decision 19). Transient failures simply retry on the next tick.
    fn poll_pull_requests(&mut self, cx: &mut Context<Self>) {
        let poll = self.store.poll_pull_requests(cx);
        self._github_pr_poll = Some(cx.spawn(async move |this, cx| match poll.await {
            Ok(changed) if changed > 0 => {
                // A PR changed state: the task list and the run's stepper both
                // have something new to show.
                this.update(cx, |_this, cx| cx.emit(IntegrationsEvent::Changed))
                    .ok();
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("Could not poll pull requests: {error}"),
        }));
    }

    fn github_connected(&self) -> bool {
        self.connected
            .iter()
            .any(|integration| integration.provider == "github")
    }

    /// Re-read the connections and the app behind them (used when ownership
    /// changed elsewhere).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// Collapse any expanded integration settings. Returns whether anything
    /// was open, so the window-wide Escape observer knows if it consumed
    /// the keypress.
    pub fn collapse_settings(&mut self, cx: &mut Context<Self>) -> bool {
        let mut collapsed = false;
        if self.github_settings_expanded {
            self.github_settings_expanded = false;
            collapsed = true;
        }
        if let Some(picker) = self.todoist_sync.clone()
            && picker.read_with(cx, |picker, _| picker.is_adding())
        {
            picker.update(cx, |picker, cx| picker.cancel_adding(cx));
            collapsed = true;
        }
        if let Some(app_id) = self.todoist_app_id
            && self
                .settings
                .read_with(cx, |settings, _| settings.is_expanded(app_id))
        {
            self.settings.update(cx, |settings, cx| {
                settings.toggle_expanded(app_id, cx)
            });
            collapsed = true;
        }
        if collapsed {
            cx.notify();
        }
        collapsed
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let fetch = self.store.list_integrations(cx);
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(list) => {
                // The provider's app is what owns tags, so look it up for the
                // card's settings before rendering.
                let todoist_app_id = match list.iter().find(|i| i.provider == "todoist") {
                    Some(integration) => store
                        .app_for_integration(integration.id, cx)
                        .await
                        .ok()
                        .flatten()
                        .map(|app| app.id),
                    None => None,
                };
                // The card's status line reads the last successful pass (§5.8).
                let last_sync = match list.iter().find(|i| i.provider == "github") {
                    Some(integration) => store
                        .github_last_synced(integration.id, cx)
                        .await
                        .ok()
                        .flatten(),
                    None => None,
                };
                this.update(cx, |this, cx| {
                    this.connected = list;
                    this.github_last_sync = last_sync;
                    this.github_pat_saved = github_auth::has_personal_token();
                    if this.todoist_app_id != todoist_app_id {
                        // New (or removed) provider app: drop the cached tag
                        // picker so it rebuilds for the right app.
                        this.todoist_sync = None;
                    }
                    this.todoist_app_id = todoist_app_id;
                    this._load = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("Could not load integrations: {e}"));
                    this._load = None;
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn todoist_connected(&self) -> bool {
        self.connected.iter().any(|i| i.provider == "todoist")
    }

    /// Start the device flow: ask GitHub for a code, show it, then poll until
    /// the user approves. Two hops rather than one so the code is on screen
    /// while the poll is still running.
    fn start_github_connect(&mut self, cx: &mut Context<Self>) {
        if self.github_connecting {
            return;
        }
        self.github_connecting = true;
        self.github_code = None;
        self.status = Some("Requesting a GitHub device code…".to_string());
        cx.notify();

        let begin = gpui_tokio::Tokio::spawn_result(cx, async move { github_auth::begin().await });
        self._connect = Some(cx.spawn(async move |this, cx| {
            let login = match begin.await {
                Ok(login) => login,
                Err(e) => {
                    let message = format!("GitHub connect failed: {e}");
                    tracing::error!("{message}");
                    this.update(cx, |this, cx| {
                        this.github_connecting = false;
                        this.status = Some(message.clone());
                        cx.emit(IntegrationsEvent::Notice(message));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let polled = login.clone();
            let expires_in = login.expires_in;
            this.update(cx, |this, cx| {
                let store = this.store.clone();
                // Polling and the account lookup are network work: both stay on
                // the Tokio runtime, only the result comes back to GPUI.
                let poll = gpui_tokio::Tokio::spawn_result(cx, async move {
                    let token = github_auth::complete(polled).await?;
                    Ok::<_, anyhow::Error>(github_auth::account_login(&token).await.ok())
                });
                // The code is already on the clipboard and the GitHub device
                // page already open by the time the user looks at the card.
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(login.user_code.clone()));
                todoist_auth::open_browser(&login.verification_uri);
                this.github_code = Some(login);
                this.github_code_expires = Some(
                    std::time::Instant::now() + std::time::Duration::from_secs(expires_in),
                );
                this.start_github_code_tick(cx);
                this.status = Some("Waiting for GitHub approval…".to_string());
                this._github_poll = Some(cx.spawn(async move |this, cx| {
                    let account = match poll.await {
                        Ok(account) => account,
                        Err(e) => {
                            let message = format!("GitHub connect failed: {e}");
                            tracing::error!("{message}");
                            this.update(cx, |this, cx| {
                                this.github_connecting = false;
                                this.github_code = None;
                                this.github_code_expires = None;
                                this.status = Some(message.clone());
                                cx.emit(IntegrationsEvent::Notice(message));
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    };
                    let created = store
                        .create_integration("github".to_string(), account, cx)
                        .await;
                    // Bind the project directories that already point at a
                    // github.com remote, so the first pass imports into them
                    // instead of waiting for a manual sync.
                    if let Err(e) = store.bind_detected_github_repos(cx).await {
                        tracing::warn!("GitHub repo detection failed: {e}");
                    }
                    this.update(cx, |this, cx| {
                        this.github_connecting = false;
                        this.github_code = None;
                        this.github_code_expires = None;
                        match created {
                            Ok(_) => {
                                this.status = Some("GitHub connected.".to_string());
                                cx.emit(IntegrationsEvent::Changed);
                                // The first import is quiet and starts now
                                // rather than after the poller's first tick.
                                this.start_github_sync(false, cx);
                            }
                            Err(e) => {
                                let message = format!("GitHub connect failed: {e}");
                                tracing::error!("{message}");
                                this.status = Some(message.clone());
                                cx.emit(IntegrationsEvent::Notice(message));
                            }
                        }
                        this.reload(cx);
                        cx.notify();
                    })
                    .ok();
                }));
                cx.notify();
            })
            .ok();
        }));
    }

    /// Keep the card's countdown moving while a device code is on screen, and
    /// retire the code when GitHub stops accepting it. Each tick notifies so
    /// the remaining time repaints; the task ends as soon as the code does.
    fn start_github_code_tick(&mut self, cx: &mut Context<Self>) {
        self._github_tick = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let tick = this.update(cx, |this, cx| match this.github_code_expires {
                Some(deadline) if std::time::Instant::now() >= deadline => {
                    this.github_code = None;
                    this.github_code_expires = None;
                    this.github_connecting = false;
                    this.status = Some(
                        "The GitHub device code expired. Connect again to finish.".to_string(),
                    );
                    cx.notify();
                    false
                }
                Some(_) => {
                    cx.notify();
                    true
                }
                // The connect finished, so there is nothing left to count.
                None => false,
            });
            match tick {
                Ok(true) => {}
                _ => return,
            }
        }));
    }

    /// The device code's remaining lifetime, as `m:ss`.
    fn github_code_remaining(&self) -> Option<String> {
        let deadline = self.github_code_expires?;
        Some(format_countdown(
            deadline.saturating_duration_since(std::time::Instant::now()),
        ))
    }

    /// One sync pass. A manual press is a full pass so deletions and label
    /// changes converge; the poller stays incremental.
    fn start_github_sync(&mut self, full: bool, cx: &mut Context<Self>) {
        if self.github_syncing {
            return;
        }
        self.github_syncing = true;
        self.status = Some("Syncing GitHub…".to_string());
        cx.notify();
        let sync = self.store.sync_github(full, cx);
        self._github_sync = Some(cx.spawn(async move |this, cx| match sync.await {
            Ok(summary) => {
                let message = summary.describe();
                this.update(cx, |this, cx| {
                    this.github_syncing = false;
                    this.github_last_sync = Some(jiff::Timestamp::now());
                    this.status = Some(format!("GitHub synced — {message}"));
                    cx.emit(IntegrationsEvent::Changed);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                // A permanent failure blocks with its reason; the retry
                // policy has already exhausted the transient ones (decision 22).
                let message = format!("GitHub sync failed: {e}");
                tracing::error!("{message}");
                this.update(cx, |this, cx| {
                    this.github_syncing = false;
                    this.status = Some(message.clone());
                    cx.emit(IntegrationsEvent::Notice(message));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

        /// The token field, created on first render like the pickers below.
    fn pat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = self.github_pat_input.clone() {
            return input;
        }
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("ghp_… or github_pat_…", window, cx);
            state
        });
        self._github_pat_sub = Some(cx.subscribe(&input, |this, _input, event, cx| match event {
            InputEvent::PressEnter { .. } => this.store_github_pat(None, cx),
            _ => {}
        }));
        self.github_pat_input = Some(input.clone());
        input
    }

    /// Persist the token field's contents: a blank field clears the stored
    /// token. The field is cleared after reading when a window is at hand.
    fn save_github_pat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.pat_input(window, cx).read(cx).text().to_string();
        self.pat_input(window, cx).update(cx, |input, cx| {
            input.set_value("", window, cx)
        });
        self.store_github_pat(Some(token), cx);
    }

    fn store_github_pat(&mut self, token: Option<String>, cx: &mut Context<Self>) {
        let token = token.or_else(|| {
            self.github_pat_input
                .clone()
                .map(|input| input.read(cx).text().to_string())
        });
        let save = gpui_tokio::Tokio::spawn_result(cx, async move {
            github_auth::save_personal_token(token).await
        });
        self.status = Some("Saving the personal token…".to_string());
        cx.notify();
        self._github_sync = Some(cx.spawn(async move |this, cx| match save.await {
            Ok(login) => {
                this.update(cx, |this, cx| {
                    this.github_pat_saved = login.is_some();
                    this.status = Some(match &login {
                        Some(login) => format!("Personal token saved for @{login}."),
                        None => "Personal token removed.".to_string(),
                    });
                    // A token without a connection row syncs nothing, so a
                    // first-time token also connects the integration.
                    if login.is_some() && !this.github_connected() {
                        let store = this.store.clone();
                        let created =
                            store.create_integration("github".to_string(), login, cx);
                        this._github_sync = Some(cx.spawn(async move |this, cx| {
                            match created.await {
                                Ok(_) => {
                                    this.update(cx, |this, cx| {
                                        cx.emit(IntegrationsEvent::Changed);
                                        this.reload(cx);
                                        cx.notify();
                                    })
                                    .ok();
                                }
                                Err(e) => {
                                    this.update(cx, |this, cx| {
                                        this.status =
                                            Some(format!("GitHub connect failed: {e}"));
                                        cx.notify();
                                    })
                                    .ok();
                                }
                            }
                        }));
                    } else {
                        this.reload(cx);
                    }
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                let message = format!("Personal token failed: {e}");
                tracing::error!("{message}");
                this.update(cx, |this, cx| {
                    this.github_pat_saved = github_auth::has_personal_token();
                    this.status = Some(message.clone());
                    cx.emit(IntegrationsEvent::Notice(message));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn disconnect_github(&mut self, cx: &mut Context<Self>) {

        let Some(id) = self
            .connected
            .iter()
            .find(|i| i.provider == "github")
            .map(|i| i.id)
        else {
            return;
        };
        let remove = self.store.delete_integration(id, cx);
        self._load = Some(cx.spawn(async move |this, cx| match remove.await {
            Ok(()) => {
                // The row is gone even if the token file is not: leaving a live
                // token behind silently would be worse than the extra line.
                let forgotten = github_auth::disconnect()
                    .err()
                    .map(|e| format!(" The stored token could not be removed: {e}"));
                this.update(cx, |this, cx| {
                    this.github_code = None;
                    this.github_pat_saved = false;
                    this.status = Some(match forgotten {
                        Some(note) => format!("GitHub disconnected.{note}"),
                        None => "GitHub disconnected.".to_string(),
                    });
                    cx.emit(IntegrationsEvent::Changed);
                    this.reload(cx);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("Disconnect failed: {e}"));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn start_todoist_connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        self.connecting = true;
        self.status = Some("Waiting in the browser to authorize Todoist…".to_string());
        cx.notify();

        let store = self.store.clone();
        // Network I/O must run on the Tokio runtime: `cx.spawn` polls on
        // GPUI's own executor, where reqwest/tokio panic with "there is no
        // reactor running". `Tokio::spawn_result` hops to Tokio and hands
        // the result back as a GPUI task.
        let network = gpui_tokio::Tokio::spawn_result(cx, async move {
            todoist_auth::connect().await
        });
        self._connect = Some(cx.spawn(async move |this, cx| {
            let token = match network.await {
                Ok(token) => token,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.connecting = false;
                        this.status = Some(format!("Todoist connect failed: {e}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let _ = token;
            // Token storage (keychain vs config) is still open per the
            // spec; record the connection so links can scope to it.
            let created = store
                .create_integration("todoist".to_string(), None, cx)
                .await;
            this.update(cx, |this, cx| {
                this.connecting = false;
                match created {
                    Ok(_) => {
                        this.status = Some("Todoist connected.".to_string());
                        cx.emit(IntegrationsEvent::Changed);
                    }
                    Err(e) => this.status = Some(format!("Todoist connect failed: {e}")),
                }
                this.reload(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.syncing || !self.todoist_connected() {
            return;
        }
        self.syncing = true;
        self.status = Some("Syncing Todoist…".to_string());
        cx.notify();
        let sync = self.store.sync_todoist(cx);
        self._sync = Some(cx.spawn(async move |this, cx| match sync.await {
            Ok(summary) => {
                this.update(cx, |this, cx| {
                    this.syncing = false;
                    this.status = Some(format!(
                        "Synced {} project(s): {} task(s), {} section(s), {} removed.",
                        summary.projects,
                        summary.tasks_upserted,
                        summary.sections,
                        summary.tasks_tombstoned,
                    ));
                    cx.emit(IntegrationsEvent::Changed);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    this.syncing = false;
                    this.status = Some(format!("Sync failed: {e}"));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn disconnect_todoist(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .connected
            .iter()
            .find(|i| i.provider == "todoist")
            .map(|i| i.id)
        else {
            return;
        };
        let remove = self.store.delete_integration(id, cx);
        self._load = Some(cx.spawn(async move |this, cx| match remove.await {
            Ok(()) => {
                this.update(cx, |this, cx| {
                    this.status = Some("Todoist disconnected.".to_string());
                    cx.emit(IntegrationsEvent::Changed);
                    this.reload(cx);
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("Disconnect failed: {e}"));
                    cx.notify();
                })
                .ok();
            }
        }));
    }
}

impl EventEmitter<IntegrationsEvent> for IntegrationsView {}

impl IntegrationsView {
    /// The Todoist card: connect/sync/disconnect, the synced-tags picker,
    /// plus the app's ownership settings behind the gear once connected
    /// (which tags it captures into).
    fn todoist_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let connected = self.todoist_connected();
        let app_id = self.todoist_app_id;
        let expanded = app_id.is_some_and(|app_id| {
            self.settings
                .read_with(cx, |settings, _| settings.is_expanded(app_id))
        });
        let gear = app_id.filter(|_| connected).map(|app_id| {
            self.settings
                .update(cx, |settings, cx| settings.gear_button(app_id, "Todoist", cx))
        });
        let settings_block = app_id.map(|app_id| {
            self.settings
                .update(cx, |settings, cx| settings.settings_block(app_id, 0, cx))
        });

        let actions: gpui::AnyElement = if connected {
            Button::new("todoist-disconnect")
                .ghost()
                .compact()
                .label("Disconnect")
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.disconnect_todoist(cx);
                }))
                .into_any_element()
        } else {
            Button::new("todoist-connect")
                .compact()
                .label(if self.connecting { "Waiting…" } else { "Connect" })
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.start_todoist_connect(cx);
                }))
                .into_any_element()
        };

        let mut controls = div().h_flex().items_center().gap_2().child(
            div()
                .text_sm()
                .text_color(if connected {
                    rgb(0x4ade80)
                } else {
                    rgb(0x737373)
                })
                .child(if connected { "Connected" } else { "Not connected" }),
        );
        controls = controls.child(actions);

        let description = if connected {
            "Sync projects both ways with Todoist."
        } else {
            "Sync projects both ways with Todoist. Connect to get started."
        };
        integration_card(!connected)
            .v_flex()
            .gap_3()
            .child(
                div()
                    .id("todoist-card-header")
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .when(connected && app_id.is_some(), |this| {
                        this.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                            if let Some(app_id) = this.todoist_app_id {
                                this.settings.update(cx, |settings, cx| {
                                    settings.toggle_expanded(app_id, cx)
                                });
                                cx.notify();
                            }
                        }))
                    })
                    .child(provider_icon(todoist_icon()).when(!connected, |this| {
                        this.opacity(0.45)
                    }))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .gap_0p5()
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .when(!connected, |this| {
                                                this.text_color(rgb(0x6b6b6b))
                                            })
                                            .child("Todoist"),
                                    )
                                    .when_some(gear, |this, gear| {
                                        this.child(
                                            div()
                                                .id("todoist-gear-guard")
                                                .on_click(|_, _, cx| cx.stop_propagation())
                                                .child(gear),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if connected {
                                        rgb(0xa3a3a3)
                                    } else {
                                        rgb(0x5f5f5f)
                                    })
                                    .child(description),
                            ),
                    )
                    .child(controls),
            )
            .when(expanded, |this| {
                this.when_some(settings_block, |this, block| this.child(block))
                    .child(
                        div().px_4().pb_1().h_flex().child(
                            Button::new("todoist-sync")
                                .ghost()
                                .compact()
                                .with_size(Size::Small)
                                .border_1()
                                .border_color(rgb(HAIRLINE))
                                .text_color(rgb(0xa3a3a3))
                                .cursor_pointer()
                                .label(if self.syncing { "Syncing…" } else { "Sync now" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sync_now(cx);
                                })),
                        ),
                    )
                    .child(self.todoist_sync_block(window, cx))
            })
            .into_any_element()
    }

    /// Project ↔ tag pairings for Todoist: the pair list with one
    /// "+ sync project(s)" button. The two-step mapping picker opens in a
    /// popover over the list and lands back here once paired.
    fn todoist_sync_block(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.todoist_app_id.is_none() {
            return div().into_any_element();
        }
        let picker = match self.todoist_sync.clone() {
            Some(picker) => picker,
            None => {
                let store = self.store.clone();
                let settings = self.settings.clone();
                let picker = cx.new(|cx| TodoistSyncPicker::new(store, window, cx));
                cx.subscribe(&picker, move |this, _picker, event, cx| match event {
                    TodoistSyncEvent::Changed => {
                        settings.update(cx, |settings, cx| settings.refresh(cx));
                        this.refresh(cx);
                        cx.emit(IntegrationsEvent::Changed);
                    }
                })
                .detach();
                self.todoist_sync = Some(picker.clone());
                picker
            }
        };
        div()
            .v_flex()
            .gap_1()
            .px_4()
            .pb_3()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x737373))
                    .child("Synced tags"),
            )
            .child(picker.update(cx, |picker, cx| {
                picker.render_settings_block("integrations-todoist", window, cx)
            }))
            .into_any_element()
    }
}

impl IntegrationsView {
    /// The GitHub card: device-flow connect (the code, then the poll) and
    /// disconnect once connected, plus an optional personal access token for
    /// orgs that never approved the OAuth app. The token lives only in
    /// `~/.config/my-todo/github.json`; the database holds the connection row.
    fn github_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let account = self
            .connected
            .iter()
            .find(|i| i.provider == "github")
            .and_then(|i| i.account_label.clone());
        let connected = self.github_connected();
        let expanded = self.github_settings_expanded;
        let gear = connected.then(|| {
            Button::new("github-settings")
                .ghost()
                .compact()
                .with_size(Size::Small)
                .icon(gpui_component_assets::IconName::Settings)
                .tooltip(if expanded {
                    "Hide settings".to_string()
                } else {
                    "Settings for GitHub".to_string()
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.github_settings_expanded = !this.github_settings_expanded;
                    cx.notify();
                }))
                .into_any_element()
        });

        let controls = if connected {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x4ade80))
                        .child("Connected"),
                )
                .child(
                    Button::new("github-sync")
                        .ghost()
                        .compact()
                        .label(if self.github_syncing {
                            "Syncing…"
                        } else {
                            "Sync now"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_github_sync(true, cx);
                        })),
                )
                .child(
                    Button::new("github-disconnect")
                        .ghost()
                        .compact()
                        .label("Disconnect")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.disconnect_github(cx);
                        })),
                )
        } else {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x737373))
                        .child("Not connected"),
                )
                .child(
                    Button::new("github-connect")
                        .compact()
                        .label(if self.github_connecting {
                            "Waiting…"
                        } else {
                            "Connect"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_github_connect(cx);
                        })),
                )
        };

        let code = self.github_code.clone();
        let remaining = self.github_code_remaining();
        let last_synced = self.github_last_sync.map(|at| {
            format!(
                "Last synced {}",
                since_label(at, jiff::Timestamp::now())
            )
        });
        div()
            .v_flex()
            .gap_2()
            .child(
                integration_card(!connected)
                    .v_flex()
                    .gap_3()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .child(provider_icon(gpui_component_assets::IconName::Github).when(
                                !connected,
                                |this| this.opacity(0.45),
                            ))
                            .child(
                                div()
                                    .v_flex()
                                    .flex_1()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .h_flex()
                                            .items_center()
                                            .gap_2()
                                            .child(div().font_semibold().child("GitHub"))
                                            .when(!connected, |this| {
                                                this.text_color(rgb(0x6b6b6b))
                                            })
                                            .when_some(gear, |this, gear| this.child(gear))
                                            .when_some(account, |this, account| {
                                                this.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(rgb(0x737373))
                                                        .child(account),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(if connected {
                                                rgb(0xa3a3a3)
                                            } else {
                                                rgb(0x5f5f5f)
                                            })
                                            .child("Issue-backed tasks get branches, worktrees and pull requests."),
                                    )
                                    .when_some(last_synced, |this, last_synced| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0x737373))
                                                .child(last_synced),
                                        )
                                    }),
                            )
                            .child(controls),
                    )
                    .when(expanded, |this| {
                        this.child(self.github_pat_block(window, cx))
                    }),
            )
            .when_some(code, |this, code| {
                let url = code.verification_uri.clone();
                this.child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(0x2e2e2e))
                        .bg(rgb(0x1e1e1e))
                        .px_3()
                        .py_2()
                        .v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x737373))
                                .child("Enter this code on GitHub to finish connecting"),
                        )
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_lg().font_semibold().child(code.user_code.clone()))
                                .child(
                                    Button::new("github-copy")
                                        .ghost()
                                        .compact()
                                        .label("Copy code")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let Some(code) = this.github_code.clone() else {
                                                return;
                                            };
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                code.user_code,
                                            ));
                                            this.status = Some("Code copied.".to_string());
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when_some(remaining, |this, remaining| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x737373))
                                    .child(format!("The code expires in {remaining}")),
                            )
                        })
                        .child(
                            Button::new("github-open")
                                .ghost()
                                .compact()
                                .label("Open the GitHub device page")
                                .on_click(move |_, _, _| {
                                    todoist_auth::open_browser(&url);
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    /// Optional personal access token: authenticates every request instead
    /// of the device-flow token, for orgs that never approved the OAuth app.
    /// The token is write-only on screen — the field never echoes it back.
    fn github_pat_block(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let input = self.pat_input(window, cx);
        let using = self.github_pat_saved;
        div()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x2e2e2e))
            .bg(rgb(0x1e1e1e))
            .px_3()
            .py_2()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x737373))
                    .child(if using {
                        "Syncing with a personal token. Save an empty field to remove it."
                    } else {
                        "Optional: a personal token, for orgs that never approved the app."
                    }),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&input).appearance(false)))
                    .child(
                        Button::new("github-pat-save")
                            .ghost()
                            .compact()
                            .label("Save")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_github_pat(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

/// Brand logo for Todoist (vendored SVG): the shared asset bundle only
/// ships the kit's default icon list, so brand art renders from bytes,
/// like the navbar's integrations icon.
fn todoist_icon() -> gpui_component::Icon {
    gpui_component::Icon::default()
        .data(include_bytes!("../../assets/icons/todoist.svg"))
        .with_size(Size::Large)
}

/// A countdown in the `m:ss` shape the device code block shows.
fn format_countdown(remaining: std::time::Duration) -> String {
    format!(
        "{}:{:02}",
        remaining.as_secs() / 60,
        remaining.as_secs() % 60
    )
}

/// How long ago something happened, in the coarse units a status line wants.
fn since_label(then: jiff::Timestamp, now: jiff::Timestamp) -> String {
    let seconds = (now.as_second() - then.as_second()).max(0);
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

/// Subtle card shell for one integration row. A dimmed card uses darker
/// colors rather than opacity, so its Connect button stays full-strength
/// as the one wanted interaction.
fn integration_card(dimmed: bool) -> gpui::Div {
    div()
        .rounded_lg()
        .border_1()
        .border_color(if dimmed { rgb(0x232323) } else { rgb(0x2e2e2e) })
        .bg(if dimmed { rgb(0x1a1a1a) } else { rgb(0x232323) })
        .p_4()
}

fn provider_icon(icon: impl IntoElement) -> gpui::Div {
    div()
        .w(px(40.))
        .h(px(40.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(0x1e1e1e))
        .child(icon)
}

impl Render for IntegrationsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .h_full()
            // Same surface as the task list view (APP_BG), not the darker
            // navbar tone.
            .bg(rgb(APP_BG))
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().text_xl().font_semibold().child("Integrations"))
                    .child(self.todoist_card(window, cx))
                    .child(self.github_card(window, cx))
                    .when_some(self.status.clone(), |this, status| {
                        this.child(
                            div().text_sm().text_color(rgb(0xa3a3a3)).child(status),
                        )
                    })
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card's last-sync line, which coarsens as it ages instead of
    /// reprinting a timestamp.
    #[test]
    fn the_last_sync_line_coarsens_with_age() {
        let at = |seconds: i64| jiff::Timestamp::from_second(seconds).expect("timestamp");
        let now = at(1_000_000);
        assert_eq!(since_label(at(1_000_000), now), "just now");
        assert_eq!(since_label(at(999_941), now), "just now");
        assert_eq!(since_label(at(999_940), now), "1 min ago");
        assert_eq!(since_label(at(996_400), now), "1 h ago");
        assert_eq!(since_label(at(910_000), now), "1 d ago");
        // A clock that moved backwards cannot claim to be in the future.
        assert_eq!(since_label(at(1_000_060), now), "just now");
    }

    #[test]
    fn the_device_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(
            format_countdown(std::time::Duration::from_secs(900)),
            "15:00"
        );
        assert_eq!(format_countdown(std::time::Duration::from_secs(65)), "1:05");
        assert_eq!(format_countdown(std::time::Duration::ZERO), "0:00");
    }
}

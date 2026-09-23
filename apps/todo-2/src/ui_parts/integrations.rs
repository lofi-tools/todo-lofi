//! Integrations dialog: lists connectable providers (Todoist today).
//! Clicking Connect runs the provider's OAuth flow, then records the
//! connection as an `integrations` row so syncs can scope links per
//! integration.

use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextView;
use gpui_component::{Disableable, Sizable, Size, StyledExt};

use crate::github_auth;
use crate::store::Store;
use crate::theme::{APP_BG, DANGER, HAIRLINE};
use crate::todoist_auth;
use crate::ui_parts::apps::AppSettings;
use crate::ui_parts::notifications::{self, NoticeLevel};
use crate::ui_parts::todoist_sync::{TodoistSyncEvent, TodoistSyncPicker};

/// Demand-driven polling (decision 23): short while a PR or a run is live,
/// idle otherwise, so a quiet app barely talks to GitHub. The idle cadence
/// is the card's sync frequency; the active one stays fixed.
const GITHUB_POLL_ACTIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// A pass that started within this window already holds what an immediately
/// following one would fetch, so opening projects in quick succession does
/// not line up passes back to back.
const GITHUB_SYNC_FRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// Sync frequency presets for the GitHub card: `(seconds, label)`.
/// `0` means manual syncing only.
const GITHUB_POLL_PRESETS: [(u64, &str); 4] = [
    (0, "Manual"),
    (60, "1 min"),
    (300, "5 min"),
    (900, "15 min"),
];

const GITHUB_NEW_CLASSIC_TOKEN_URL: &str = concat!(
    "https://github.com/settings/tokens/new",
    // A non-expiring token keeps background syncing from silently failing
    // once the default 30 days are up.
    "?scopes=repo&description=todo-lofi&default_expires_at=none",
);

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
    /// Whether that app is enabled. A connected integration whose app is
    /// disabled keeps its row for the history but stops syncing, and its
    /// card offers only re-enabling.
    todoist_app_enabled: bool,
    /// The app behind the GitHub integration, so the card can offer the
    /// same disable/re-enable section as Todoist. Disabling stops syncing
    /// while the integration row and synced data stay for re-enabling.
    github_app_id: Option<u64>,
    github_app_enabled: bool,
    settings: Entity<AppSettings>,
    todoist_sync: Option<Entity<TodoistSyncPicker>>,
    connecting: bool,
    syncing: bool,
    status: Option<String>,
    /// GitHub's own status line ("GitHub synced — …", errors, token notes),
    /// rendered inside the expanded GitHub card rather than at the page
    /// bottom, so each integration owns its feedback.
    github_status: Option<String>,
    /// The device code the user is typing into GitHub while we poll for the
    /// token; `None` when no connect is in flight.
    github_code: Option<github_auth::DeviceLogin>,
    /// When the shown code stops working. GitHub's own expiry decides when the
    /// flow is over, so the card counts down instead of waiting on a poll
    /// result that may never come (§5.8).
    github_code_expires: Option<std::time::Instant>,
    /// When the integration last synced successfully, shown on the card.
    github_last_sync: Option<jiff::Timestamp>,
    /// `owner/repo` of every repo this integration syncs with (detected
    /// remotes and explicit bindings alike), shown on the card.
    github_repos: Vec<String>,
    github_connecting: bool,
    github_syncing: bool,
    /// Background sync cadence in seconds (`0` means manual only), mirrored
    /// from the connection file so the card can render it without file I/O
    /// on every frame.
    github_poll_interval: u64,
    /// Whether the GitHub card's settings (the personal token) are expanded.
    github_settings_expanded: bool,
    /// Whether the connection file holds a personal access token, which
    /// authenticates every request instead of the device-flow token.
    github_pat_saved: bool,
    /// The token field on the GitHub card, created on first render like the
    /// tag pickers below. The token itself is never echoed back into it.
    github_pat_input: Option<Entity<InputState>>,
    /// When the last pass was started, so an automatic pass only goes out
    /// when the data on screen can still be stale.
    github_sync_started: Option<std::time::Instant>,
    /// Whether the launch pass has gone out. The connection's first load
    /// starts it; later reloads leave the cadence to the poller.
    github_launch_sync_done: bool,
    _github_pat_sub: Option<Subscription>,
    _load: Option<gpui::Task<()>>,
    _connect: Option<gpui::Task<()>>,
    _sync: Option<gpui::Task<()>>,
    _github_poll: Option<gpui::Task<()>>,
    _github_sync: Option<gpui::Task<()>>,
    _github_project_sync: Option<gpui::Task<()>>,
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
            todoist_app_enabled: true,
            github_app_id: None,
            github_app_enabled: true,
            settings,
            todoist_sync: None,
            connecting: false,
            syncing: false,
            status: None,
            github_status: None,
            github_code: None,
            github_code_expires: None,
            github_last_sync: None,
            github_repos: Vec::new(),
            github_connecting: false,
            github_syncing: false,
            github_poll_interval: github_auth::poll_interval_secs(),
            github_settings_expanded: false,
            github_pat_saved: github_auth::has_personal_token(),
            github_pat_input: None,
            github_sync_started: None,
            github_launch_sync_done: false,
            _github_pat_sub: None,
            _load: None,
            _connect: None,
            _sync: None,
            _github_poll: None,
            _github_sync: None,
            _github_project_sync: None,
            _github_pr_poll: None,
            _github_poller: None,
            _github_tick: None,
        };
        this.reload(cx);
        this.start_github_poller(cx);
        this
    }

    /// Sync in the background for the life of the window: frequent while
    /// something is pending, backing off to the card's sync frequency
    /// otherwise. Manual frequency means no automatic syncing at all.
    fn start_github_poller(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._github_poller = Some(cx.spawn(async move |this, cx| {
            loop {
                let cadence = this
                    .read_with(cx, |this, _| this.github_poll_interval)
                    .unwrap_or(github_auth::DEFAULT_POLL_INTERVAL_SECS);
                if cadence == 0 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(60))
                        .await;
                    continue;
                }
                let pending = store.github_work_pending(cx).await.ok().unwrap_or(false);
                cx.background_executor()
                    .timer(if pending {
                        GITHUB_POLL_ACTIVE
                    } else {
                        std::time::Duration::from_secs(cadence)
                    })
                    .await;
                let ready = this
                    .read_with(cx, |this, _| {
                        this.github_connected()
                            && github_auth::has_usable_credentials()
                            && !this.github_disabled()
                            && !this.github_syncing
                    })
                    .unwrap_or(false);
                if !ready {
                    continue;
                }
                this.update(cx, |this, cx| {
                    this.start_github_sync(false, cx);
                    this.poll_pull_requests(cx);
                })
                .ok();
            }
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
            self.settings
                .update(cx, |settings, cx| settings.toggle_expanded(app_id, cx));
            collapsed = true;
        }
        if collapsed {
            cx.notify();
        }
        collapsed
    }

    /// Toggle the Todoist card. Only one integration card is expanded at a
    /// time, so expanding it collapses the GitHub card.
    fn toggle_todoist_expanded(&mut self, cx: &mut Context<Self>) {
        let Some(app_id) = self.todoist_app_id else {
            return;
        };
        let will_expand = self
            .settings
            .read_with(cx, |settings, _| !settings.is_expanded(app_id));
        self.settings
            .update(cx, |settings, cx| settings.toggle_expanded(app_id, cx));
        if will_expand {
            self.github_settings_expanded = false;
        }
        cx.notify();
    }

    /// Toggle the GitHub card. Only one integration card is expanded at a
    /// time, so expanding it collapses the Todoist card.
    fn toggle_github_expanded(&mut self, cx: &mut Context<Self>) {
        self.set_github_expanded(!self.github_settings_expanded, cx);
    }

    /// Set the GitHub card's expansion, collapsing the Todoist card when
    /// expanding so only one is open at a time.
    fn set_github_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.github_settings_expanded = expanded;
        if expanded && let Some(app_id) = self.todoist_app_id {
            self.settings.update(cx, |settings, cx| {
                if settings.is_expanded(app_id) {
                    settings.toggle_expanded(app_id, cx);
                }
            });
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let fetch = self.store.list_integrations(cx);
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(list) => {
                // The provider's app is what owns tags, so look it up for the
                // card's settings before rendering.
                let todoist_app = match list.iter().find(|i| i.provider == "todoist") {
                    Some(integration) => store
                        .app_for_integration(integration.id, cx)
                        .await
                        .ok()
                        .flatten()
                        .map(|app| (app.id, app.enabled)),
                    None => None,
                };
                let github_app = match list.iter().find(|i| i.provider == "github") {
                    Some(integration) => store
                        .app_for_integration(integration.id, cx)
                        .await
                        .ok()
                        .flatten()
                        .map(|app| (app.id, app.enabled)),
                    None => None,
                };
                // The card's status line reads the last successful pass (§5.8),
                // and its repo line the bound repos (detected or explicit).
                let (last_sync, bound_repos) = match list.iter().find(|i| i.provider == "github") {
                    Some(integration) => {
                        let last_sync = store
                            .github_last_synced(integration.id, cx)
                            .await
                            .ok()
                            .flatten();
                        let repos = store
                            .github_bound_repos(integration.id, cx)
                            .await
                            .map(|repos| {
                                repos.into_iter().map(|repo| repo.external_id()).collect()
                            })
                            .unwrap_or_default();
                        (last_sync, repos)
                    }
                    None => (None, Vec::new()),
                };
                this.update(cx, |this, cx| {
                    this.connected = list;
                    this.github_last_sync = last_sync;
                    this.github_repos = bound_repos;
                    this.github_pat_saved = github_auth::has_personal_token();
                    this.github_poll_interval = github_auth::poll_interval_secs();
                    let todoist_app_id = todoist_app.map(|(id, _)| id);
                    if this.todoist_app_id != todoist_app_id {
                        // New (or removed) provider app: drop the cached tag
                        // picker so it rebuilds for the right app.
                        this.todoist_sync = None;
                    }
                    this.todoist_app_id = todoist_app_id;
                    this.todoist_app_enabled =
                        todoist_app.map(|(_, enabled)| enabled).unwrap_or(true);
                    this.github_app_id = github_app.map(|(id, _)| id);
                    this.github_app_enabled =
                        github_app.map(|(_, enabled)| enabled).unwrap_or(true);
                    this._load = None;
                    this.start_github_launch_sync(cx);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    let message = format!("Could not load integrations: {e}");
                    notifications::report(cx, NoticeLevel::Error, message.clone());
                    this.status = Some(message);
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

    /// A connected integration whose app was disabled: the row stays for the
    /// history, syncing stopped, and the card offers only re-enabling.
    fn todoist_disabled(&self) -> bool {
        self.todoist_connected() && self.todoist_app_id.is_some() && !self.todoist_app_enabled
    }

    fn github_disabled(&self) -> bool {
        self.github_connected() && self.github_app_id.is_some() && !self.github_app_enabled
    }

    /// Disable stops syncing but keeps the integration row and synced data,
    /// so it can be re-enabled later. Items are always kept.
    fn disable_integration_app(&mut self, app_id: u64, label: &str, cx: &mut Context<Self>) {
        let remove = self.store.remove_app(app_id, false, cx);
        let label = label.to_string();
        self._load = Some(cx.spawn(async move |this, cx| match remove.await {
            Ok(()) => {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("{label} disabled. Synced data kept."));
                    cx.emit(IntegrationsEvent::Changed);
                    this.reload(cx);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    let message = format!("Disable failed: {e}");
                    notifications::report(cx, NoticeLevel::Error, message.clone());
                    this.status = Some(message);
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn re_enable_integration_app(&mut self, app_id: u64, label: &str, cx: &mut Context<Self>) {
        let enable = self.store.set_app_enabled(app_id, true, cx);
        let label = label.to_string();
        self._load = Some(cx.spawn(async move |this, cx| match enable.await {
            Ok(()) => {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("{label} re-enabled."));
                    cx.emit(IntegrationsEvent::Changed);
                    this.reload(cx);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    let message = format!("Re-enable failed: {e}");
                    notifications::report(cx, NoticeLevel::Error, message.clone());
                    this.status = Some(message);
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// Bottom settings section for an expanded card: the red disable button,
    /// or re-enable once disabled. Rendered by both integration cards.
    fn integration_disable_section(
        &self,
        app_id: Option<u64>,
        disabled: bool,
        label: &str,
        id_prefix: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(app_id) = app_id else {
            return div().into_any_element();
        };
        let label = label.to_string();
        let button_id = format!("{id_prefix}-disable-toggle");
        let button_label = if disabled {
            format!("Re-enable {label}")
        } else {
            format!("Disable {label}")
        };
        div()
            .px_4()
            .pb_3()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .min_h(px(28.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(if disabled {
                                "Disabled. Synced data is kept; re-enable to resume syncing."
                            } else {
                                "Disable stops syncing but keeps synced data."
                            }),
                    )
                    .child(
                        Button::new(button_id)
                            .ghost()
                            .compact()
                            .with_size(Size::Small)
                            .when(!disabled, |this| this.text_color(rgb(DANGER)))
                            .label(button_label)
                            .tooltip(if disabled {
                                "Resume syncing this integration".to_string()
                            } else {
                                "Stop syncing but keep synced data".to_string()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if disabled {
                                    this.re_enable_integration_app(app_id, &label, cx);
                                } else {
                                    this.disable_integration_app(app_id, &label, cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
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
        self.github_status = Some("Requesting a GitHub device code…".to_string());
        self.set_github_expanded(true, cx);
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
                        this.github_status = Some(message.clone());
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
                this.github_code_expires =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(expires_in));
                this.start_github_code_tick(cx);
                this.github_status = Some("Waiting for GitHub approval…".to_string());
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
                                this.github_status = Some(message.clone());
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
                                this.github_status = Some("GitHub connected.".to_string());
                                cx.emit(IntegrationsEvent::Changed);
                                // The first import is quiet and starts now
                                // rather than after the poller's first tick.
                                this.start_github_sync(false, cx);
                            }
                            Err(e) => {
                                let message = format!("GitHub connect failed: {e}");
                                tracing::error!("{message}");
                                this.github_status = Some(message.clone());
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
        self._github_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let tick = this.update(cx, |this, cx| match this.github_code_expires {
                    Some(deadline) if std::time::Instant::now() >= deadline => {
                        this.github_code = None;
                        this.github_code_expires = None;
                        this.github_connecting = false;
                        let message =
                            "The GitHub device code expired. Connect again to finish.".to_string();
                        notifications::report(cx, NoticeLevel::Warning, message.clone());
                        this.github_status = Some(message);
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
        if self.github_syncing || self.github_disabled() {
            return;
        }
        self.github_syncing = true;
        self.github_sync_started = Some(std::time::Instant::now());
        self.github_status = Some("Syncing GitHub…".to_string());
        self.set_github_expanded(true, cx);
        cx.notify();
        let sync = self.store.sync_github(full, cx);
        self._github_sync = Some(cx.spawn(async move |this, cx| match sync.await {
            Ok(summary) => {
                let message = summary.describe();
                // A pass that found nothing is the common case: the poller runs
                // every few seconds while a run is pending. Announcing it would
                // reload every surface synced content appears on — the tag
                // tree, the task list, the selected task — for no change, and
                // the reload costs rows their measured heights. The card still
                // gets its timestamp, which is inside this same update.
                let changed = !summary.is_empty();
                this.update(cx, |this, cx| {
                    this.github_syncing = false;
                    this.github_last_sync = Some(jiff::Timestamp::now());
                    this.github_status = Some(format!("GitHub synced — {message}"));
                    if changed {
                        cx.emit(IntegrationsEvent::Changed);
                    }
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
                    this.github_status = Some(message.clone());
                    cx.emit(IntegrationsEvent::Notice(message));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The id of the connected GitHub integration, if there is one.
    fn github_integration_id(&self) -> Option<u64> {
        self.connected
            .iter()
            .find(|integration| integration.provider == "github")
            .map(|integration| integration.id)
    }

    /// Start a pass now, for callers that cannot wait out the poller's next
    /// tick: the launch of the app and every GitHub-backed project that is
    /// opened. A pass already in flight is left alone, and so is a connection
    /// that is absent, disabled, or short of credentials — those would only
    /// produce a failure notice the user cannot act on.
    pub fn sync_github_now(&mut self, cx: &mut Context<Self>) {
        if !self.github_connected()
            || self.github_disabled()
            || self.github_syncing
            || !github_auth::has_usable_credentials()
        {
            return;
        }
        if self
            .github_sync_started
            .is_some_and(|started| started.elapsed() < GITHUB_SYNC_FRESH)
        {
            return;
        }
        self.start_github_sync(false, cx);
    }

    /// The connection's first load is the launch moment for GitHub: a pass
    /// goes out then and there, so the projects shown on startup have their
    /// tasks without waiting out the poller's first tick.
    fn start_github_launch_sync(&mut self, cx: &mut Context<Self>) {
        if self.github_launch_sync_done {
            return;
        }
        self.github_launch_sync_done = true;
        self.sync_github_now(cx);
    }

    /// A project was opened. When GitHub is what backs it, pass now rather
    /// than at the next tick, so its tasks arrive with the view. Local
    /// projects cost two reads and no network.
    pub fn project_opened(&mut self, tag_name: String, cx: &mut Context<Self>) {
        if !self.github_connected() || self.github_disabled() {
            return;
        }
        let Some(integration_id) = self.github_integration_id() else {
            return;
        };
        let store = self.store.clone();
        self._github_project_sync = Some(cx.spawn(async move |this, cx| {
            if !project_is_github_backed(&store, integration_id, tag_name, cx).await {
                return;
            }
            this.update(cx, |this, cx| this.sync_github_now(cx)).ok();
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
        self._github_pat_sub = Some(cx.subscribe(&input, |this, input, event, cx| match event {
            // Enter saves a typed token; with nothing typed there is nothing
            // to save, matching the Save button's disabled state.
            InputEvent::PressEnter { .. } => {
                if token_field_filled(&input.read(cx).text().to_string()) {
                    this.store_github_pat(None, cx);
                }
            }
            // Every edit re-renders the card, so Save follows the field.
            InputEvent::Change => cx.notify(),
            _ => {}
        }));
        self.github_pat_input = Some(input.clone());
        input
    }

    /// Persist the token field's contents and clear the field. Only reachable
    /// with something typed; a blank field means removal, which the dedicated
    /// Remove button does instead.
    fn save_github_pat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.pat_input(window, cx).read(cx).text().to_string();
        self.pat_input(window, cx)
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.store_github_pat(Some(token), cx);
    }

    /// Drop the stored personal token, falling back to the device-flow
    /// credential.
    fn remove_github_pat(&mut self, cx: &mut Context<Self>) {
        self.store_github_pat(Some(String::new()), cx);
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
        self.github_status = Some("Saving the personal token…".to_string());
        self.set_github_expanded(true, cx);
        cx.notify();
        self._github_sync = Some(cx.spawn(async move |this, cx| match save.await {
            Ok(login) => {
                this.update(cx, |this, cx| {
                    this.github_pat_saved = login.is_some();
                    this.github_status = Some(match &login {
                        Some(login) => format!("Personal token saved for @{login}."),
                        None => "Personal token removed.".to_string(),
                    });
                    // A token without a connection row syncs nothing, so a
                    // first-time token also connects the integration.
                    if login.is_some() && !this.github_connected() {
                        let store = this.store.clone();
                        let created = store.create_integration("github".to_string(), login, cx);
                        this._github_sync =
                            Some(cx.spawn(async move |this, cx| match created.await {
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
                                        let message = format!("GitHub connect failed: {e}");
                                        notifications::report(
                                            cx,
                                            NoticeLevel::Error,
                                            message.clone(),
                                        );
                                        this.github_status = Some(message);
                                        cx.notify();
                                    })
                                    .ok();
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
                    this.github_status = Some(message.clone());
                    cx.emit(IntegrationsEvent::Notice(message));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// Persist the card's sync frequency: the poller picks it up on its next
    /// tick, so nothing restarts.
    fn set_github_poll_interval(&mut self, secs: u64, cx: &mut Context<Self>) {
        match github_auth::set_poll_interval(secs) {
            Ok(()) => {
                self.github_poll_interval = secs;
                self.github_status = Some(if secs == 0 {
                    "Automatic syncing off. Use Sync now for on-demand passes.".to_string()
                } else {
                    format!("Background sync {}.", Self::poll_preset_label(secs))
                });
                cx.notify();
            }
            Err(e) => {
                let message = format!("Sync frequency failed: {e}");
                notifications::report(cx, NoticeLevel::Warning, message.clone());
                self.github_status = Some(message);
                cx.notify();
            }
        }
    }

    fn poll_preset_label(secs: u64) -> String {
        GITHUB_POLL_PRESETS
            .iter()
            .find(|(preset, _)| *preset == secs)
            .map(|(_, label)| format!("every {label}"))
            .unwrap_or_else(|| format!("every {secs}s"))
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
        let network =
            gpui_tokio::Tokio::spawn_result(cx, async move { todoist_auth::connect().await });
        self._connect = Some(cx.spawn(async move |this, cx| {
            let token = match network.await {
                Ok(token) => token,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.connecting = false;
                        let message = format!("Todoist connect failed: {e}");
                        notifications::report(cx, NoticeLevel::Error, message.clone());
                        this.status = Some(message);
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
                    Err(e) => {
                        let message = format!("Todoist connect failed: {e}");
                        notifications::report(cx, NoticeLevel::Error, message.clone());
                        this.status = Some(message);
                    }
                }
                this.reload(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.syncing || !self.todoist_connected() || self.todoist_disabled() {
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
                        "Synced {} project: {} task(s), {} section(s), {} removed.",
                        summary.projects,
                        summary.tasks_upserted,
                        summary.sections,
                        summary.tasks_tombstoned,
                    ));
                    // Unlike the GitHub pass above, this one is a user action
                    // ("Sync now", or the sync that follows pairing a project),
                    // so it always reports: the refresh it drives is the answer
                    // the user asked for, whatever the summary says.
                    cx.emit(IntegrationsEvent::Changed);
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    this.syncing = false;
                    let message = format!("Sync failed: {e}");
                    notifications::report(cx, NoticeLevel::Error, message.clone());
                    this.status = Some(message);
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
        let disabled = self.todoist_disabled();
        let dimmed = !connected || disabled;
        let app_id = self.todoist_app_id;
        let expanded = app_id.is_some_and(|app_id| {
            self.settings
                .read_with(cx, |settings, _| settings.is_expanded(app_id))
        });
        let gear = app_id.filter(|_| connected).map(|_| {
            Button::new("todoist-settings")
                .ghost()
                .compact()
                .with_size(Size::Small)
                .icon(gpui_component_assets::IconName::Settings)
                .tooltip(if expanded {
                    "Hide settings".to_string()
                } else {
                    "Settings for Todoist".to_string()
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_todoist_expanded(cx);
                }))
                .into_any_element()
        });
        let settings_block = app_id.map(|app_id| {
            self.settings.update(cx, |settings, cx| {
                settings.settings_block_without_disable(app_id, 0, cx)
            })
        });

        // The collapsed header carries no actions besides Connect: syncing
        // lives in the expanded settings, and disabling in its bottom
        // section. Only the status text shows here.
        let actions: gpui::AnyElement = if connected {
            div().into_any_element()
        } else {
            Button::new("todoist-connect")
                .compact()
                .label(if self.connecting {
                    "Waiting…"
                } else {
                    "Connect"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.start_todoist_connect(cx);
                }))
                .into_any_element()
        };

        let mut controls = div().h_flex().items_center().gap_2().child(
            div()
                .text_sm()
                .text_color(if connected && !disabled {
                    rgb(0x4ade80)
                } else {
                    rgb(0x737373)
                })
                .child(if disabled {
                    "Disabled"
                } else if connected {
                    "Connected"
                } else {
                    "Not connected"
                }),
        );
        if !connected {
            controls = controls.child(actions);
        }
        controls = controls.flex_none();

        let description = if disabled {
            "Todoist is disabled. Expand to re-enable; synced data is kept."
        } else if connected {
            "Sync projects both ways with Todoist."
        } else {
            "Sync projects both ways with Todoist. Connect to get started."
        };
        integration_card(dimmed)
            .v_flex()
            .gap_3()
            .child(
                div()
                    .id("todoist-card-header")
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .when(connected && app_id.is_some(), |this| {
                        this.cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_todoist_expanded(cx);
                            }))
                    })
                    .child(provider_icon(todoist_icon()).when(dimmed, |this| this.opacity(0.45)))
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
                                            .when(dimmed, |this| this.text_color(rgb(0x6b6b6b)))
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
                                    .text_color(if dimmed { rgb(0x5f5f5f) } else { rgb(0xa3a3a3) })
                                    .child(description),
                            ),
                    )
                    .child(controls),
            )
            .when(expanded, |this| {
                this.when_some(settings_block, |this, block| this.child(block))
                    .when(!disabled, |this| {
                        this.child(
                            div().px_4().pb_1().h_flex().child(
                                Button::new("todoist-sync")
                                    .ghost()
                                    .compact()
                                    .with_size(Size::Small)
                                    .border_1()
                                    .border_color(rgb(HAIRLINE))
                                    .text_color(rgb(0xa3a3a3))
                                    .cursor_pointer()
                                    .label(if self.syncing {
                                        "Syncing…"
                                    } else {
                                        "Sync now"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sync_now(cx);
                                    })),
                            ),
                        )
                        .child(self.todoist_sync_block(window, cx))
                    })
                    .child(
                        self.integration_disable_section(
                            app_id, disabled, "Todoist", "todoist", cx,
                        ),
                    )
            })
            .into_any_element()
    }

    /// Project ↔ tag pairings for Todoist: the pair list with one
    /// "+ sync project" button. The two-step mapping picker opens in a
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
        let disabled = self.github_disabled();
        let dimmed = !connected || disabled;
        let github_app_id = self.github_app_id;
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
                    this.toggle_github_expanded(cx);
                }))
                .into_any_element()
        });

        // The collapsed header carries no actions besides Connect: Sync now
        // lives in the expanded settings next to the sync stats, and
        // disabling in the bottom section. Only the status text shows here.
        let controls = if disabled {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(div().text_sm().text_color(rgb(0x737373)).child("Disabled"))
        } else if connected {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(div().text_sm().text_color(rgb(0x4ade80)).child("Connected"))
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
                    cx.stop_propagation();
                    this.start_github_connect(cx);
                })),
            )
        };
        let controls = controls.flex_none();

        let code = self.github_code.clone();
        let remaining = self.github_code_remaining();
        // The connected repo, when the integration syncs with exactly one.
        // Auto-detected remotes and explicit bindings both land here through
        // `bound_repos`, so a single-repo setup names it on the card.
        let bound_repos = self.github_repos.clone();
        let repo_line: Option<String> = if !connected || disabled {
            None
        } else if bound_repos.len() == 1 {
            bound_repos.first().cloned()
        } else if bound_repos.is_empty() {
            None
        } else {
            Some(format!("{} repos", bound_repos.len()))
        };
        div()
            .w_full()
            .min_w_0()
            .v_flex()
            .gap_2()
            .child(
                integration_card(dimmed)
                    .w_full()
                    .flex_shrink_0()
                    .v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("github-card-header")
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .when(connected, |this| {
                                this.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_github_expanded(cx);
                                }))
                            })
                            .child(provider_icon(gpui_component_assets::IconName::Github).when(
                                dimmed,
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
                                            .when(dimmed, |this| {
                                                this.text_color(rgb(0x6b6b6b))
                                            })
                                            .when_some(gear, |this, gear| {
                                                this.child(
                                                    div()
                                                        .id("github-gear-guard")
                                                        .on_click(|_, _, cx| cx.stop_propagation())
                                                        .child(gear),
                                                )
                                            })
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
                                            .text_color(if dimmed {
                                                rgb(0x5f5f5f)
                                            } else {
                                                rgb(0xa3a3a3)
                                            })
                                            .child(if disabled {
                                                "GitHub is disabled. Expand to re-enable; synced data is kept."
                                            } else {
                                                "Sync github issues & projects, auto-create branches & pull requests"
                                            }),
                                    )
                                    .when_some(repo_line, |this, repo_line| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0x737373))
                                                .child(repo_line),
                                        )
                                    })
                            )
                            .child(controls),
                    )
                    .when(expanded, |this| {
                        this.child(self.github_sync_block(cx))
                            .when(!disabled, |this| {
                                this.child(self.github_pat_block(window, cx))
                            })
                            .child(self.integration_disable_section(
                                github_app_id,
                                disabled,
                                "GitHub",
                                "github",
                                cx,
                            ))
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
                                            this.github_status = Some("Code copied.".to_string());
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

    /// Sync settings at the top of the expanded GitHub card: the background
    /// frequency, then one row with the last pass, the synced quantities and
    /// the Sync now button.
    fn github_sync_block(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let interval = self.github_poll_interval;
        let disabled = self.github_disabled();
        let last_synced = self
            .github_last_sync
            .map(|at| format!("Last synced {}", since_label(at, jiff::Timestamp::now())));
        let status = self.github_status.clone();
        let syncing = self.github_syncing;
        let mut presets = div().h_flex().items_center().gap_1();
        for (secs, label) in GITHUB_POLL_PRESETS {
            let selected = interval == secs;
            presets = presets.child(
                Button::new(format!("github-frequency-{secs}"))
                    .ghost()
                    .compact()
                    .with_size(Size::Small)
                    .when(selected, |this| {
                        this.border_1()
                            .border_color(rgb(HAIRLINE))
                            .text_color(rgb(0xa3a3a3))
                    })
                    .when(!selected, |this| this.text_color(rgb(0x737373)))
                    .cursor_pointer()
                    .label(label)
                    .tooltip(if secs == 0 {
                        "No automatic syncing; sync on demand".to_string()
                    } else {
                        format!("Sync automatically every {label}")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_github_poll_interval(secs, cx);
                    })),
            );
        }
        div()
            .v_flex()
            .gap_2()
            .px_4()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child("Sync frequency"),
                    )
                    .child(presets),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .when_some(last_synced, |this, last_synced| {
                                this.child(
                                    div().text_xs().text_color(rgb(0x737373)).child(last_synced),
                                )
                            })
                            .when_some(status, |this, status| {
                                let copy_text = status.clone();
                                this.child(
                                    div()
                                        .h_flex()
                                        .items_start()
                                        .gap_1()
                                        // Selectable so a failure payload can be
                                        // read and dragged out; the button
                                        // copies the message whole.
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_sm()
                                                .text_color(rgb(0xa3a3a3))
                                                .child(
                                                    TextView::markdown("github-status", status)
                                                        .selectable(true),
                                                ),
                                        )
                                        .child(
                                            Button::new("github-status-copy")
                                                .ghost()
                                                .compact()
                                                .icon(gpui_component_assets::IconName::Copy)
                                                .tooltip("Copy this message")
                                                .on_click(move |_, _, cx: &mut gpui::App| {
                                                    cx.write_to_clipboard(
                                                        gpui::ClipboardItem::new_string(
                                                            copy_text.clone(),
                                                        ),
                                                    );
                                                }),
                                        ),
                                )
                            }),
                    )
                    .when(!disabled, |this| {
                        this.child(
                            Button::new("github-sync")
                                .ghost()
                                .compact()
                                .with_size(Size::Small)
                                .border_1()
                                .border_color(rgb(HAIRLINE))
                                .text_color(rgb(0xa3a3a3))
                                .cursor_pointer()
                                .label(if syncing { "Syncing…" } else { "Sync now" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.start_github_sync(true, cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Optional personal access token: authenticates every request instead
    /// of the device-flow token, for orgs that never approved the OAuth app.
    /// The token is write-only on screen — the field never echoes it back —
    /// so the block carries the how-to-make-one steps, each with the button
    /// that opens the page it is about.
    fn github_pat_block(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let input = self.pat_input(window, cx);
        let using = self.github_pat_saved;
        let typed = token_field_filled(&input.read(cx).text().to_string());
        let heading = div()
            .w_full()
            .min_w_0()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3a3a3))
                    .child("Personal access token"),
            )
            .child(
                div().min_w_0().text_xs().text_color(rgb(0x737373)).child(if using {
                    "Syncing with a personal token. Remove it to fall back to the GitHub sign-in."
                } else {
                    "Optional, and the quicker way in when your repos belong to an organization: a token you create works on them immediately, while the GitHub sign-in has to be approved by an org owner first. The token is revocable on GitHub at any time."
                }),
            );
        let steps = div()
            .w_full()
            .min_w_0()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x737373))
                    .child("Create one on GitHub:"),
            )
            .child(pat_step(
                1,
                "Open your token settings on GitHub.".to_string(),
                &[],
            ))
            .child(pat_step(
                2,
                "Click \"Generate new token\" → \"Generate new token (classic)\", give it a name (e.g. \"todo-lofi\"), and tick the repo scope."
                    .to_string(),
                &[(
                    "github-pat-new-classic",
                    "New classic token",
                    GITHUB_NEW_CLASSIC_TOKEN_URL,
                )],
            ))
            .child(pat_step(
                3,
                "Copy the token GitHub shows you — it is shown only once.".to_string(),
                &[],
            ))
            .child(pat_step(
                4,
                "Paste it below. It then authenticates every request instead of the GitHub sign-in."
                    .to_string(),
                &[],
            ));
        div()
            .w_full()
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x2e2e2e))
            .bg(rgb(0x1e1e1e))
            .px_3()
            .py_2()
            .v_flex()
            .gap_2()
            .child(heading)
            .when(!using, |this| this.child(steps))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .v_flex()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0x737373)).child(if using {
                        "Replace the stored token"
                    } else {
                        "Paste your token here"
                    }))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&input).appearance(false)),
                            )
                            .when(using, |this| {
                                this.child(
                                    Button::new("github-pat-remove")
                                        .flex_none()
                                        .ghost()
                                        .compact()
                                        .label("Remove")
                                        .tooltip("Stop using the personal token")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.remove_github_pat(cx);
                                        })),
                                )
                            })
                            .child(
                                Button::new("github-pat-save")
                                    .flex_none()
                                    .ghost()
                                    .compact()
                                    .label("Save")
                                    .disabled(!typed)
                                    .tooltip(if typed {
                                        "Save this token"
                                    } else {
                                        "Paste a token first"
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_github_pat(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// Whether the token field holds something worth saving. Whitespace alone is
/// nothing: the field is trimmed before the token is stored, so a blank field
/// would be a save of an empty token.
fn token_field_filled(text: &str) -> bool {
    !text.trim().is_empty()
}

/// One numbered step in the personal-token instructions. Each action renders
/// as a button travelling to the page the step is about, so a step that needs
/// GitHub is one click from the card.
fn pat_step(
    index: usize,
    text: String,
    actions: &[(&'static str, &'static str, &'static str)],
) -> gpui::AnyElement {
    let mut row = div()
        .id(SharedString::from(format!("pat-step-{index}")))
        .debug_selector(move || format!("pat-step-{index}"))
        .w_full()
        .min_w_0()
        .h_flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .flex_none()
                .pt_0p5()
                .text_xs()
                .text_color(rgb(0x737373))
                .child(format!("{index}.")),
        )
        .child(
            div()
                .id(SharedString::from(format!("pat-step-text-{index}")))
                .debug_selector(move || format!("pat-step-text-{index}"))
                .flex_1()
                .min_w_0()
                .pt_0p5()
                .text_xs()
                .text_color(rgb(0x737373))
                .child(text),
        );
    for (id, label, url) in actions {
        let (id, label, url) = (*id, *label, *url);
        row = row.child(
            Button::new(id)
                .flex_none()
                .ghost()
                .compact()
                .with_size(Size::Small)
                .label(label)
                .tooltip(format!("Open {url}"))
                .on_click(move |_, _, _| todoist_auth::open_browser(url)),
        );
    }
    row.into_any_element()
}

/// Brand logo for Todoist (vendored SVG): the shared asset bundle only
/// ships the kit's default icon list, so brand art renders from bytes,
/// like the navbar's integrations icon.
fn todoist_icon() -> gpui_component::Icon {
    gpui_component::Icon::default()
        .data(include_bytes!("../../assets/icons/todoist.svg"))
        .with_size(Size::Large)
}

/// Whether GitHub is what backs a project: the tag is resolved and checked
/// against every repo this integration syncs with — a detected remote or an
/// explicit sync target alike (`bound_repos` covers both). Both halves are
/// reads, so a local project opens without asking GitHub anything.
async fn project_is_github_backed(
    store: &Store,
    integration_id: u64,
    tag_name: String,
    cx: &impl gpui::AppContext,
) -> bool {
    let tag = match store.get_tag_by_name(tag_name, cx).await {
        Ok(Some(tag)) => tag,
        Ok(None) => return false,
        Err(error) => {
            tracing::error!("Failed to resolve the opened project: {error}");
            return false;
        }
    };
    match store.tag_bound_repos(tag.id, integration_id, cx).await {
        Ok(repos) => !repos.is_empty(),
        Err(error) => {
            tracing::error!("Failed to read the opened project's GitHub repos: {error}");
            false
        }
    }
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
///
/// Shared with the notifications pane, which ages its entries the same way.
pub(crate) fn since_label(then: jiff::Timestamp, now: jiff::Timestamp) -> String {
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
            .min_w_0()
            .h_full()
            .overflow_y_scrollbar()
            // Same surface as the task list view (APP_BG), not the darker
            // navbar tone.
            .bg(rgb(APP_BG))
            .child(
                // `min_w_0` is what lets the column shrink to the pane: with
                // the automatic minimum a flex item refuses to go below its
                // longest unbroken line, so an instruction line would widen
                // the card — and every parent — instead of wrapping.
                div()
                    .w_full()
                    .flex_1()
                    .min_w_0()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().text_xl().font_semibold().child("Integrations"))
                    .child(self.todoist_card(window, cx))
                    .child(self.github_card(window, cx))
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_sm().text_color(rgb(0xa3a3a3)).child(status))
                    }),
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

    /// A step line wider than the width it is given wraps onto more lines
    /// rather than widening its row — the row stays exactly as wide as the
    /// container, which is what keeps the sub-card, and every parent above it,
    /// inside the pane. The host is a row so the step is a flex item on the
    /// main axis, where the automatic minimum size is what used to let a long
    /// line push the layout wider than its pane.
    #[gpui::test]
    fn the_token_steps_wrap_within_their_width(cx: &mut gpui::TestAppContext) {
        const WIDTH: f32 = 400.;
        const PADDING: f32 = 16.;
        struct Host;
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                // A pane-shaped row holding a stretched column, the way the
                // step reaches its width under the card.
                div().w(px(WIDTH)).h_flex().child(
                    div()
                        .id("test-column")
                        .debug_selector(|| "test-column".to_string())
                        .flex_1()
                        .min_w_0()
                        .p(px(PADDING))
                        .v_flex()
                        .child(pat_step(
                            1,
                            "Click \"Generate new token\" → \"Generate new token (classic)\", give it a name, and tick the repo scope."
                                .to_string(),
                            &[(
                                "test-step-button",
                                "New classic token",
                                GITHUB_NEW_CLASSIC_TOKEN_URL,
                            )],
                        )),
                )
            }
        }
        cx.update(gpui_component::init);
        let (_view, cx) = cx.add_window_view(|_, _| Host);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let row = cx
            .debug_bounds("pat-step-1")
            .expect("the step row measured");
        let line = cx
            .debug_bounds("pat-step-text-1")
            .expect("the step text measured");
        // Without a zero minimum the line takes its whole unwrapped length and
        // runs past the row (and the card, and the pane) on a narrow window.
        assert!(
            line.right() <= row.right(),
            "the step text ({line:?}) overflows its row ({row:?})"
        );
        assert!(
            line.size.height > px(20.),
            "a line too long for one row should wrap, height was {}",
            line.size.height
        );
    }

    /// The token field saves only something: an empty — or whitespace-only —
    /// field leaves the Save button disabled, so pressing it can never store a
    /// blank token.
    #[test]
    fn a_blank_token_field_leaves_nothing_to_save() {
        assert!(!token_field_filled(""));
        assert!(!token_field_filled("   "));
        assert!(!token_field_filled("\n\t "));
        assert!(token_field_filled("ghp_abc"));
        assert!(token_field_filled("  github_pat_abc  "));
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

    /// The decision for one project name, driven to completion: the store's
    /// reads run on Tokio while the answer comes back on a GPUI task, so the
    /// test pumps the executor in real time until it lands.
    fn backed(
        cx: &mut gpui::TestAppContext,
        store: &Store,
        integration_id: u64,
        tag_name: &str,
    ) -> bool {
        use std::cell::Cell;
        use std::rc::Rc;

        cx.executor().allow_parking();
        let recorded: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
        let recorder = recorded.clone();
        let store = store.clone();
        let tag_name = tag_name.to_string();
        cx.spawn(move |cx: gpui::AsyncApp| async move {
            let decision = project_is_github_backed(&store, integration_id, tag_name, &cx).await;
            recorder.set(Some(decision));
        })
        .detach();
        for _ in 0..400 {
            if let Some(decision) = recorded.get() {
                return decision;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            cx.run_until_parked();
        }
        panic!("the project lookup did not finish");
    }

    /// Opening a project only asks GitHub for one a repo backs: the tag has
    /// to resolve and carry a binding. A local project costs the same two
    /// reads and no network, and an unknown name is not a project at all.
    #[gpui::test]
    fn only_a_github_backed_project_is_fetched(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test's Tokio runtime");
        let handle = runtime.handle().clone();
        cx.update(|cx| gpui_tokio::init_from_handle(cx, handle.clone()));
        let (store, integration_id) = handle.block_on(async {
            let config = storage::StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = storage::TodoStore::new(&config)
                .await
                .expect("the in-memory store");
            let bound = store.create_tag("bound").await.expect("the synced tag");
            store.create_tag("local").await.expect("the local tag");
            let integration = store
                .create_integration("github", Some("octocat".to_string()))
                .await
                .expect("the integration");
            store
                .bind_repo_tag(bound.id, integration.id, "octocat", "hello-world")
                .await
                .expect("the repo binding");
            (Store::new(store), integration.id)
        });

        assert!(backed(cx, &store, integration_id, "bound"));
        assert!(!backed(cx, &store, integration_id, "local"));
        assert!(!backed(cx, &store, integration_id, "missing"));
    }
}

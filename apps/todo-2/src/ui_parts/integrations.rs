//! Integrations dialog: lists connectable providers (Todoist today).
//! Clicking Connect runs the provider's OAuth flow, then records the
//! connection as an `integrations` row so syncs can scope links per
//! integration.

use gpui::{
    AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Window, div, px, rgb,
    prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Sizable, Size, StyledExt};

use crate::github_auth;
use crate::store::Store;
use crate::theme::APP_BG;
use crate::todoist_auth;
use crate::ui_parts::apps::AppSettings;
use crate::ui_parts::todoist_sync::{TodoistSyncEvent, TodoistSyncPicker};

pub enum IntegrationsEvent {
    Changed,
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
    github_connecting: bool,
    _load: Option<gpui::Task<()>>,
    _connect: Option<gpui::Task<()>>,
    _sync: Option<gpui::Task<()>>,
    _github_poll: Option<gpui::Task<()>>,
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
            github_connecting: false,
            _load: None,
            _connect: None,
            _sync: None,
            _github_poll: None,
        };
        this.reload(cx);
        this
    }

    /// Re-read the connections and the app behind them (used when ownership
    /// changed elsewhere).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
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
                this.update(cx, |this, cx| {
                    this.connected = list;
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
                    this.update(cx, |this, cx| {
                        this.github_connecting = false;
                        this.status = Some(format!("GitHub connect failed: {e}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let polled = login.clone();
            this.update(cx, |this, cx| {
                let store = this.store.clone();
                // Polling and the account lookup are network work: both stay on
                // the Tokio runtime, only the result comes back to GPUI.
                let poll = gpui_tokio::Tokio::spawn_result(cx, async move {
                    let token = github_auth::complete(polled).await?;
                    Ok::<_, anyhow::Error>(github_auth::account_login(&token).await.ok())
                });
                this.github_code = Some(login);
                this.status = Some("Waiting for GitHub approval…".to_string());
                this._github_poll = Some(cx.spawn(async move |this, cx| {
                    let account = match poll.await {
                        Ok(account) => account,
                        Err(e) => {
                            this.update(cx, |this, cx| {
                                this.github_connecting = false;
                                this.github_code = None;
                                this.status = Some(format!("GitHub connect failed: {e}"));
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    };
                    let created = store
                        .create_integration("github".to_string(), account, cx)
                        .await;
                    this.update(cx, |this, cx| {
                        this.github_connecting = false;
                        this.github_code = None;
                        match created {
                            Ok(_) => {
                                this.status = Some("GitHub connected.".to_string());
                                cx.emit(IntegrationsEvent::Changed);
                            }
                            Err(e) => {
                                this.status = Some(format!("GitHub connect failed: {e}"))
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
        let gear = app_id
            .map(|app_id| {
                self.settings
                    .update(cx, |settings, cx| settings.gear_button(app_id, "Todoist", cx))
            });
        let settings_block = app_id.map(|app_id| {
            self.settings
                .update(cx, |settings, cx| settings.settings_block(app_id, 0, cx))
        });

        let actions: gpui::AnyElement = if connected {
            div()
                .h_flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("todoist-sync")
                        .ghost()
                        .compact()
                        .label(if self.syncing { "Syncing…" } else { "Sync now" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sync_now(cx);
                        })),
                )
                .child(
                    Button::new("todoist-disconnect")
                        .ghost()
                        .compact()
                        .label("Disconnect")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.disconnect_todoist(cx);
                        })),
                )
                .into_any_element()
        } else {
            Button::new("todoist-connect")
                .ghost()
                .compact()
                .label(if self.connecting { "Waiting…" } else { "Connect" })
                .on_click(cx.listener(|this, _, _, cx| {
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
        if let Some(gear) = gear {
            controls = controls.child(gear);
        }

        div()
            .v_flex()
            .gap_2()
            .child(
                integration_card(false)
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(provider_icon(todoist_icon()))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .gap_0p5()
                            .child(div().font_semibold().child("Todoist"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("Sync projects both ways with Todoist."),
                            ),
                    )
                    .child(controls),
            )
            .when(connected, |this| {
                this.child(self.todoist_sync_block(window, cx))
            })
            .when(expanded, |this| {
                this.when_some(settings_block, |this, block| this.child(block))
            })
            .into_any_element()
    }

    /// Project ↔ tag pairings for Todoist: the same picker used in
    /// settings, so both menus stay in sync.
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
                picker.render_picker("integrations-todoist", window, cx)
            }))
            .into_any_element()
    }
}

impl IntegrationsView {
    /// The GitHub card: device-flow connect (the code, then the poll) and
    /// disconnect once connected. The token lives only in
    /// `~/.config/my-todo/github.json`; the database holds the connection row.
    fn github_card(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let account = self
            .connected
            .iter()
            .find(|i| i.provider == "github")
            .and_then(|i| i.account_label.clone());
        let connected = account.is_some();

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
                        .ghost()
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
        div()
            .v_flex()
            .gap_2()
            .child(
                integration_card(false)
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(provider_icon(gpui_component_assets::IconName::Github))
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
                                    .text_color(rgb(0xa3a3a3))
                                    .child("Issue-backed tasks get branches, worktrees and pull requests."),
                            ),
                    )
                    .child(controls),
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
                                .text_lg()
                                .font_semibold()
                                .child(code.user_code.clone()),
                        )
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
}

/// Brand logo for Todoist (vendored SVG): the shared asset bundle only
/// ships the kit's default icon list, so brand art renders from bytes,
/// like the navbar's integrations icon.
fn todoist_icon() -> gpui_component::Icon {
    gpui_component::Icon::default()
        .data(include_bytes!("../../assets/icons/todoist.svg"))
        .with_size(Size::Large)
}

/// Subtle card shell for one integration row. `dimmed` grays the whole
/// card out for integrations that are not available yet.
fn integration_card(dimmed: bool) -> gpui::Div {
    div()
        .rounded_lg()
        .border_1()
        .border_color(rgb(0x2e2e2e))
        .bg(rgb(0x232323))
        .p_4()
        .when(dimmed, |this| this.opacity(0.55))
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
                    .child(self.github_card(cx))
                    .when_some(self.status.clone(), |this, status| {
                        this.child(
                            div().text_sm().text_color(rgb(0xa3a3a3)).child(status),
                        )
                    })
            )
    }
}
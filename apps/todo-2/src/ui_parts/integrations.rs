//! Integrations dialog: lists connectable providers (Todoist today).
//! Clicking Connect runs the provider's OAuth flow, then records the
//! connection as an `integrations` row so syncs can scope links per
//! integration.

use gpui::{
    Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Window, div, rgb,
    prelude::FluentBuilder,
};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};

use crate::store::Store;
use crate::todoist_auth::{self, OAuthConfig};

pub enum IntegrationsEvent {
    Changed,
}

pub struct IntegrationsView {
    store: Store,
    connected: Vec<storage::Integration>,
    connecting: bool,
    status: Option<String>,
    _load: Option<gpui::Task<()>>,
    _connect: Option<gpui::Task<()>>,
}

impl IntegrationsView {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            store,
            connected: Vec::new(),
            connecting: false,
            status: None,
            _load: None,
            _connect: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let fetch = self.store.list_integrations(cx);
        self._load = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(list) => {
                this.update(cx, |this, cx| {
                    this.connected = list;
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

    fn start_todoist_connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        self.connecting = true;
        self.status = Some("Waiting in the browser to authorize Todoist…".to_string());
        cx.notify();

        let store = self.store.clone();
        self._connect = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let config = OAuthConfig::load()?;
                let _token = todoist_auth::connect(&config).await?;
                // Token storage (keychain vs config) is still open per the
                // spec; record the connection so links can scope to it.
                let created = store
                    .create_integration("todoist".to_string(), None, cx)
                    .await?;
                Ok::<_, anyhow::Error>(created)
            }
            .await;
            this.update(cx, |this, cx| {
                this.connecting = false;
                match result {
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

impl Render for IntegrationsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.todoist_connected();
        div()
            .flex_1()
            .h_full()
            .bg(rgb(0x1e1e1e))
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().text_xl().font_semibold().child("Integrations"))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                    .child(
                        div()
                            .v_flex()
                            .gap_0p5()
                            .child(div().font_semibold().child("Todoist"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("Sync projects both ways with Todoist."),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if connected {
                                        rgb(0x4ade80)
                                    } else {
                                        rgb(0x737373)
                                    })
                                    .child(if connected { "Connected" } else { "Not connected" }),
                            )
                            .child(if connected {
                                Button::new("todoist-disconnect")
                                    .ghost()
                                    .compact()
                                    .label("Disconnect")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.disconnect_todoist(cx);
                                    }))
                            } else {
                                Button::new("todoist-connect")
                                    .ghost()
                                    .compact()
                                    .label(if self.connecting {
                                        "Waiting…"
                                    } else {
                                        "Connect"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.start_todoist_connect(cx);
                                    }))
                            }),
                    )
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(
                            div().text_sm().text_color(rgb(0xa3a3a3)).child(status),
                        )
                    })
            )
    }
}
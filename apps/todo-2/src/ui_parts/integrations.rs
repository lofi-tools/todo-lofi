//! Integrations dialog: lists connectable providers (Todoist today).
//! Clicking Connect runs the provider's OAuth flow, then records the
//! connection as an `integrations` row so syncs can scope links per
//! integration.

use gpui::{
    Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Window, div, px, rgb,
    prelude::FluentBuilder,
};
use gpui_component::{Sizable, Size, StyledExt};
use gpui_component::button::{Button, ButtonVariants};

use crate::store::Store;
use crate::todoist_auth;

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

/// Small pill badge, e.g. "Coming soon".
fn badge(label: &'static str) -> gpui::Div {
    div()
        .rounded_full()
        .bg(rgb(0x2a2a2a))
        .border_1()
        .border_color(rgb(0x3a3a3a))
        .px_2()
        .py_0p5()
        .text_xs()
        .text_color(rgb(0xa3a3a3))
        .child(label)
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
                        integration_card(false)
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .child(provider_icon(todoist_icon()))
                            .child(
                                div().v_flex().flex_1().gap_0p5().child(
                                    div().font_semibold().child("Todoist"),
                                ).child(
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
                    .child(
                        integration_card(true)
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .child(provider_icon(
                                gpui_component_assets::IconName::Github,
                            ))
                            .child(
                                div().v_flex().flex_1().gap_0p5().child(
                                    div()
                                        .h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(div().font_semibold().child("GitHub"))
                                        .child(badge("Coming soon")),
                                ).child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0xa3a3a3))
                                        .child("Turn issues and PRs into tasks."),
                                ),
                            ),
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(
                            div().text_sm().text_color(rgb(0xa3a3a3)).child(status),
                        )
                    })
            )
    }
}
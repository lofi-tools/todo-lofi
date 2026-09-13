//! Todoist project ↔ local tag pairing ("Synced tags").
//!
//! The old picker attached the integration's app to tags, which never showed
//! the actual sync mapping. Todoist syncs per remote project through
//! `external_tag_links`, so this picker shows one row per pairing
//! (`remote project ⇄ #local-tag`) with an unpair action, plus a two-step
//! add flow: pick a remote project first, then pair it with a local tag
//! through a fuzzy input. Pairing links and immediately syncs so the tag
//! populates without waiting for a manual sync.

use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, Size, StyledExt};

use crate::store::Store;
use crate::theme::{CARD_BG, HAIRLINE, TEXT_FAINT, TEXT_MUTED};
use crate::ui_parts::apps::rank_tag;

#[derive(Clone)]
pub enum TodoistSyncEvent {
    Changed,
}

/// One pairing row: `(remote project id, remote name or fallback, tag id,
/// tag label)`.
struct Pair {
    external_id: String,
    remote_name: String,
    tag_id: u64,
    tag_label: String,
}

pub struct TodoistSyncPicker {
    store: Store,
    integration_id: Option<u64>,
    pairs: Vec<Pair>,
    /// Live remote projects `(id, name)`; empty with an error note when the
    /// network or token fails.
    remote: Vec<(String, String)>,
    remote_error: Option<String>,
    local_tags: Vec<(u64, String)>,
    /// Step two of the add flow: the remote project being paired.
    selected_remote: Option<(String, String)>,
    remote_filter: Entity<InputState>,
    local_filter: Entity<InputState>,
    _remote_sub: Subscription,
    _local_sub: Subscription,
    status: Option<String>,
    _task: Option<Task<()>>,
}

impl TodoistSyncPicker {
    pub fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let remote_filter = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Filter Todoist projects…", window, cx);
            state
        });
        let local_filter = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Type to find a local tag…", window, cx);
            state
        });
        let _remote_sub = cx.subscribe(&remote_filter, |this, _, event, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.pick_highlighted_remote(cx),
            _ => {}
        });
        let _local_sub = cx.subscribe(&local_filter, |this, _, event, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.pair_highlighted_local(cx),
            _ => {}
        });
        let mut picker = Self {
            store,
            integration_id: None,
            pairs: Vec::new(),
            remote: Vec::new(),
            remote_error: None,
            local_tags: Vec::new(),
            selected_remote: None,
            remote_filter,
            local_filter,
            _remote_sub,
            _local_sub,
            status: None,
            _task: None,
        };
        picker.refresh(cx);
        picker
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let integration = store.todoist_integration(cx).await.unwrap_or(None);
            let integration_id = integration.map(|integration| integration.id);
            let (pairs, locals) = match integration_id {
                Some(id) => {
                    let links = store.integration_tag_pairs(id, cx).await.unwrap_or_default();
                    let tags = store.list_tags(cx).await.unwrap_or_default();
                    let pairs = links
                        .into_iter()
                        .map(|(external_id, _, tag_id, tag_label)| {
                            (external_id, tag_id, tag_label)
                        })
                        .collect::<Vec<_>>();
                    let locals = tags.iter().map(|tag| (tag.id, tag.label())).collect();
                    (pairs, locals)
                }
                None => (Vec::new(), Vec::new()),
            };
            // Network on the Tokio runtime; a failure leaves existing pairs
            // visible with an explanatory note.
            let remote = store.todoist_remote_projects(cx).await;
            this.update(cx, |this, cx| {
                this.integration_id = integration_id;
                this.local_tags = locals;
                match remote {
                    Ok(projects) => {
                        this.remote_error = None;
                        let names: std::collections::HashMap<&String, &String> = projects
                            .iter()
                            .map(|(id, name)| (id, name))
                            .collect();
                        this.pairs = pairs
                            .into_iter()
                            .map(|(external_id, tag_id, tag_label)| {
                                let remote_name = names
                                    .get(&external_id)
                                    .map(|name| (*name).clone())
                                    .unwrap_or_else(|| external_id.clone());
                                Pair {
                                    external_id,
                                    remote_name,
                                    tag_id,
                                    tag_label,
                                }
                            })
                            .collect();
                        this.remote = projects;
                    }
                    Err(error) => {
                        this.remote_error =
                            Some(format!("Could not load Todoist projects: {error}"));
                        this.pairs = pairs
                            .into_iter()
                            .map(|(external_id, tag_id, tag_label)| Pair {
                                remote_name: external_id.clone(),
                                external_id,
                                tag_id,
                                tag_label,
                            })
                            .collect();
                    }
                }
                if let Some((id, _)) = &this.selected_remote
                    && this.pairs.iter().any(|pair| &pair.external_id == id)
                {
                    this.selected_remote = None;
                }
                this._task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn remote_query(&self, cx: &gpui::App) -> String {
        self.remote_filter.read(cx).text().to_string()
    }

    fn local_query(&self, cx: &gpui::App) -> String {
        self.local_filter.read(cx).text().to_string()
    }

    /// Remote projects without a pairing yet, filtered by the remote input.
    fn unpaired_remotes(&self, cx: &gpui::App) -> Vec<(String, String)> {
        let query = self.remote_query(cx);
        let mut ranked: Vec<(usize, String, String)> = self
            .remote
            .iter()
            .filter(|(id, _)| !self.pairs.iter().any(|pair| &pair.external_id == id))
            .filter_map(|(id, name)| rank_tag(&query, name).map(|rank| (rank, id.clone(), name.clone())))
            .collect();
        ranked.sort();
        ranked
            .into_iter()
            .take(8)
            .map(|(_, id, name)| (id, name))
            .collect()
    }

    /// Local tags filtered by the local input.
    fn local_suggestions(&self, cx: &gpui::App) -> Vec<(u64, String)> {
        let query = self
            .local_query(cx)
            .trim()
            .trim_start_matches('#')
            .to_string();
        let mut ranked: Vec<(usize, u64, String)> = self
            .local_tags
            .iter()
            .filter_map(|(id, label)| rank_tag(&query, label).map(|rank| (rank, *id, label.clone())))
            .collect();
        ranked.sort();
        ranked
            .into_iter()
            .take(8)
            .map(|(_, id, label)| (id, label))
            .collect()
    }

    fn pick_highlighted_remote(&mut self, cx: &mut Context<Self>) {
        if let Some(remote) = self.unpaired_remotes(cx).into_iter().next() {
            self.selected_remote = Some(remote);
            cx.notify();
        }
    }

    fn pair_highlighted_local(&mut self, cx: &mut Context<Self>) {
        if let Some((tag_id, _)) = self.local_suggestions(cx).into_iter().next() {
            self.pair_with(tag_id, cx);
        }
    }

    /// Link the selected remote project to `tag_id`, then sync immediately
    /// so the tag populates.
    fn pair_with(&mut self, tag_id: u64, cx: &mut Context<Self>) {
        let (Some(integration_id), Some((external_id, _))) =
            (self.integration_id, self.selected_remote.clone())
        else {
            return;
        };
        let link = self.store.link_tag(
            integration_id,
            external_id,
            tag_id,
            "project".to_string(),
            false,
            cx,
        );
        let store = self.store.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = link.await {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("Failed: {error}"));
                    cx.notify();
                })
                .ok();
                return;
            }
            let summary = store.sync_todoist(cx).await;
            this.update(cx, |this, cx| {
                match summary {
                    Ok(summary) => {
                        this.status = Some(format!(
                            "Paired and synced: {} task(s), {} section(s).",
                            summary.tasks_upserted, summary.sections
                        ));
                    }
                    Err(error) => this.status = Some(format!("Paired, but sync failed: {error}")),
                }
                this.selected_remote = None;
                cx.emit(TodoistSyncEvent::Changed);
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn unpair(&mut self, external_id: String, cx: &mut Context<Self>) {
        let Some(integration_id) = self.integration_id else {
            return;
        };
        let unlink = self
            .store
            .unlink_tag(integration_id, external_id, cx);
        self._task = Some(cx.spawn(async move |this, cx| {
            let outcome = unlink.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => this.status = Some("Unpaired; local tasks are kept.".to_string()),
                    Err(error) => this.status = Some(format!("Failed: {error}")),
                }
                cx.emit(TodoistSyncEvent::Changed);
                this.refresh(cx);
            })
            .ok();
        }));
    }

    /// Full block: current pairs, then the two-step add flow.
    pub fn render_picker(
        &mut self,
        id_prefix: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.integration_id.is_none() {
            return div()
                .text_sm()
                .text_color(rgb(TEXT_MUTED))
                .child("Connect Todoist first to pair projects.")
                .into_any_element();
        }
        let pair_rows: Vec<AnyElement> = self
            .pairs
            .iter()
            .map(|pair| {
                let external_id = pair.external_id.clone();
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .min_h(px(28.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(rgb(0xd4d4d4))
                            .child(format!("{} ⇄ #{}", pair.remote_name, pair.tag_label)),
                    )
                    .child(
                        Button::new(format!("{id_prefix}-unpair-{}", pair.external_id))
                            .ghost()
                            .compact()
                            .with_size(Size::Small)
                            .label("Unpair")
                            .tooltip("Stop syncing this project; local tasks are kept")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unpair(external_id.clone(), cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let mut block = div().v_flex().gap_2();
        if pair_rows.is_empty() {
            block = block.child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT_MUTED))
                    .child("No projects paired yet."),
            );
        } else {
            block = block.child(div().v_flex().gap_1().children(pair_rows));
        }
        block = block.child(self.add_flow(id_prefix, cx));
        if let Some(error) = self.remote_error.clone() {
            block = block.child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(error));
        }
        if let Some(status) = self.status.clone() {
            block = block.child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(status));
        }
        block.into_any_element()
    }

    /// Step one: pick a remote project. Step two: pair it with a local tag.
    fn add_flow(&mut self, id_prefix: &str, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_remote.clone();
        match selected {
            None => {
                let rows: Vec<AnyElement> = self
                    .unpaired_remotes(cx)
                    .into_iter()
                    .map(|(id, name)| {
                        div()
                            .id(SharedString::from(format!("{id_prefix}-remote-{id}")))
                            .w_full()
                            .h_flex()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .text_color(rgb(0xd4d4d4))
                            .hover(|this| this.bg(rgb(0x2a2a2a)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let name = this
                                    .remote
                                    .iter()
                                    .find(|(remote_id, _)| remote_id == &id)
                                    .map(|(_, name)| name.clone())
                                    .unwrap_or_else(|| id.clone());
                                this.selected_remote = Some((id.clone(), name));
                                cx.notify();
                            }))
                            .child(format!("Pair “{name}”…"))
                            .into_any_element()
                    })
                    .collect();
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child("1 · Pick a Todoist project"),
                    )
                    .child(Input::new(&self.remote_filter).with_size(Size::Small))
                    .when(!rows.is_empty(), |this| {
                        this.child(
                            div()
                                .v_flex()
                                .gap_0p5()
                                .max_h(px(200.))
                                .overflow_y_scrollbar()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(HAIRLINE))
                                .bg(rgb(CARD_BG))
                                .p_1()
                                .children(rows),
                        )
                    })
                    .into_any_element()
            }
            Some((_, remote_name)) => {
                let rows: Vec<AnyElement> = self
                    .local_suggestions(cx)
                    .into_iter()
                    .map(|(tag_id, label)| {
                        div()
                            .id(SharedString::from(format!("{id_prefix}-local-{tag_id}")))
                            .w_full()
                            .h_flex()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .text_color(rgb(0xd4d4d4))
                            .hover(|this| this.bg(rgb(0x2a2a2a)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pair_with(tag_id, cx);
                            }))
                            .child(format!("#{label}"))
                            .into_any_element()
                    })
                    .collect();
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(format!("2 · Pair “{remote_name}” with")),
                            )
                            .child(
                                Button::new(format!("{id_prefix}-cancel-pair"))
                                    .ghost()
                                    .compact()
                                    .with_size(Size::Small)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_remote = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(Input::new(&self.local_filter).with_size(Size::Small))
                    .when(!rows.is_empty(), |this| {
                        this.child(
                            div()
                                .v_flex()
                                .gap_0p5()
                                .max_h(px(200.))
                                .overflow_y_scrollbar()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(HAIRLINE))
                                .bg(rgb(CARD_BG))
                                .p_1()
                                .children(rows),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

impl EventEmitter<TodoistSyncEvent> for TodoistSyncPicker {}

impl Render for TodoistSyncPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

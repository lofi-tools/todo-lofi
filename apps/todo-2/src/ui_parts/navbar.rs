use gpui::{
    Animation, AnimationExt, Context, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::IconName;
use gpui_component::scroll::ScrollableElement;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use storage::prelude::*;

use crate::store::Store;
use crate::theme::{PANEL_BG, PANEL_HOVER, SUCCESS, TEXT_FAINT, TEXT_MUTED};

#[derive(Clone)]
pub enum NavBarEvent {
    TagSelected(Vec<String>),
    AllTasks,
    /// The + button was clicked; the parent should open the project picker.
    OpenProjectPicker,
    /// The footer "Integrations" row was clicked.
    OpenIntegrations,
    /// The footer "Automations" row was clicked.
    OpenAutomations,
    /// The footer "Workflows" row was clicked.
    OpenWorkflows,
    /// The footer "Settings" row was clicked.
    OpenSettings,
}

/// Which main panel is shown next to the navbar. The navbar highlights
/// the matching footer row.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum NavPanel {
    #[default]
    Tasks,
    Integrations,
    Automations,
    Workflows,
    Settings,
}

pub struct NavBar {
    store: Store,
    top_level_tags: Vec<Tag>,
    children_cache: HashMap<u64, Vec<Tag>>,
    selected_path: Vec<String>,
    active_panel: NavPanel,
    /// Provider per synced tag (`tag_id → provider`), for the corner badge
    /// on synced tag icons. Unlinked tags are absent.
    linked_providers: HashMap<u64, String>,
    /// Project tags with a streaming agent turn, keyed by tag name. Each
    /// row shows a pulsing dot while its project is busy.
    busy_tags: HashSet<String>,
    _fetch_tags: Option<Task<()>>,
    _fetch_children: Option<Task<()>>,
}

impl NavBar {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let fetch_store = store.clone();
        let tags_task = fetch_store.list_top_level_tags(cx);
        let providers_task = fetch_store.tag_link_providers(cx);

        let _fetch_tags = Some(cx.spawn(async move |this, cx| {
            let tags = match tags_task.await {
                Ok(tags) => tags,
                Err(e) => {
                    tracing::error!("Failed to fetch tags: {e}");
                    return;
                }
            };
            let providers = providers_task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.top_level_tags = tags;
                this.linked_providers = providers;
                this._fetch_tags = None;
                cx.notify();
            })
            .ok();
        }));

        Self {
            store,
            top_level_tags: Vec::new(),
            children_cache: HashMap::new(),
            selected_path: Vec::new(),
            active_panel: NavPanel::Tasks,
            linked_providers: HashMap::new(),
            busy_tags: HashSet::new(),
            _fetch_tags,
            _fetch_children: None,
        }
    }

    /// Mark a project tag as running a turn (or not), so its row shows the
    /// busy dot. `tag_name` is the tag's unique name, not its label.
    pub fn set_agent_busy(&mut self, tag_name: &str, busy: bool, cx: &mut Context<Self>) {
        let changed = if busy {
            self.busy_tags.insert(tag_name.to_string())
        } else {
            self.busy_tags.remove(tag_name)
        };
        if changed {
            cx.notify();
        }
    }

    /// Whether `tag_name` is a project tag with a turn currently running.
    pub fn is_agent_busy(&self, tag_name: &str) -> bool {
        self.busy_tags.contains(tag_name)
    }

    /// Highlight the footer row matching the visible main panel.
    pub fn set_panel(&mut self, panel: NavPanel, cx: &mut Context<Self>) {
        if self.active_panel != panel {
            self.active_panel = panel;
            cx.notify();
        }
    }

    /// The currently selected tag path (used to drop stale async
    /// navigation results).
    pub fn selected_path(&self) -> &[String] {
        &self.selected_path
    }

    /// Re-fetch the tag tree after a new tag was created elsewhere (e.g. the
    /// project picker modal).
    pub fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        let tags_task = self.store.list_top_level_tags(cx);
        let providers_task = self.store.tag_link_providers(cx);
        self._fetch_tags = Some(cx.spawn(async move |this, cx| {
            let tags = match tags_task.await {
                Ok(tags) => tags,
                Err(e) => {
                    tracing::error!("Failed to refresh tags: {e}");
                    return;
                }
            };
            let providers = providers_task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.top_level_tags = tags;
                this.linked_providers = providers;
                this.children_cache.clear();
                this._fetch_tags = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn navigate_to_tag(
        &mut self,
        _tag_name: &str,
        _tag_id: u64,
        path: &[String],
        cx: &mut Context<Self>,
    ) {
        if self.selected_path == path {
            return;
        }
        self.selected_path = path.to_vec();
        cx.emit(NavBarEvent::TagSelected(path.to_vec()));

        let mut current_children = self.top_level_tags.clone();
        for name in &self.selected_path {
            if let Some(tag) = current_children.iter().find(|t| t.name == *name) {
                if !self.children_cache.contains_key(&tag.id) {
                    let store = self.store.clone();
                    let id = tag.id;
                    let fetch_task = store.get_children(id, cx);
                    self._fetch_children =
                        Some(cx.spawn(async move |this, cx| match fetch_task.await {
                            Ok(children) => {
                                this.update(cx, |this, cx| {
                                    this.children_cache.insert(id, children);
                                    this._fetch_children = None;
                                    cx.notify();
                                })
                                .ok();
                            }
                            Err(e) => {
                                tracing::error!("Failed to fetch children: {e}");
                            }
                        }));
                    break;
                }
                current_children = self
                    .children_cache
                    .get(&tag.id)
                    .cloned()
                    .unwrap_or_default();
            }
        }
        cx.notify();
    }

    fn collect_visible_tags(&self) -> Vec<VisibleTag> {
        let mut result = Vec::new();

        fn walk(
            tag: &Tag,
            depth: usize,
            ancestors: &[String],
            selected_path: &[String],
            children_cache: &HashMap<u64, Vec<Tag>>,
            busy_tags: &HashSet<String>,
            result: &mut Vec<VisibleTag>,
        ) {
            let children = children_cache.get(&tag.id).cloned().unwrap_or_default();
            let has_children = !children.is_empty();
            let mut path = ancestors.to_vec();
            path.push(tag.name.clone());
            result.push(VisibleTag {
                name: tag.name.clone(),
                label: tag_label(tag),
                id: tag.id,
                depth,
                has_children,
                path: path.clone(),
                is_project: tag.is_project(),
                busy: busy_tags.contains(&tag.name),
            });

            let is_on_path = selected_path.iter().any(|p| p == &tag.name);
            if is_on_path {
                for child in &children {
                    walk(
                        child,
                        depth + 1,
                        &path,
                        selected_path,
                        children_cache,
                        busy_tags,
                        result,
                    );
                }
            }
        }

        for tag in &self.top_level_tags {
            walk(
                tag,
                0,
                &[],
                &self.selected_path,
                &self.children_cache,
                &self.busy_tags,
                &mut result,
            );
        }

        result
    }
}

/// One rendered navbar row: the tag's identity plus the decorations the row
/// needs (depth indent, project icon, busy dot).
struct VisibleTag {
    name: String,
    label: String,
    id: u64,
    depth: usize,
    has_children: bool,
    path: Vec<String>,
    is_project: bool,
    busy: bool,
}

/// A small pulsing dot shown on a project row while its agent turn streams.
/// The pulse is never the only signal — clicking the row also opens the pane.
fn busy_dot() -> gpui::AnyElement {
    div()
        .w(px(6.))
        .h(px(6.))
        .flex_none()
        .rounded_full()
        .bg(rgb(SUCCESS))
        .with_animation(
            "agent-busy-dot",
            Animation::new(Duration::from_millis(1100)).repeat(),
            |dot, phase| dot.opacity(0.3 + 0.7 * phase),
        )
        .into_any_element()
}

/// The user-facing label of a tag: the display name when set (project
/// tags), otherwise the plain name.
fn tag_label(tag: &Tag) -> String {
    tag.label()
}

impl EventEmitter<NavBarEvent> for NavBar {}

impl Render for NavBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible_tags = self.collect_visible_tags();
        let selected_tag = self.selected_path.last().cloned();
        let is_all_tasks = self.selected_path.is_empty();

        div()
            .h_full()
            .bg(rgb(0x1e1e1e))
            .border_r_1()
            .border_color(rgb(0x333333))
            .p_4()
            .v_flex()
            .gap_0p5()
            .child(
                div()
                    .flex_1()
                    // Deep tag trees can exceed the viewport height, so make
                    // the nav scrollable while the footer stays pinned.
                    .overflow_y_scrollbar()
                    .v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .justify_between()
                            .mb_2()
                            .mt_4()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("Tags"),
                            )
                            .child(
                                Button::new("add-project-tag")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::Plus)
                                    .tooltip("Tag a local project")
                                    .on_click(cx.listener(|_this, _, _, cx| {
                                        cx.emit(NavBarEvent::OpenProjectPicker);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("all-tasks")
                            .child("All Tasks")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(if is_all_tasks {
                                rgb(0x2a2a2a)
                            } else {
                                rgb(0x1e1e1e)
                            })
                            .hover(|s| s.bg(rgb(0x2a2a2a)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected_path.clear();
                                cx.emit(NavBarEvent::AllTasks);
                                cx.notify();
                            })),
                    )
                    .children(visible_tags.into_iter().map(
                        |VisibleTag {
                             name: tag_name,
                             label: tag_label,
                             id: tag_id,
                             depth,
                             has_children: _has_children,
                             path,
                             is_project,
                             busy,
                         }| {
                            let tag_for_click = tag_name.clone();
                            let path_for_click = path;
                            let is_selected = selected_tag.as_deref() == Some(&tag_name);

                            let prefix: gpui::AnyElement = if is_project {
                                div()
                                    .w(px(16.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(IconName::Folder)
                                    .into_any_element()
                            } else {
                                let badge = self
                                    .linked_providers
                                    .get(&tag_id)
                                    .map(|provider| provider.as_str());
                                div()
                                    .w(px(16.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(
                                        div()
                                            .relative()
                                            .child("#")
                                            .when_some(badge, |this, provider| {
                                                this.child(sync_badge(provider))
                                            }),
                                    )
                                    .into_any_element()
                            };

                            div()
                                .h_flex()
                                .items_center()
                                .ml(px(depth as f32 * 8.0))
                                .child(
                                    div()
                                        .id(("tag", tag_id))
                                        .flex_1()
                                        .h_flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(prefix)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .child(tag_label),
                                        )
                                        .when(busy, |this| this.child(busy_dot()))
                                        .px_2()
                                        .py_0p5()
                                        .rounded_md()
                                        .bg(if is_selected {
                                            rgb(0x2a2a2a)
                                        } else {
                                            rgb(0x1e1e1e)
                                        })
                                        .hover(|s| s.bg(rgb(0x2a2a2a)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.navigate_to_tag(
                                                &tag_for_click,
                                                tag_id,
                                                &path_for_click,
                                                cx,
                                            );
                                        })),
                                )
                        },
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(rgb(0x333333))
                    .pt_2()
                    .v_flex()
                    .gap_0p5()
                    .child(nav_footer_row(
                        "nav-automations",
                        gpui_component_assets::IconName::Bot,
                        "Automations",
                        self.active_panel == NavPanel::Automations,
                        NavBarEvent::OpenAutomations,
                        cx,
                    ))
                    .child(nav_footer_row(
                        "nav-integrations",
                        integrations_icon(),
                        "Integrations",
                        self.active_panel == NavPanel::Integrations,
                        NavBarEvent::OpenIntegrations,
                        cx,
                    ))
                    .child(nav_footer_row(
                        "nav-workflows",
                        gpui_component_assets::IconName::Play,
                        "Workflows",
                        self.active_panel == NavPanel::Workflows,
                        NavBarEvent::OpenWorkflows,
                        cx,
                    ))
                    .child(nav_footer_row(
                        "nav-settings",
                        gpui_component_assets::IconName::Settings,
                        "Settings",
                        self.active_panel == NavPanel::Settings,
                        NavBarEvent::OpenSettings,
                        cx,
                    )),
            )
    }
}

/// The integrations icon (Lucide `blocks`): three blocks piled up with a
/// fourth one being added.
///
/// Rendered from vendored SVG bytes instead of the shared asset bundle:
/// the app bundle only ships the icons in the kit's `default-icons.txt`
/// list, which does not include `blocks`, so a bundled path would resolve
/// to nothing and render blank.
fn integrations_icon() -> gpui_component::Icon {
    gpui_component::Icon::default().data(include_bytes!("../../assets/icons/blocks.svg"))
}

/// Tiny provider badge overlaid on the bottom-right corner of a tag's `#`
/// icon. Unknown providers get no badge.
fn sync_badge(provider: &str) -> gpui::AnyElement {
    let icon: gpui::AnyElement = match provider {
        "todoist" => gpui_component::Icon::default()
            .data(include_bytes!("../../assets/icons/todoist.svg"))
            .size(px(7.))
            .into_any_element(),
        _ => return div().into_any_element(),
    };
    div()
        .absolute()
        .bottom(px(0.))
        .right(px(-4.))
        .rounded_full()
        .bg(rgb(0x1e1e1e))
        .p(px(2.))
        .child(icon)
        .into_any_element()
}

/// A footer row pinned at the bottom of the navbar (Integrations,
/// Settings): icon + label, same hover treatment as the tag rows.
fn nav_footer_row(
    id: &'static str,
    icon: impl IntoElement,
    label: &'static str,
    active: bool,
    event: NavBarEvent,
    cx: &mut Context<NavBar>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded_md()
        .text_color(rgb(TEXT_MUTED))
        .bg(if active { rgb(PANEL_HOVER) } else { rgb(PANEL_BG) })
        .hover(|s| s.bg(rgb(PANEL_HOVER)))
        .child(
            div()
                .w(px(16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon),
        )
        .child(label)
        .on_click(cx.listener(move |_this, _, _, cx| {
            cx.emit(event.clone());
        }))
}

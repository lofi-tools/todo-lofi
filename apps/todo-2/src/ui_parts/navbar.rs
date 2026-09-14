use gpui::{
    Animation, AnimationExt, Context, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::IconName;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
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
    /// A tag row's context menu asked for its settings; the parent shows the
    /// tag settings popover for that tag.
    OpenTagSettings(String),
    /// The footer "Integrations" row was clicked.
    OpenIntegrations,
    /// The footer "Automations" row was clicked.
    OpenAutomations,
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
    Settings,
}

pub struct NavBar {
    store: Store,
    /// Every tag row, already ordered and indented by the store. Rendering
    /// folds it to the selected path — top-level rows plus the one branch the
    /// selected tag sits in — so the nav stays collapsed until a tag is
    /// picked, and no per-tag expand state is kept.
    rows: Vec<TagTreeRow>,
    selected_path: Vec<String>,
    active_panel: NavPanel,
    /// Provider per synced tag (`tag_id → provider`), for the corner badge
    /// on synced tag icons. Unlinked tags are absent.
    linked_providers: HashMap<u64, String>,
    /// Project tags with a streaming agent turn, keyed by tag name. Each
    /// row shows a pulsing dot while its project is busy.
    busy_tags: HashSet<String>,
    _fetch_tags: Option<Task<()>>,
}

impl NavBar {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let fetch_store = store.clone();
        let tags_task = fetch_store.tag_tree_rows(cx);
        let providers_task = fetch_store.tag_link_providers(cx);

        let _fetch_tags = Some(cx.spawn(async move |this, cx| {
            let rows = match tags_task.await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::error!("Failed to fetch tags: {e}");
                    return;
                }
            };
            let providers = providers_task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.rows = rows;
                this.linked_providers = providers;
                this._fetch_tags = None;
                cx.notify();
            })
            .ok();
        }));

        Self {
            store,
            rows: Vec::new(),
            selected_path: Vec::new(),
            active_panel: NavPanel::Tasks,
            linked_providers: HashMap::new(),
            busy_tags: HashSet::new(),
            _fetch_tags,
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

    /// The currently selected tag path.
    pub fn selected_path(&self) -> &[String] {
        &self.selected_path
    }

    /// Re-fetch the tag tree after a tag, placement, or section changed
    /// elsewhere (the tag settings popover, the project picker).
    pub fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        let tags_task = self.store.tag_tree_rows(cx);
        let providers_task = self.store.tag_link_providers(cx);
        self._fetch_tags = Some(cx.spawn(async move |this, cx| {
            let rows = match tags_task.await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::error!("Failed to refresh tags: {e}");
                    return;
                }
            };
            let providers = providers_task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.rows = rows;
                this.linked_providers = providers;
                this._fetch_tags = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Select a tag by the path it was reached through. The path is what
    /// disambiguates a project that is placed under several parents. The
    /// selection also decides which branch of the tree render shows expanded.
    fn navigate_to_tag(&mut self, path: &[String], cx: &mut Context<Self>) {
        if self.selected_path == path {
            return;
        }
        self.selected_path = path.to_vec();
        cx.emit(NavBarEvent::TagSelected(path.to_vec()));
        cx.notify();
    }
}

/// Whether `row` should render under the current selection. Top-level rows
/// are always visible; a nested row is shown only when every ancestor along
/// its path is on the selected path, so exactly the branch of the selected
/// tag is expanded and everything else stays collapsed.
fn row_visible(path: &[String], selected_path: &[String]) -> bool {
    path.iter()
        .take(path.len().saturating_sub(1))
        .all(|ancestor| selected_path.contains(ancestor))
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

impl EventEmitter<NavBarEvent> for NavBar {}

impl Render for NavBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_all_tasks = self.selected_path.is_empty();
        // Cloned so the row closures do not borrow `self` while `cx.listener`
        // borrows `cx`.
        let rows = self.rows.clone();
        let selected_path = self.selected_path.clone();
        let linked_providers = self.linked_providers.clone();
        let busy_tags = self.busy_tags.clone();
        let nav = cx.weak_entity();

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
                    // The expanded branch can still exceed the viewport
                    // height; scroll it while the footer stays pinned.
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
                    .children(rows.into_iter().filter_map(|row| {
                        row_visible(&row.path, &selected_path).then_some(row)
                    }).map(|row| {
                        let TagTreeRow {
                            tag,
                            depth,
                            path,
                            is_directory_backed,
                            is_section: _is_section,
                        } = row;
                        let tag_id = tag.id;
                        let tag_name = tag.name.clone();
                        let label = tag.label();
                        let is_selected = path == selected_path;
                        let busy = busy_tags.contains(&tag_name);
                        // A duplicated row (the same project under several
                        // parents) must not reuse an element id, so the path
                        // identifies the row rather than the tag.
                        let row_id = format!("tag-row:{}", path.join("/"));

                        let prefix: gpui::AnyElement = if is_directory_backed {
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
                            let badge = linked_providers.get(&tag_id).map(|provider| provider.as_str());
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

                        let click_path = path.clone();
                        let menu_tag_name = tag_name.clone();
                        let menu_nav = nav.clone();
                        div()
                            .h_flex()
                            .items_center()
                            .ml(px(depth as f32 * 8.0))
                            .child(
                                div()
                                    .id(row_id)
                                    .flex_1()
                                    .h_flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(prefix)
                                    .child(div().flex_1().min_w_0().truncate().child(label))
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
                                        this.navigate_to_tag(&click_path, cx);
                                    }))
                                    .context_menu(move |menu, _window, _cx| {
                                        let nav = menu_nav.clone();
                                        let tag_name = menu_tag_name.clone();
                                        menu.item(PopupMenuItem::new("Tag settings…").on_click(
                                            move |_, _window, cx| {
                                                nav.update(cx, |_this, cx| {
                                                    cx.emit(NavBarEvent::OpenTagSettings(
                                                        tag_name.clone(),
                                                    ));
                                                })
                                                .ok();
                                            },
                                        ))
                                    }),
                            )
                    })),
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

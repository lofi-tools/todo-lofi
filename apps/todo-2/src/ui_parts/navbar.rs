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
    /// The navbar moved to another destination: a tag row click, a footer
    /// row click, or Escape stepping back. The layout shows the matching
    /// panel and the task list follows when the destination is a tag.
    Navigated(NavDestination),
    /// The + button was clicked; the parent should open the project picker.
    OpenProjectPicker,
    /// A tag row's context menu asked for its settings; the parent shows the
    /// tag settings popover for that tag.
    OpenTagSettings(String),
}

/// One place the main panel can be pointed at: a task view (all tasks or one
/// tag) or one of the menu panels. Exactly one destination is current at a
/// time, which keeps the navbar's highlight unambiguous and makes a menu a
/// normal navigation step rather than a mode stacked on top of the tag pane.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum NavDestination {
    #[default]
    AllTasks,
    /// A tag, by the path it was reached through: the path disambiguates a
    /// project that is placed under several parents.
    Tag(Vec<String>),
    Integrations,
    Automations,
    Settings,
}

impl NavDestination {
    /// Whether this destination shows the task list (with its optional
    /// details/agent panes).
    pub fn is_tasks(&self) -> bool {
        matches!(self, Self::AllTasks | Self::Tag(_))
    }
}

/// The navigation stack: where the app is pointed, plus the destinations
/// visited before it. Held in state so every navigation path — a tag row, a
/// footer row, or Escape — moves through the same stack, and `current` is the
/// single source of truth for both the highlighted row and the panel on
/// screen.
#[derive(Clone, Debug, Default)]
pub struct NavHistory {
    current: NavDestination,
    back: Vec<NavDestination>,
}

impl NavHistory {
    pub fn current(&self) -> &NavDestination {
        &self.current
    }

    /// Point at `destination`, remembering where we came from so back returns
    /// here. False when that is already the destination, which is what makes
    /// re-clicking the row of the destination on screen a no-op.
    pub fn navigate(&mut self, destination: NavDestination) -> bool {
        if self.current == destination {
            return false;
        }
        self.back.push(std::mem::replace(&mut self.current, destination));
        true
    }

    /// Step back one destination. False when there is nothing to step back
    /// to; `current` is where the app ended up.
    pub fn go_back(&mut self) -> bool {
        match self.back.pop() {
            Some(previous) => {
                self.current = previous;
                true
            }
            None => false,
        }
    }

    /// The tag branch the navbar keeps expanded: the current tag, or the last
    /// tag visited before a menu. Keeping the branch open is what lets a
    /// nested tag still be clicked while a menu panel is showing, so it can be
    /// navigated back to.
    pub fn expanded_path(&self) -> &[String] {
        if let NavDestination::Tag(path) = &self.current {
            return path;
        }
        self.back
            .iter()
            .rev()
            .find_map(|destination| match destination {
                NavDestination::Tag(path) => Some(path.as_slice()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

pub struct NavBar {
    store: Store,
    /// Every tag row, already ordered and indented by the store. Rendering
    /// folds it to the expanded path — top-level rows plus the one branch the
    /// visited tag sits in — so the nav stays collapsed until a tag is picked,
    /// and no per-tag expand state is kept.
    rows: Vec<TagTreeRow>,
    /// Where the app is pointed and how it got there.
    nav: NavHistory,
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
            nav: NavHistory::default(),
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

    /// The destination the main panel should be showing. This is the one
    /// place navigation state is read from: the highlighted row and the panel
    /// can never disagree.
    pub fn destination(&self) -> &NavDestination {
        self.nav.current()
    }

    /// Navigate to `destination` (a tag row, a footer row, or a programmatic
    /// jump). Emits `Navigated` only when it actually moved, so re-clicking
    /// the row of the destination on screen is a no-op.
    pub fn navigate_to(&mut self, destination: NavDestination, cx: &mut Context<Self>) {
        if self.nav.navigate(destination.clone()) {
            cx.emit(NavBarEvent::Navigated(destination));
            cx.notify();
        }
    }

    /// Step back one navigation step (Escape from a menu). False when there is
    /// nothing to step back to.
    pub fn navigate_back(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.nav.go_back() {
            return false;
        }
        let destination = self.nav.current().clone();
        cx.emit(NavBarEvent::Navigated(destination));
        cx.notify();
        true
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
}

/// Whether `row` should render under the expanded path. Top-level rows are
/// always visible; a nested row is shown only when every ancestor along its
/// path is on the expanded path, so exactly the branch of the visited tag is
/// expanded and everything else stays collapsed.
fn row_visible(path: &[String], expanded_path: &[String]) -> bool {
    path.iter()
        .take(path.len().saturating_sub(1))
        .all(|ancestor| expanded_path.contains(ancestor))
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
        // Cloned so the row closures do not borrow `self` while `cx.listener`
        // borrows `cx`.
        let destination = self.nav.current().clone();
        // The branch to keep open is the visited tag even while a menu panel
        // is showing, so the row that leads back to it stays clickable.
        let expanded_path = self.nav.expanded_path().to_vec();
        let is_all_tasks = destination == NavDestination::AllTasks;
        let rows = self.rows.clone();
        let linked_providers = self.linked_providers.clone();
        let busy_tags = self.busy_tags.clone();
        let nav = cx.weak_entity();

        div()
            .h_full()
            .bg(rgb(0x1e1e1e))
            .border_r_1()
            .border_color(rgb(0x333333))
            .px_4()
            .pt_4()
            .pb_2()
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
                                this.navigate_to(NavDestination::AllTasks, cx);
                            })),
                    )
                    .children(rows.into_iter().filter_map(|row| {
                        row_visible(&row.path, &expanded_path).then_some(row)
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
                        let is_selected = destination == NavDestination::Tag(path.clone());
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
                                        this.navigate_to(
                                            NavDestination::Tag(click_path.clone()),
                                            cx,
                                        );
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
.pt_3()
                    .v_flex()
                    .gap_0p5()
                    .child(nav_footer_row(
                        "nav-automations",
                        gpui_component_assets::IconName::Bot,
                        "Automations",
                        destination == NavDestination::Automations,
                        NavDestination::Automations,
                        cx,
                    ))
                    .child(nav_footer_row(
                        "nav-integrations",
                        integrations_icon(),
                        "Integrations",
                        destination == NavDestination::Integrations,
                        NavDestination::Integrations,
                        cx,
                    ))
                    .child(nav_footer_row(
                        "nav-settings",
                        gpui_component_assets::IconName::Settings,
                        "Settings",
                        destination == NavDestination::Settings,
                        NavDestination::Settings,
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

/// A footer row pinned at the bottom of the navbar (Automations,
/// Integrations, Settings): icon + label, same hover treatment as the tag
/// rows, and the same navigation step a tag row makes.
fn nav_footer_row(
    id: &'static str,
    icon: impl IntoElement,
    label: &'static str,
    active: bool,
    destination: NavDestination,
    cx: &mut Context<NavBar>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap_1p5()
        .px_2()
        // Equal top and bottom padding per row; the rows sit inside the
        // navbar's own `p_4`, so both edges get the same breathing room.
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
        .on_click(cx.listener(move |this, _, _, cx| {
            this.navigate_to(destination.clone(), cx);
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str) -> NavDestination {
        NavDestination::Tag(vec![name.to_string()])
    }

    #[test]
    fn a_menu_step_leaves_the_visited_tag_clickable() {
        let mut nav = NavHistory::default();
        assert!(nav.navigate(tag("todo-lofi")));
        assert!(nav.navigate(NavDestination::Integrations));

        // The branch of the tag visited before the menu stays expanded, so
        // the very row that leads back to it is still on screen.
        assert_eq!(nav.expanded_path(), ["todo-lofi".to_string()]);

        // And re-clicking it navigates back to the tag: the whole point of
        // tracking the destination rather than the panel.
        assert!(nav.navigate(tag("todo-lofi")));
        assert_eq!(nav.current(), &tag("todo-lofi"));
        assert_eq!(nav.expanded_path(), ["todo-lofi".to_string()]);
    }

    #[test]
    fn re_clicking_the_current_destination_changes_nothing() {
        let mut nav = NavHistory::default();
        assert!(nav.navigate(NavDestination::Settings));
        assert!(!nav.navigate(NavDestination::Settings));
        assert!(nav.navigate(tag("dev")));
        assert!(!nav.navigate(tag("dev")));
        assert!(nav.navigate(tag("other")));
    }

    #[test]
    fn back_steps_through_every_destination_visited() {
        let mut nav = NavHistory::default();
        nav.navigate(tag("dev"));
        nav.navigate(NavDestination::Integrations);
        nav.navigate(NavDestination::Settings);

        assert!(nav.go_back());
        assert_eq!(nav.current(), &NavDestination::Integrations);
        assert!(nav.go_back());
        assert_eq!(nav.current(), &tag("dev"));
        assert!(nav.go_back());
        assert_eq!(nav.current(), &NavDestination::AllTasks);
        assert!(!nav.go_back());
    }
}

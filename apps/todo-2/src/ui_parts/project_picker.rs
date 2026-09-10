//! Modal for adding a tag: three tabs sharing one dialog.
//!
//! - "Tag": a raw tag name typed into a text input.
//! - "Project": a local git-repo directory picked from the fuzzy list.
//! - "Todoist": a Todoist project picked from the same fuzzy list UI,
//!   shown only while the Todoist integration is connected.

use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
    div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, MoveDown as InputMoveDown};
use gpui_component::input::{MoveUp as InputMoveUp};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, StyledExt};

use crate::projects::Project;
use crate::store::Store;
use crate::todoist_auth::{self, TodoistProject};

const CONTEXT: &str = "ProjectPicker";

/// A no-op action dispatched by Enter in the picker context. Enter inside a
/// filter input raises `InputEvent::PressEnter` (handled via subscription);
/// this binding swallows Enter pressed while focus is elsewhere in the
/// picker so it doesn't bubble to the dialog's own Enter handling.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = project_picker, no_json)]
pub struct SelectFocused;

/// Dismiss action dispatched by Esc in the picker context.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = project_picker, no_json)]
pub struct Cancel;

/// Register the picker's key bindings. Called once at startup, before the
/// first picker is created. Bindings live in the `ProjectPicker` context so
/// they only match while the picker modal has focus. `up`/`down` reuse the
/// input's own MoveUp/MoveDown actions (bound to arrow keys already by
/// gpui-component) so the arrows work both in the filter input (cursor
/// moves) and on the list (handled below) depending on focus; Enter raises
/// the input's `PressEnter` event, which the parent handles.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
        KeyBinding::new("enter", SelectFocused, Some(CONTEXT)),
    ]);
}

#[derive(Clone, Debug)]
pub enum ProjectPickerEvent {
    /// The user picked a local directory project (Enter or click).
    Selected(Project),
    /// The user submitted a raw tag name (Enter or Create button).
    TagName(String),
    /// The user picked a Todoist project (Enter or click).
    TodoistProject { id: String, name: String },
    /// The user dismissed the modal (Esc).
    Dismissed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerTab {
    Tag,
    Directory,
    Todoist,
}

impl PickerTab {
    fn label(self) -> &'static str {
        match self {
            PickerTab::Tag => "Tag",
            PickerTab::Directory => "Project",
            PickerTab::Todoist => "Todoist",
        }
    }
}

enum TodoistState {
    /// Integration not connected: the tab is hidden entirely.
    Hidden,
    Loading,
    Loaded,
    Failed(String),
}

pub struct ProjectPicker {
    store: Store,
    tab: PickerTab,
    todoist_state: TodoistState,
    projects: Vec<Project>,
    /// Indices into `projects` matching the current filter, in rank order.
    visible: Vec<usize>,
    todoist_projects: Vec<TodoistProject>,
    /// Indices into `todoist_projects` matching the current filter.
    todoist_visible: Vec<usize>,
    /// Shared fuzzy-filter input for the two list tabs.
    query: Entity<InputState>,
    /// Raw tag name input for the Tag tab.
    tag_input: Entity<InputState>,
    cursor: usize,
    focus_handle: FocusHandle,
    _filter_subscription: gpui::Subscription,
    _tag_subscription: gpui::Subscription,
    _load: Option<gpui::Task<()>>,
}

impl ProjectPicker {
    pub fn new(
        projects: Vec<Project>,
        store: Store,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let visible = (0..projects.len()).collect();
        let query = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Filter projects...", window, cx);
            state
        });
        let filter_subscription = cx.subscribe(&query, |this, _, event, cx| {
            match event {
                InputEvent::Change => this.apply_filter(cx),
                // Enter inside the filter field confirms the focused row,
                // matching the keyboard contract of the picker itself.
                InputEvent::PressEnter { .. } => this.select_current(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            }
        });
        let tag_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Tag name...", window, cx);
            state
        });
        let tag_subscription = cx.subscribe(&tag_input, |this, _, event, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.submit_tag(cx),
                InputEvent::Change | InputEvent::Focus | InputEvent::Blur => {}
            }
        });
        let mut this = Self {
            store,
            tab: PickerTab::Directory,
            todoist_state: TodoistState::Hidden,
            projects,
            visible,
            todoist_projects: Vec::new(),
            todoist_visible: Vec::new(),
            query,
            tag_input,
            cursor: 0,
            focus_handle: cx.focus_handle(),
            _filter_subscription: filter_subscription,
            _tag_subscription: tag_subscription,
            _load: None,
        };
        this.check_todoist(cx);
        this
    }

    /// Ask the store whether Todoist is connected; if so, reveal the tab
    /// and fetch its projects on the Tokio runtime (network must never run
    /// on GPUI's executor).
    fn check_todoist(&mut self, cx: &mut Context<Self>) {
        let integrations = self.store.list_integrations(cx);
        let fetch = gpui_tokio::Tokio::spawn_result(cx, async move {
            let connected = integrations
                .await?
                .into_iter()
                .any(|i| i.provider == "todoist");
            if !connected {
                return Ok::<_, anyhow::Error>(None);
            }
            Ok(Some(todoist_auth::list_projects().await?))
        });
        self.todoist_state = TodoistState::Loading;
        self._load = Some(cx.spawn(async move |this, cx| match fetch.await {
            Ok(Some(projects)) => {
                this.update(cx, |this, cx| {
                    this.todoist_projects = projects;
                    this.todoist_visible = (0..this.todoist_projects.len()).collect();
                    this.todoist_state = TodoistState::Loaded;
                    this._load = None;
                    cx.notify();
                })
                .ok();
            }
            Ok(None) => {
                this.update(cx, |this, cx| {
                    this.todoist_state = TodoistState::Hidden;
                    this._load = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                this.update(cx, |this, cx| {
                    this.todoist_state =
                        TodoistState::Failed(format!("Could not load Todoist projects: {e}"));
                    this._load = None;
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn tabs(&self) -> Vec<PickerTab> {
        let mut tabs = vec![PickerTab::Tag, PickerTab::Directory];
        if !matches!(self.todoist_state, TodoistState::Hidden) {
            tabs.push(PickerTab::Todoist);
        }
        tabs
    }

    fn switch_tab(&mut self, tab: PickerTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.cursor = 0;
        self.apply_filter(cx);
        self.focus_active_input(window, cx);
    }

    /// Subsequence fuzzy match with a simple rank: consecutive prefix matches
    /// score best, plain subsequence matches after that. Good enough for
    /// narrowing a directory list by typing a few letters.
    fn rank(query: &str, name: &str) -> Option<usize> {
        if query.is_empty() {
            return Some(0);
        }
        let query = query.to_lowercase();
        let name_lower = name.to_lowercase();
        if let Some(prefix) = name_lower.strip_prefix(&query) {
            return Some(prefix.len());
        }
        // Plain subsequence check; rank by spread of matched chars.
        let mut search = name_lower.char_indices().peekable();
        let mut matched: Vec<usize> = Vec::new();
        for q in query.chars() {
            loop {
                match search.next() {
                    Some((index, c)) if c == q => {
                        matched.push(index);
                        break;
                    }
                    Some(_) => continue,
                    None => return None,
                }
            }
        }
        let spread = matched.last().copied().unwrap_or(0) - matched.first().copied().unwrap_or(0);
        Some(name_lower.len() + spread)
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        match self.tab {
            PickerTab::Tag => {}
            PickerTab::Directory => {
                let mut ranked: Vec<(usize, usize)> = self
                    .projects
                    .iter()
                    .enumerate()
                    .filter_map(|(index, project)| {
                        Self::rank(&query, &project.name).map(|score| (index, score))
                    })
                    .collect();
                ranked.sort_by_key(|(_, score)| *score);
                self.visible = ranked.into_iter().map(|(index, _)| index).collect();
            }
            PickerTab::Todoist => {
                let mut ranked: Vec<(usize, usize)> = self
                    .todoist_projects
                    .iter()
                    .enumerate()
                    .filter_map(|(index, project)| {
                        Self::rank(&query, &project.name).map(|score| (index, score))
                    })
                    .collect();
                ranked.sort_by_key(|(_, score)| *score);
                self.todoist_visible = ranked.into_iter().map(|(index, _)| index).collect();
            }
        }
        self.cursor = 0;
        cx.notify();
    }

    fn active_names(&self) -> Vec<String> {
        match self.tab {
            PickerTab::Tag => Vec::new(),
            PickerTab::Directory => self
                .visible
                .iter()
                .map(|&i| self.projects[i].name.clone())
                .collect(),
            PickerTab::Todoist => self
                .todoist_visible
                .iter()
                .map(|&i| self.todoist_projects[i].name.clone())
                .collect(),
        }
    }

    fn select_current(&mut self, cx: &mut Context<Self>) {
        match self.tab {
            PickerTab::Tag => self.submit_tag(cx),
            PickerTab::Directory => {
                if let Some(&index) = self.visible.get(self.cursor) {
                    let project = self.projects[index].clone();
                    cx.emit(ProjectPickerEvent::Selected(project));
                }
            }
            PickerTab::Todoist => {
                if let Some(&index) = self.todoist_visible.get(self.cursor) {
                    let project = self.todoist_projects[index].clone();
                    cx.emit(ProjectPickerEvent::TodoistProject {
                        id: project.id,
                        name: project.name,
                    });
                }
            }
        }
    }

    fn submit_tag(&mut self, cx: &mut Context<Self>) {
        let name = self.tag_input.read(cx).value().trim().to_string();
        if !name.is_empty() {
            cx.emit(ProjectPickerEvent::TagName(name));
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = match self.tab {
            PickerTab::Tag => return,
            PickerTab::Directory => self.visible.len(),
            PickerTab::Todoist => self.todoist_visible.len(),
        } as isize;
        if count == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).rem_euclid(count);
        self.cursor = next as usize;
        cx.notify();
    }

    /// Focus the fuzzy-filter input so typing filters immediately. The
    /// picker's own focus handle only carries the key context; the input is
    /// what should receive keystrokes.
    pub fn focus_filter(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active_input(window, cx);
    }

    fn focus_active_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.tab {
            PickerTab::Tag => &self.tag_input,
            PickerTab::Directory | PickerTab::Todoist => &self.query,
        };
        input.update(cx, |state, cx| state.focus(window, cx));
    }
}

impl Focusable for ProjectPicker {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ProjectPickerEvent> for ProjectPicker {}

impl Render for ProjectPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;

        let names = self.active_names();
        let cursor = self.cursor;
        let rows = names
            .into_iter()
            .enumerate()
            .map(|(row_index, name)| {
                let is_cursor = row_index == cursor;
                let id_kind = match self.tab {
                    PickerTab::Todoist => "todoist-option",
                    _ => "project-option",
                };
                div()
                    .id(gpui::ElementId::named_usize(id_kind, row_index))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(if is_cursor {
                        rgb(0x2a2a2a)
                    } else {
                        rgb(0x1e1e1e)
                    })
                    .hover(|s| s.bg(rgb(0x2a2a2a)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cursor = row_index;
                        this.select_current(cx);
                    }))
                    .child(name)
            })
            .collect::<Vec<_>>();
        let empty = rows.is_empty();

        let tabs = self.tabs();
        let active = self.tab;
        let tab_bar = div().h_flex().gap_1().children(tabs.into_iter().map(|tab| {
            Button::new(("picker-tab", tab as usize))
                .ghost()
                .compact()
                .label(tab.label())
                .toggled(active == tab)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switch_tab(tab, window, cx);
                }))
        }));

        let mut body = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_this, _: &Cancel, _, cx| {
                cx.stop_propagation();
                cx.emit(ProjectPickerEvent::Dismissed);
            }))
            .on_action(cx.listener(|this, _: &InputMoveUp, _, cx| {
                this.move_cursor(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &InputMoveDown, _, cx| {
                this.move_cursor(1, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFocused, _, cx| {
                this.select_current(cx);
            }))
            .flex()
            .flex_col()
            .gap_3()
            .child(tab_bar);

        match self.tab {
            PickerTab::Tag => {
                body = body
                    .child(Input::new(&self.tag_input).with_size(gpui_component::Size::Medium))
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .child(
                                Button::new("create-tag")
                                    .ghost()
                                    .compact()
                                    .label("Create tag")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.submit_tag(cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x666666))
                            .child("Enter create · Esc cancel"),
                    );
            }
            PickerTab::Directory | PickerTab::Todoist => {
                body = body
                    .child(Input::new(&self.query).with_size(gpui_component::Size::Medium))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            // The scrollable wrapper inherits the element's size
                            // (not max-height), so the list needs a fixed height to
                            // actually scroll instead of growing to fit its rows.
                            .h(px(320.))
                            .overflow_y_scrollbar()
                            .children(rows),
                    );
                if let PickerTab::Todoist = self.tab {
                    body = body.child(match &self.todoist_state {
                        TodoistState::Loading => div()
                            .text_sm()
                            .text_color(rgb(0xa3a3a3))
                            .child("Loading Todoist projects…"),
                        TodoistState::Failed(message) => div()
                            .text_sm()
                            .text_color(rgb(0xff6b6b))
                            .child(message.clone()),
                        TodoistState::Loaded | TodoistState::Hidden => div().text_xs()
                            .text_color(rgb(0x666666))
                            .child("↑/↓ navigate · Enter select · Esc cancel"),
                    });
                    if empty && matches!(self.todoist_state, TodoistState::Loaded) {
                        body = body.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xa3a3a3))
                                .child("No matching projects"),
                        );
                    }
                } else {
                    if empty {
                        body = body.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xa3a3a3))
                                .child("No matching projects"),
                        );
                    }
                    body = body.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x666666))
                            .child("↑/↓ navigate · Enter select · Esc cancel"),
                    );
                }
            }
        }
        body
    }
}

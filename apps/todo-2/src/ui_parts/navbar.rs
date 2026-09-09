use gpui::{
    Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Task, Window, div, px, rgb,
};
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::IconName;
use gpui_component::scroll::ScrollableElement;
use std::collections::HashMap;
use storage::prelude::*;

use crate::store::Store;

#[derive(Clone)]
pub enum NavBarEvent {
    TagSelected(Vec<String>),
    AllTasks,
    /// The + button was clicked; the parent should open the project picker.
    OpenProjectPicker,
}

pub struct NavBar {
    store: Store,
    top_level_tags: Vec<Tag>,
    children_cache: HashMap<u64, Vec<Tag>>,
    selected_path: Vec<String>,
    _fetch_tags: Option<Task<()>>,
    _fetch_children: Option<Task<()>>,
}

impl NavBar {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let fetch_store = store.clone();
        let fetch_task = fetch_store.list_top_level_tags(cx);

        let _fetch_tags = Some(cx.spawn(async move |this, cx| match fetch_task.await {
            Ok(tags) => {
                this.update(cx, |this, cx| {
                    this.top_level_tags = tags;
                    this._fetch_tags = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch tags: {e}");
            }
        }));

        Self {
            store,
            top_level_tags: Vec::new(),
            children_cache: HashMap::new(),
            selected_path: Vec::new(),
            _fetch_tags,
            _fetch_children: None,
        }
    }

    /// Re-fetch the tag tree after a new tag was created elsewhere (e.g. the
    /// project picker modal).
    pub fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        let fetch_task = self.store.list_top_level_tags(cx);
        self._fetch_tags = Some(cx.spawn(async move |this, cx| match fetch_task.await {
            Ok(tags) => {
                this.update(cx, |this, cx| {
                    this.top_level_tags = tags;
                    this.children_cache.clear();
                    this._fetch_tags = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to refresh tags: {e}");
            }
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

    fn collect_visible_tags(&self) -> Vec<(String, String, u64, usize, bool, Vec<String>, bool)> {
        let mut result = Vec::new();

        fn walk(
            tag: &Tag,
            depth: usize,
            ancestors: &[String],
            selected_path: &[String],
            children_cache: &HashMap<u64, Vec<Tag>>,
            result: &mut Vec<(String, String, u64, usize, bool, Vec<String>, bool)>,
        ) {
            let children = children_cache.get(&tag.id).cloned().unwrap_or_default();
            let has_children = !children.is_empty();
            let mut path = ancestors.to_vec();
            path.push(tag.name.clone());
            result.push((
                tag.name.clone(),
                tag_label(tag),
                tag.id,
                depth,
                has_children,
                path.clone(),
                tag.is_project(),
            ));

            let is_on_path = selected_path.iter().any(|p| p == &tag.name);
            if is_on_path {
                for child in &children {
                    walk(
                        child,
                        depth + 1,
                        &path,
                        selected_path,
                        children_cache,
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
                &mut result,
            );
        }

        result
    }
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
            .gap_2()
            // Deep tag trees can exceed the viewport height, so make the nav
            // scrollable.
            .overflow_y_scrollbar()
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
                |(tag_name, tag_label, tag_id, depth, _has_children, path, is_project)| {
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
                            .text_color(rgb(0x737373))
                            .child(IconName::Folder)
                            .into_any_element()
                    } else {
                        div()
                            .w(px(16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(0x737373))
                            .child("#")
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
                                .child(tag_label)
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
            ))
    }
}

use crate::task_store::TaskStore;
use gpui::{
    Context, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, Styled,
    Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme, StyledExt};

pub struct NavBar {
    pub task_store: Entity<TaskStore>,
    pub selected_tag: Entity<Option<String>>,
    pub selected_path: Entity<Vec<String>>,
}

fn collect_visible_tags(
    task_store: &TaskStore,
    selected_path: &[String],
) -> Vec<(String, usize, bool)> {
    let top_level = task_store.top_level_tags().unwrap_or_default();
    let mut result = Vec::new();

    fn walk(
        tag: &str,
        depth: usize,
        selected_path: &[String],
        task_store: &TaskStore,
        result: &mut Vec<(String, usize, bool)>,
    ) {
        let children = task_store.children_of(tag).unwrap_or_default();
        let has_children = !children.is_empty();
        result.push((tag.to_string(), depth, has_children));

        let is_on_path = selected_path.iter().any(|p| p == tag);
        if is_on_path {
            for child in &children {
                walk(child, depth + 1, selected_path, task_store, result);
            }
        }
    }

    for tag in &top_level {
        walk(tag, 0, selected_path, task_store, &mut result);
    }

    result
}

impl Render for NavBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_path = self.selected_path.read(cx).clone();
        let selected_tag = self.selected_tag.read(cx).clone();

        let visible_tags = {
            let task_store = self.task_store.read(cx);
            collect_visible_tags(task_store, &selected_path)
        };

        div()
            .w_64()
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .p_2()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .mb_1()
                    .child("Tags"),
            )
            .child(
                div()
                    .child("All Tasks")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                    .when(selected_tag.is_none(), |this| this.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_tag.update(cx, |tag, _| *tag = None);
                            this.selected_path.update(cx, |p, _| p.clear());
                            cx.notify();
                        }),
                    ),
            )
            .children(
                visible_tags
                    .into_iter()
                    .map(|(tag_name, depth, has_children)| {
                        let is_on_path = selected_path.contains(&tag_name);
                        let is_selected = Some(tag_name.clone()) == selected_tag;
                        let indent = depth;
                        let tag_for_select = tag_name.clone();

                        let disclosure = if has_children {
                            let indicator = if is_on_path { "▼" } else { "▶" };
                            div()
                                .w_5()
                                .flex_none()
                                .h_flex()
                                .justify_center()
                                .items_center()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(indicator)
                        } else {
                            div().w_5().flex_none()
                        };

                        div()
                            .h_flex()
                            .items_center()
                            .ml(px(indent as f32 * 8.0))
                            .child(disclosure)
                            .child(
                                div()
                                    .flex_1()
                                    .child(tag_name)
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                                    .when(is_selected, |this| this.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.selected_tag.update(cx, |tag, _| {
                                                *tag = Some(tag_for_select.clone())
                                            });
                                            let path = {
                                                let task_store = this.task_store.read(cx);
                                                task_store
                                                    .path_to(&tag_for_select)
                                                    .unwrap_or_default()
                                            };
                                            this.selected_path.update(cx, move |p, _| *p = path);
                                            cx.notify();
                                        }),
                                    ),
                            )
                    }),
            )
    }
}

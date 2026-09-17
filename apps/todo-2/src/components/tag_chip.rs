use gpui::{Div, ParentElement, Styled, div, px, rgb};
use gpui_component::{Sizable, StyledExt};

/// A tag chip, shared by the details pane's tag list (read-only and inside
/// the editor) and a task row's tag sub-row. A directory-backed tag (a
/// project) shows the folder icon in place of the hashtag, matching the
/// navbar's row prefix.
pub fn tag_chip(label: &str, is_project: bool) -> Div {
    let chip = div()
        .h_flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .text_size(px(10.))
        .px(px(4.))
        .py(px(1.))
        .rounded(px(2.))
        .bg(rgb(0x2a2a2a))
        .text_color(rgb(0xa3a3a3));
    if is_project {
        chip.child(
            gpui_component::Icon::new(gpui_component::IconName::Folder)
                .with_size(gpui_component::Size::XSmall),
        )
        .child(label.to_string())
    } else {
        chip.child(format!("#{label}"))
    }
}

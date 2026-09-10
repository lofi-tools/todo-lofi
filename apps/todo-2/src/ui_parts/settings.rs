//! Settings main panel: app-level preferences shown in place of the
//! task list when the navbar's Settings row is active.

use gpui::{div, rgb, Context, IntoElement, ParentElement, Render, Styled, Window};
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt;

pub struct SettingsView;

impl SettingsView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .h_full()
            .bg(rgb(0x1e1e1e))
            .overflow_y_scrollbar()
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(div().text_xl().font_semibold().child("Settings"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xa3a3a3))
                            .child("App preferences will live here."),
                    ),
            )
    }
}

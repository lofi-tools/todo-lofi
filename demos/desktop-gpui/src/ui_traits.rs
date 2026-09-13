use gpui::{App, Corners, Edges, ParentElement, Pixels, StyleRefinement, Styled, Window, div, px};
use gpui_component::{ActiveTheme, StyledExt};

pub mod icon {
    // use gpui::{AnyElement, App, Entity, SharedString};
    // use gpui_component::{Icon, IconName, icon_named};

    // /// Types implementing this trait can automatically be converted to [`Icon`].
    // ///
    // /// This allows you to implement a custom version of [`IconName`] that functions as a drop-in
    // /// replacement for other UI components.
    // pub trait IconNamed {
    //     /// Returns the embedded path of the icon.
    //     fn path(self) -> SharedString;
    // }

    // impl<T: IconNamed> From<T> for Icon {
    //     fn from(value: T) -> Self {
    //         Icon::build(value)
    //     }
    // }

    // // Generate `IconName` from the icons that `gpui-component-assets` ships.
    // // The `$VAR` form resolves to the absolute path published by the assets
    // // crate's `build.rs` (via cargo's `links` mechanism) and re-exported by
    // // our own `build.rs`. See `gpui_component_macros::icon_named!`'s doc
    // // comment for the full mechanism.
    // icon_named!(IconName, "$GPUI_COMPONENT_DEFAULT_ICONS_DIR");

    // impl IconName {
    //     /// Return the icon as a Entity<Icon>
    //     pub fn view(self, cx: &mut App) -> Entity<Icon> {
    //         Icon::build(self).view(cx)
    //     }
    // }

    // impl From<IconName> for AnyElement {
    //     fn from(val: IconName) -> Self {
    //         Icon::build(val).into_any_element()
    //     }
    // }

    // impl RenderOnce for IconName {
    //     fn render(self, _: &mut Window, _cx: &mut App) -> impl IntoElement {
    //         Icon::build(self)
    //     }
    // }
}

pub(crate) trait FocusableExt<T: ParentElement + Styled + Sized> {
    /// Add focus ring to the element.
    fn focus_ring(self, is_focused: bool, margins: Pixels, window: &Window, cx: &App) -> Self;
}

impl<T: ParentElement + Styled + Sized> FocusableExt<T> for T {
    fn focus_ring(mut self, is_focused: bool, margins: Pixels, window: &Window, cx: &App) -> Self {
        if !is_focused {
            return self;
        }

        const RING_BORDER_WIDTH: Pixels = px(1.5);
        let rem_size = window.rem_size();
        let style = self.style();

        let border_widths = Edges::<Pixels> {
            top: style
                .border_widths
                .top
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
            bottom: style
                .border_widths
                .bottom
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
            left: style
                .border_widths
                .left
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
            right: style
                .border_widths
                .right
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
        };

        // Update the radius based on element's corner radii and the ring border width.
        let radius = Corners::<Pixels> {
            top_left: style
                .corner_radii
                .top_left
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
            top_right: style
                .corner_radii
                .top_right
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
            bottom_left: style
                .corner_radii
                .bottom_left
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
            bottom_right: style
                .corner_radii
                .bottom_right
                .map(|v| v.to_pixels(rem_size))
                .unwrap_or_default(),
        }
        .map(|v| *v + RING_BORDER_WIDTH);

        let mut inner_style = StyleRefinement::default();
        inner_style.corner_radii.top_left = Some(radius.top_left.into());
        inner_style.corner_radii.top_right = Some(radius.top_right.into());
        inner_style.corner_radii.bottom_left = Some(radius.bottom_left.into());
        inner_style.corner_radii.bottom_right = Some(radius.bottom_right.into());

        let inset = RING_BORDER_WIDTH + margins;

        self.child(
            div()
                .flex_none()
                .absolute()
                .top(-(inset + border_widths.top))
                .left(-(inset + border_widths.left))
                .right(-(inset + border_widths.right))
                .bottom(-(inset + border_widths.bottom))
                .border(RING_BORDER_WIDTH)
                .border_color(cx.theme().ring.alpha(0.2))
                .refine_style(&inner_style),
        )
    }
}

use gpui::{
    div, prelude::FluentBuilder as _, relative, rgb, AnyElement, App, Div, ElementId,
    InteractiveElement, IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement,
    StyleRefinement, Styled, Window,
};
use gpui_component::{
    text::Text, v_flex, ActiveTheme, Disableable, Selectable, Sizable, Size, StyleSized,
    StyledExt as _,
};
use std::rc::Rc;

/// A Checkbox element.
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    base: Div,
    style: StyleRefinement,
    label: Option<Text>,
    children: Vec<AnyElement>,
    checked: bool,
    disabled: bool,
    size: Size,
    on_click: Option<OnClickHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            base: div(),
            style: StyleRefinement::default(),
            label: None,
            children: Vec::new(),
            checked: false,
            disabled: false,
            size: Size::default(),
            on_click: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    fn handle_click(
        on_click: &Option<OnClickHandler>,
        checked: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let new_checked = !checked;
        if let Some(f) = on_click {
            (f)(&new_checked, window, cx);
        }
    }
}

type OnClickHandler = Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

impl InteractiveElement for Checkbox {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}
impl StatefulInteractiveElement for Checkbox {}

impl Styled for Checkbox {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        &mut self.style
    }
}

impl Disableable for Checkbox {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Selectable for Checkbox {
    fn selected(self, selected: bool) -> Self {
        self.checked(selected)
    }

    fn is_selected(&self) -> bool {
        self.checked
    }
}

impl ParentElement for Checkbox {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Sizable for Checkbox {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

pub(crate) fn checkbox_check_icon(
    _id: ElementId,
    _size: Size,
    checked: bool,
    disabled: bool,
    _window: &mut Window,
    _cx: &mut App,
) -> impl IntoElement {
    let color = if disabled {
        gpui::black().opacity(0.5)
    } else {
        gpui::black()
    };

    div()
        .absolute()
        .top_0()
        .left_0()
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h_full()
        .text_color(color)
        .when(checked, |this| this.child("✓"))
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let checked = self.checked;

        div().child(
            self.base
                .id(self.id.clone())
                .h_flex()
                .gap_2()
                .items_start()
                .line_height(relative(1.))
                .text_color(cx.theme().foreground)
                .map(|this| match self.size {
                    Size::XSmall => this.text_xs(),
                    Size::Small => this.text_sm(),
                    Size::Medium => this.text_base(),
                    Size::Large => this.text_lg(),
                    _ => this,
                })
                .when(self.disabled, |this| {
                    this.text_color(cx.theme().muted_foreground)
                })
                .rounded(cx.theme().radius * 0.5)
                .refine_style(&self.style)
                .child(
                    div()
                        .relative()
                        .map(|svg| svg.size_with(self.size))
                        .flex_shrink_0()
                        .rounded_full()
                        .when(!checked, |this| this.border_1().border_color(rgb(0xffffff)))
                        .map(|this| match checked {
                            false => this.bg(rgb(0x1a1a1a)),
                            _ => this.bg(rgb(0x666666)),
                        })
                        .child(checkbox_check_icon(
                            self.id,
                            self.size,
                            checked,
                            self.disabled,
                            window,
                            cx,
                        )),
                )
                .when(self.label.is_some() || !self.children.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .flex_1()
                            .overflow_hidden()
                            .line_height(relative(1.2))
                            .gap_1()
                            .map(|this| {
                                if let Some(label) = self.label {
                                    this.child(
                                        div()
                                            .size_full()
                                            .text_color(cx.theme().foreground)
                                            .when(self.disabled, |this| {
                                                this.text_color(cx.theme().muted_foreground)
                                            })
                                            .line_height(relative(1.))
                                            .child(label),
                                    )
                                } else {
                                    this
                                }
                            })
                            .children(self.children),
                    )
                })
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default();
                })
                .when(!self.disabled, |this| {
                    this.on_click({
                        let on_click = self.on_click.clone();
                        move |_, window, cx| {
                            window.prevent_default();
                            Self::handle_click(&on_click, checked, window, cx);
                        }
                    })
                }),
        )
    }
}

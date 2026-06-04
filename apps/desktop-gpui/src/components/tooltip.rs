// use std::{cell::Cell, rc::Rc};

// use gpui::{
//     Action, AnyView, App, Bounds, Pixels, SharedString, StatefulInteractiveElement, Window,
// };
// use gpui_component::{
//     Root,
//     tooltip::{Tooltip, TooltipOverlay},
// };

// /// Shared tooltip state that components (Button, Switch, Checkbox, Radio, etc.)
// /// can embed to get `.tooltip()` support with minimal boilerplate.
// #[derive(Default)]
// pub(crate) struct ComponentTooltip {
//     pub text: Option<(
//         SharedString,
//         Option<(Rc<Box<dyn Action>>, Option<SharedString>)>,
//     )>,
//     pub builder: Option<Rc<dyn Fn(&mut Window, &mut App) -> AnyView>>,
// }

// impl ComponentTooltip {
//     /// Apply this tooltip to a `Stateful<Div>` (or any `ManagedTooltipExt` element).
//     pub fn apply<E: ManagedTooltipExt>(self, el: E) -> E {
//         if let Some(builder) = self.builder {
//             el.managed_tooltip(move |window, cx| builder(window, cx))
//         } else if let Some((text, action)) = self.text {
//             el.managed_tooltip(move |window, cx| {
//                 Tooltip::new(text.clone())
//                     .when_some(action.clone(), |this, (action, context)| {
//                         this.action(
//                             action.boxed_clone().as_ref(),
//                             context.as_ref().map(|c| c.as_ref()),
//                         )
//                     })
//                     .build(window, cx)
//             })
//         } else {
//             el
//         }
//     }
// }

// // ── Internal managed tooltip trait ──────────────────────────────────────────

// pub(crate) trait ManagedTooltipExt:
//     StatefulInteractiveElement + crate::ElementExt + Sized
// {
//     fn managed_tooltip(
//         self,
//         build_tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
//     ) -> Self {
//         let build_tooltip = Rc::new(build_tooltip);
//         let trigger_bounds_cell: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
//         let bounds_writer = trigger_bounds_cell.clone();

//         self.on_prepaint(move |bounds, _, _| {
//             bounds_writer.set(bounds);
//         })
//         .on_hover({
//             let trigger_bounds_cell = trigger_bounds_cell.clone();
//             let build_tooltip = build_tooltip.clone();
//             move |hovered, window, cx| {
//                 if let Some(overlay) = Root::tooltip_overlay(window, cx) {
//                     if *hovered {
//                         let bounds = trigger_bounds_cell.get();
//                         overlay.update(cx, |o: &mut TooltipOverlay, cx| {
//                             o.request_show(
//                                 TooltipContent {
//                                     build: build_tooltip.clone(),
//                                     trigger_bounds: bounds,
//                                 },
//                                 window,
//                                 cx,
//                             );
//                         });
//                     } else {
//                         overlay.update(cx, |o: &mut TooltipOverlay, cx| {
//                             o.request_hide(window, cx);
//                         });
//                     }
//                 }
//             }
//         })
//         .on_mouse_down(MouseButton::Left, move |_, window, cx| {
//             if let Some(overlay) = Root::tooltip_overlay(window, cx) {
//                 overlay.update(cx, |overlay, cx| {
//                     overlay.hide(cx);
//                 });
//             }
//         })
//     }
// }

// impl<E: StatefulInteractiveElement + crate::ElementExt> ManagedTooltipExt for E {}

//! Popover card under the \"repeat\" button: pick a repeat frequency
//! (daily/weekly/monthly/yearly presets or a custom N-day interval) and
//! save it, or remove an existing repeat template.

use gpui::{
    AppContext, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, px, rgb, prelude::FluentBuilder,
};
use gpui_component::Disableable;
use gpui_component::Sizable;
use gpui_component::StyledExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use storage::RepeatTaskTemplate;

use crate::theme::HAIRLINE;

#[derive(Clone, Debug)]
pub enum RepeatPickerEvent {
    /// The user chose a frequency (in days) for the repeat template.
    Saved { interval_days: u64 },
    /// The user asked to delete the repeat template.
    Removed,
}

/// Human label for an interval in days.
pub fn interval_label(interval_days: u64) -> String {
    match interval_days {
        1 => "Daily".to_string(),
        7 => "Weekly".to_string(),
        30 => "Monthly".to_string(),
        365 => "Yearly".to_string(),
        n => format!("Every {n} days"),
    }
}

const PRESETS: [(u64, &str); 4] = [(1, "Daily"), (7, "Weekly"), (30, "Monthly"), (365, "Yearly")];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Preset(u64),
    Custom,
}

pub struct RepeatPicker {
    current: Option<RepeatTaskTemplate>,
    choice: Choice,
    custom_days: u64,
    custom_input: Option<Entity<InputState>>,
    _custom_subscription: Option<Subscription>,
}

impl RepeatPicker {
    pub fn new(
        current: Option<RepeatTaskTemplate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_custom_days = match &current {
            Some(template) if !PRESETS.iter().any(|(days, _)| *days == template.interval_days) => {
                template.interval_days
            }
            _ => 7,
        };
        let choice = match &current {
            Some(template) if PRESETS.iter().any(|(days, _)| *days == template.interval_days) => {
                Choice::Preset(template.interval_days)
            }
            Some(_) => Choice::Custom,
            None => Choice::Preset(7),
        };
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(format!("{initial_custom_days}"), window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| match event {
            InputEvent::Change => this.apply_custom(cx),
            InputEvent::PressEnter { .. } => this.save(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            current,
            choice,
            custom_days: initial_custom_days,
            custom_input: Some(input),
            _custom_subscription: Some(subscription),
        }
    }

    /// Set the task's current template after an async load (the picker is
    /// opened before the lookup finishes).
    pub fn set_current(&mut self, current: Option<RepeatTaskTemplate>, cx: &mut Context<Self>) {
        self.current = current;
        cx.notify();
    }

    fn apply_custom(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = self.custom_input.clone() {
            let text = input.read(cx).text().to_string();
            self.custom_days = text.trim().parse::<u64>().unwrap_or(0);
            self.choice = Choice::Custom;
            cx.notify();
        }
    }

    fn effective_days(&self) -> u64 {
        match self.choice {
            Choice::Preset(days) => days,
            Choice::Custom => self.custom_days,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.effective_days() > 0 {
            cx.emit(RepeatPickerEvent::Saved {
                interval_days: self.effective_days(),
            });
        }
    }
}

impl EventEmitter<RepeatPickerEvent> for RepeatPicker {}

impl Render for RepeatPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.current.clone();
        let custom_input = self.custom_input.clone();
        let effective = self.effective_days();

        let presets = PRESETS.iter().map(|(days, label)| {
            let selected = self.choice == Choice::Preset(*days);
            Button::new(("repeat-preset", *days))
                .ghost()
                .compact()
                .with_size(gpui_component::Size::Small)
                .border_1()
                .border_color(if selected {
                    rgb(0x93c5fd)
                } else {
                    rgb(HAIRLINE)
                })
                .text_color(if selected {
                    rgb(0x93c5fd)
                } else {
                    rgb(0xa3a3a3)
                })
                .label(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.choice = Choice::Preset(*days);
                    cx.notify();
                }))
        });

        div()
            .v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xe5e5e5))
                    .child("Repeat"),
            )
            .when_some(current.as_ref(), |this, template| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xa3a3a3))
                        .child(format!(
                            "Currently: {}",
                            interval_label(template.interval_days)
                        )),
                )
            })
            .child(
                div()
                    .h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(presets),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0xa3a3a3)).child("Every"))
                    .child(div().w(px(56.)).children(
                        custom_input.map(|input| {
                            Input::new(&input).small().appearance(false)
                        }),
                    ))
                    .child(div().text_xs().text_color(rgb(0xa3a3a3)).child("days")),
            )
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap_2()
                    .when(current.is_some(), |this| {
                        this.child(
                            Button::new("remove-repeat")
                                .ghost()
                                .compact()
                                .text_color(rgb(0xff6b6b))
                                .label("Remove")
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    cx.emit(RepeatPickerEvent::Removed);
                                })),
                        )
                    })
                    .child(
                        Button::new("save-repeat")
                            .primary()
                            .compact()
                            .label("Save")
                            .disabled(effective == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save(cx);
                            })),
                    ),
            )
    }
}
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
    /// The user chose a frequency (in days) and optional time of day.
    Saved {
        interval_days: u64,
        time_of_day: Option<u64>,
    },
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

/// Human label for a repeat, e.g. "Daily at 6:00 PM" or "Every 3 days".
pub fn repeat_label(interval_days: u64, time_of_day: Option<u64>) -> String {
    let interval = interval_label(interval_days);
    match time_of_day {
        Some(minutes) => format!("{interval} at {}", format_time(minutes)),
        None => interval,
    }
}

fn format_time(minutes_since_midnight: u64) -> String {
    let hour = minutes_since_midnight / 60;
    let minute = minutes_since_midnight % 60;
    let am_pm = if hour < 12 { "AM" } else { "PM" };
    let hour12 = hour % 12;
    let hour12 = if hour12 == 0 { 12 } else { hour12 };
    format!("{hour12}:{minute:02} {am_pm}")
}

/// Parse "H:MM" / "HH:MM" (24h) into minutes since midnight.
fn parse_time(text: &str) -> Option<u64> {
    let (hour, minute) = text.trim().split_once(':')?;
    let hour: u64 = hour.trim().parse().ok()?;
    let minute: u64 = minute.trim().parse().ok()?;
    if hour < 24 && minute < 60 {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

/// "HH:MM" in 24h for the time input.
fn format_minutes(minutes: u64) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
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
    time_of_day: Option<u64>,
    custom_input: Option<Entity<InputState>>,
    _custom_subscription: Option<Subscription>,
    time_input: Option<Entity<InputState>>,
    _time_subscription: Option<Subscription>,
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
        let initial_time = current.as_ref().and_then(|template| template.time_of_day);
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
        let time_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            if let Some(minutes) = initial_time {
                state.set_value(format_minutes(minutes), window, cx);
            } else {
                state.set_placeholder("18:00", window, cx);
            }
            state
        });
        let time_subscription = cx.subscribe(&time_input, |this, _, event, cx| match event {
            InputEvent::Change => this.apply_time(cx),
            InputEvent::PressEnter { .. } => this.save(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            current,
            choice,
            custom_days: initial_custom_days,
            time_of_day: initial_time,
            custom_input: Some(input),
            _custom_subscription: Some(subscription),
            time_input: Some(time_input),
            _time_subscription: Some(time_subscription),
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

    fn apply_time(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = self.time_input.clone() {
            let text = input.read(cx).text().to_string();
            self.time_of_day = parse_time(&text);
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
                time_of_day: self.time_of_day,
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
                            repeat_label(template.interval_days, template.time_of_day)
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
                    .items_center()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0xa3a3a3)).child("at"))
                    .child(div().w(px(56.)).children(
                        self.time_input.clone().map(|input| {
                            Input::new(&input).small().appearance(false)
                        }),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child("(optional, 24h)"),
                    ),
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
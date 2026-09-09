//! Date-and-time picker: text field, quick actions, scrollable month
//! calendar and time stepper. Emits [`DateTimePickerEvent::Committed`]
//! with epoch seconds; persistence stays with the caller.

use gpui::{
    div, px, rgb, AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Sizable, StyledExt};

use super::calendar::{month_block, month_title, weekday_header, MONTH_BLOCK_PX};
use crate::theme::HAIRLINE;

#[derive(Clone, Copy, Debug)]
pub enum DateTimePickerEvent {
    Committed(u64),
}

pub struct DateTimePicker {
    input: Entity<InputState>,
    _input_subscription: Subscription,
    picked_date: Option<jiff::civil::Date>,
    hour: u8,
    minute: u8,
    show_time_picker: bool,
    error: Option<String>,
    cal_scroll: ScrollHandle,
}

impl DateTimePicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Type a date", window, cx);
            state
        });
        let input_subscription = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_text(cx);
            }
        });
        let picker = Self {
            input: input.clone(),
            _input_subscription: input_subscription,
            picked_date: None,
            hour: 9,
            minute: 0,
            show_time_picker: false,
            error: None,
            cal_scroll: ScrollHandle::new(),
        };
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
        picker
    }

    fn commit_text(&mut self, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).text().to_string();
        match parse_future_datetime(&raw) {
            None => {
                self.error = Some("Use YYYY-MM-DD HH:MM, in the future".to_string());
                cx.notify();
            }
            Some(until) => cx.emit(DateTimePickerEvent::Committed(until)),
        }
    }

    fn commit_datetime(&mut self, cx: &mut Context<Self>) {
        let Some(date) = self.picked_date else {
            return;
        };
        let timestamp = date
            .at(self.hour as i8, self.minute as i8, 0, 0)
            .to_zoned(jiff::tz::TimeZone::system())
            .map(|zoned| zoned.timestamp().as_second())
            .unwrap_or(0);
        if timestamp <= now_secs() {
            self.error = Some("Pick a time in the future".to_string());
            cx.notify();
            return;
        }
        cx.emit(DateTimePickerEvent::Committed(timestamp as u64));
    }

    fn quick_today(&mut self, cx: &mut Context<Self>) {
        self.picked_date = Some(today_date());
        self.show_time_picker = true;
        self.error = None;
        cx.notify();
    }

    fn quick_tomorrow(&mut self, cx: &mut Context<Self>) {
        self.picked_date = today_date().tomorrow().ok();
        self.show_time_picker = true;
        self.error = None;
        cx.notify();
    }

    fn quick_weekend(&mut self, cx: &mut Context<Self>) {
        self.picked_date = Some(next_monday_offset_weekday(today_date(), 5, false));
        self.hour = 9;
        self.minute = 0;
        self.show_time_picker = false;
        self.error = None;
        cx.notify();
        self.commit_datetime(cx);
    }

    fn quick_next_week(&mut self, cx: &mut Context<Self>) {
        self.picked_date = Some(next_monday_offset_weekday(today_date(), 0, true));
        self.hour = 9;
        self.minute = 0;
        self.show_time_picker = false;
        self.error = None;
        cx.notify();
        self.commit_datetime(cx);
    }

    fn shift_hour(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.hour = (self.hour as i32 + delta).rem_euclid(24) as u8;
        cx.notify();
    }

    fn shift_minute(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.minute = (self.minute as i32 + delta).rem_euclid(60) as u8;
        cx.notify();
    }

    /// Twelve upcoming months from the current one for the scroll view.
    fn months() -> Vec<(i16, i8)> {
        let today = today_date();
        (0..12)
            .map(|k| {
                let total = today.month() as i32 - 1 + k;
                (today.year() + (total / 12) as i16, (total % 12 + 1) as i8)
            })
            .collect()
    }

    /// Top-visible month from the scroll offset (every block is fixed height).
    fn sticky_month(&self) -> (i16, i8) {
        let months = Self::months();
        let offset: f32 = self.cal_scroll.offset().y.into();
        let top = (offset / MONTH_BLOCK_PX).floor() as usize;
        months[top.min(months.len() - 1)]
    }
}

impl EventEmitter<DateTimePickerEvent> for DateTimePicker {}

impl Render for DateTimePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div().v_flex().gap_0();

        panel = panel.child(
            div()
                .px_3()
                .py_2()
                .v_flex()
                .gap_1()
                .child(Input::new(&self.input).small().appearance(false)),
        );
        if let Some(error) = self.error.clone() {
            panel = panel.child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(0xe06c60))
                    .child(error),
            );
        }

        panel = panel.child(
            div()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(rgb(HAIRLINE))
                .h_flex()
                .flex_wrap()
                .gap_2()
                .child(mini_button("quick-today", "Today").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.quick_today(cx);
                    },
                )))
                .child(
                    mini_button("quick-tomorrow", "Tomorrow").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.quick_tomorrow(cx);
                        },
                    )),
                )
                .child(
                    mini_button("quick-weekend", "This weekend").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.quick_weekend(cx);
                        },
                    )),
                )
                .child(
                    mini_button("quick-next-week", "Next week").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.quick_next_week(cx);
                        },
                    )),
                ),
        );

        {
            let picker = cx.entity().clone();
            let today = today_date();
            let months = Self::months();
            let (sticky_year, sticky_month) = self.sticky_month();
            panel = panel.child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(HAIRLINE))
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(0xe5e5e5))
                            .child(month_title(sticky_year, sticky_month)),
                    )
                    .child(weekday_header())
                    .child(
                        div()
                            .id("until-months")
                            .h(px(300.))
                            .track_scroll(&self.cal_scroll)
                            .overflow_y_scrollbar()
                            .on_scroll_wheel(cx.listener(|_this, _, _, cx| {
                                cx.notify();
                            }))
                            .v_flex()
                            .gap_0()
                            .children(months.into_iter().map(|(year, month)| {
                                let picker = picker.clone();
                                let picked = self.picked_date;
                                month_block(year, month, picked, today, move |date, _window, cx| {
                                    picker.update(cx, |this, cx| {
                                        this.picked_date = Some(date);
                                        this.show_time_picker = true;
                                        this.error = None;
                                        cx.notify();
                                    });
                                })
                            })),
                    ),
            );
        }

        if self.show_time_picker {
            if let Some(date) = self.picked_date {
                let hour = self.hour;
                let minute = self.minute;
                panel = panel.child(
                    div()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(rgb(HAIRLINE))
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe5e5e5))
                                .child(date.to_string()),
                        )
                        .child(mini_button("hour-down", "-").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shift_hour(-1, cx);
                            },
                        )))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe5e5e5))
                                .child(format!("{hour:02}")),
                        )
                        .child(mini_button("hour-up", "+").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shift_hour(1, cx);
                            },
                        )))
                        .child(div().text_sm().text_color(rgb(0xa3a3a3)).child(":"))
                        .child(mini_button("minute-down", "-").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shift_minute(-15, cx);
                            },
                        )))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe5e5e5))
                                .child(format!("{minute:02}")),
                        )
                        .child(mini_button("minute-up", "+").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shift_minute(15, cx);
                            },
                        )))
                        .child(
                            mini_button("set-until-datetime", "Set").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.commit_datetime(cx);
                                },
                            )),
                        ),
                );
            }
        }

        panel
    }
}

/// Small transparent button matching the relationships section style.
fn mini_button(id: &'static str, label: &str) -> Button {
    Button::new(id)
        .ghost()
        .compact()
        .with_size(gpui_component::Size::Small)
        .border_1()
        .border_color(rgb(HAIRLINE))
        .text_color(rgb(0xa3a3a3))
        .label(label)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64
}

/// Today's date in the system timezone.
fn today_date() -> jiff::civil::Date {
    jiff::Zoned::now().date()
}

/// Upcoming date with the given Monday-based weekday offset (0 = Monday,
/// 5 = Saturday). Stays on `from` when it already matches unless
/// `strict` is set, in which case it moves a full week ahead.
fn next_monday_offset_weekday(
    from: jiff::civil::Date,
    target: i8,
    strict: bool,
) -> jiff::civil::Date {
    let mut ahead = (target - from.weekday().to_monday_zero_offset() + 7) % 7;
    if strict && ahead == 0 {
        ahead = 7;
    }
    from.checked_add(jiff::ToSpan::days(ahead as i64))
        .expect("small date offset")
}

/// Parse "YYYY-MM-DD HH:MM" (or date only, midnight) in the system timezone.
/// Returns None for invalid input or times that are not in the future.
fn parse_future_datetime(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let datetime = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", raw)
        .or_else(|_| jiff::civil::Date::strptime("%Y-%m-%d", raw).map(|date| date.at(0, 0, 0, 0)))
        .ok()?;
    let until = datetime
        .to_zoned(jiff::tz::TimeZone::system())
        .ok()?
        .timestamp()
        .as_second();
    (until > now_secs()).then_some(until as u64)
}

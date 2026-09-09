//! Minimal month calendar: weekday initials plus day numbers only.
//!
//! Selection state lives with the caller; interactions come back as
//! [`CalendarEvent`]s. Past days render dimmed and are not clickable.

use gpui::{
    div, px, rgb, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window,
};
use gpui_component::StyledExt;
use std::rc::Rc;

use crate::theme::HAIRLINE;

#[derive(Clone, Copy, Debug)]
pub enum CalendarEvent {
    SelectDay(jiff::civil::Date),
    ShiftMonth(i32),
}

/// Monday-first month grid for `year`/`month`, with prev/next month
/// navigation. `today` drives the today outline and disables past days.
pub fn month_calendar(
    year: i16,
    month: i8,
    selected: Option<jiff::civil::Date>,
    today: jiff::civil::Date,
    on_event: impl Fn(CalendarEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_event = Rc::new(on_event);

    let first = jiff::civil::Date::new(year, month, 1).ok();
    let (title, lead_blanks, days_in_month) = match first {
        Some(first) => (
            first.strftime("%B %Y").to_string(),
            first.weekday().to_monday_zero_offset() as usize,
            first.days_in_month() as usize,
        ),
        None => ("".to_string(), 0, 0),
    };

    let mut cells: Vec<gpui::AnyElement> = Vec::new();
    for i in 0..lead_blanks {
        cells.push(
            div()
                .id(gpui::ElementId::named_usize("calendar-blank", i))
                .w(px(26.))
                .h(px(22.))
                .into_any_element(),
        );
    }
    for day in 1..=days_in_month {
        let index = lead_blanks + day - 1;
        let date = jiff::civil::Date::new(year, month, day as i8).ok();
        let is_past = date.is_some_and(|d| d < today);
        let is_selected = selected.is_some_and(|d| Some(d) == date);
        let is_today = date.is_some_and(|d| d == today);
        let mut cell = div()
            .id(gpui::ElementId::named_usize("calendar-day", index))
            .w(px(26.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_xs()
            .text_color(if is_past {
                rgb(0x555555)
            } else {
                rgb(0xe5e5e5)
            })
            .child(day.to_string());
        if is_selected {
            cell = cell.bg(rgb(0x3a3a3a));
        } else if is_today {
            cell = cell.border_1().border_color(rgb(HAIRLINE));
        } else if !is_past {
            cell = cell.hover(|s| s.bg(rgb(0x2a2a2a)));
        }
        if !is_past {
            if let Some(date) = date {
                let on_event = on_event.clone();
                cell = cell.on_click(move |_, window, cx| {
                    on_event(CalendarEvent::SelectDay(date), window, cx);
                });
            }
        }
        cells.push(cell.into_any_element());
    }

    let mut weeks: Vec<Vec<gpui::AnyElement>> = vec![Vec::new()];
    for cell in cells {
        if weeks.last().is_some_and(|week| week.len() == 7) {
            weeks.push(Vec::new());
        }
        if let Some(week) = weeks.last_mut() {
            week.push(cell);
        }
    }
    let week_rows = weeks
        .into_iter()
        .enumerate()
        .map(|(week, days)| {
            div()
                .id(gpui::ElementId::named_usize("calendar-week", week))
                .h_flex()
                .gap_1()
                .children(days)
        })
        .collect::<Vec<_>>();

    let nav_button = |id: &'static str, label: &'static str, delta: i32| {
        let on_event = on_event.clone();
        div()
            .id(id)
            .px_2()
            .text_sm()
            .text_color(rgb(0xa3a3a3))
            .hover(|s| s.text_color(rgb(0xe5e5e5)))
            .child(label.to_string())
            .on_click(move |_, window, cx| {
                on_event(CalendarEvent::ShiftMonth(delta), window, cx);
            })
    };

    div()
        .v_flex()
        .gap_1()
        .child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .child(nav_button("calendar-prev-month", "<", -1))
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(0xe5e5e5))
                        .child(title),
                )
                .child(nav_button("calendar-next-month", ">", 1)),
        )
        .child(
            div().h_flex().gap_1().children(
                ["M", "T", "W", "T", "F", "S", "S"]
                    .into_iter()
                    .enumerate()
                    .map(|(i, initial)| {
                        div()
                            .id(gpui::ElementId::named_usize("calendar-weekday", i))
                            .w(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(initial)
                    }),
            ),
        )
        .children(week_rows)
}

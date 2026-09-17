//! Minimal month calendar: weekday initials plus day numbers only.
//!
//! Months render as fixed-height blocks so a scroll container can map its
//! offset back to the top-visible month (see `MONTH_BLOCK_PX`). Selection
//! state lives with the caller; day clicks come back through the
//! `on_select_day` callback.

use gpui::{
    div, px, rgb, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window,
};
use gpui_component::StyledExt;
use std::rc::Rc;

use crate::theme::HAIRLINE;

/// Height of one month block: 20px separator + 4px gap + six 22px week
/// rows with 4px gaps (20 + 4 + 6 * 22 + 5 * 4).
pub const MONTH_BLOCK_PX: f32 = 176.0;

const CELL_W: f32 = 26.0;
const CELL_H: f32 = 22.0;

/// Full month name and year for a sticky header, e.g. "September 2026".
pub fn month_title(year: i16, month: i8) -> String {
    jiff::civil::Date::new(year, month, 1)
        .map(|first| first.strftime("%B %Y").to_string())
        .unwrap_or_default()
}

/// Single-line Monday-first weekday initials row.
pub fn weekday_header() -> impl IntoElement {
    div().h_flex().gap_1().children(
        ["M", "T", "W", "T", "F", "S", "S"]
            .into_iter()
            .enumerate()
            .map(|(i, initial)| {
                div()
                    .id(gpui::ElementId::named_usize("calendar-weekday", i))
                    .w(px(CELL_W))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(0x737373))
                    .child(initial)
            }),
    )
}

/// One fixed-height month block: abbreviated month separator with a
/// full-width underline, then exactly six week rows of day numbers.
/// Past days render dimmed and are not clickable.
pub fn month_block(
    year: i16,
    month: i8,
    selected: Option<jiff::civil::Date>,
    today: jiff::civil::Date,
    on_select_day: impl Fn(jiff::civil::Date, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select_day = Rc::new(on_select_day);

    let first = jiff::civil::Date::new(year, month, 1).ok();
    let (abbrev, lead_blanks, days_in_month) = match first {
        Some(first) => (
            first.strftime("%b").to_string(),
            first.weekday().to_monday_zero_offset() as usize,
            first.days_in_month() as usize,
        ),
        None => ("".to_string(), 0, 0),
    };

    let stamp = (year as usize) * 100 + (month as usize);

    let mut cells: Vec<gpui::AnyElement> = Vec::new();
    for i in 0..lead_blanks {
        cells.push(
            div()
                .id(gpui::ElementId::named_usize(
                    "calendar-blank",
                    stamp * 100 + i,
                ))
                .w(px(CELL_W))
                .h(px(CELL_H))
                .into_any_element(),
        );
    }
    for day in 1..=days_in_month {
        let index = stamp * 100 + day;
        let date = jiff::civil::Date::new(year, month, day as i8).ok();
        let is_past = date.is_some_and(|d| d < today);
        let is_selected = selected.is_some_and(|d| Some(d) == date);
        let is_today = date.is_some_and(|d| d == today);
        let mut cell = div()
            .id(gpui::ElementId::named_usize("calendar-day", index))
            .w(px(CELL_W))
            .h(px(CELL_H))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(if is_past {
                rgb(0x555555)
            } else {
                rgb(0xe5e5e5)
            });
        if is_selected {
            // Background circle behind the chosen date.
            cell = cell.child(
                div()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(0x3a3a3a))
                    .child(day.to_string()),
            );
        } else {
            cell = cell.rounded_md().child(day.to_string());
            if is_today {
                cell = cell.border_1().border_color(rgb(HAIRLINE));
            } else if !is_past {
                cell = cell.hover(|s| s.bg(rgb(0x2a2a2a)));
            }
        }
        if !is_past {
            if let Some(date) = date {
                let on_select_day = on_select_day.clone();
                cell = cell.on_click(move |_, window, cx| {
                    on_select_day(date, window, cx);
                });
            }
        }
        cells.push(cell.into_any_element());
    }
    // Always six rows so every month block is exactly MONTH_BLOCK_PX tall.
    while cells.len() < 42 {
        let i = cells.len();
        cells.push(
            div()
                .id(gpui::ElementId::named_usize(
                    "calendar-pad",
                    stamp * 100 + i,
                ))
                .w(px(CELL_W))
                .h(px(CELL_H))
                .into_any_element(),
        );
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
                .id(gpui::ElementId::named_usize(
                    "calendar-week",
                    stamp * 10 + week,
                ))
                .h_flex()
                .gap_1()
                .children(days)
        })
        .collect::<Vec<_>>();

    div()
        .v_flex()
        .gap_1()
        .child(
            div()
                .h(px(20.))
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(rgb(0xa3a3a3))
                        .child(abbrev),
                )
                .child(div().flex_1().h_px().bg(rgb(HAIRLINE))),
        )
        .children(week_rows)
}

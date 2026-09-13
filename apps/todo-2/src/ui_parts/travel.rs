//! The travel-checklists panel, shown above the managed tag's task list.
//! A "+ New trip" button (same treatment as the task-details buttons)
//! opens an add-trip popover card (same card component as the details
//! pickers) with a trip name field, trip length chips, and activity chips.
//! Creating a trip starts a workflow run and spawns its checklist items
//! into the tag's Pack / Before leaving sections, which the task list
//! below renders.

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Task, Window, div, px, rgb, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::*;
use gpui_component::{Disableable, Sizable, Size, StyledExt};

use crate::store::Store;
use crate::theme::{CARD_BG, HAIRLINE};

const DAY_OPTIONS: [&str; 8] = ["1", "2", "3", "5", "8", "13", "30+", "N"];
const ACTIVITY_OPTIONS: [&str; 4] = ["hiking", "swimming", "wedding", "camping"];

fn activity_label(activity: &str) -> &'static str {
    match activity {
        "hiking" => "Hiking",
        "swimming" => "Swimming",
        "wedding" => "Wedding",
        "camping" => "Camping",
        _ => "Trip",
    }
}

/// Auto-generated trip name when the name field is left empty, e.g.
/// "Hiking, Swimming · 5-day".
fn fallback_trip_name(days: &str, activities: &[String]) -> String {
    let day_part = format!("{days}-day");
    if activities.is_empty() {
        return day_part;
    }
    let labels: Vec<String> = activities
        .iter()
        .map(|a| activity_label(a).to_string())
        .collect();
    format!("{} · {day_part}", labels.join(", "))
}

#[derive(Clone)]
pub enum TravelPanelEvent {
    /// A trip was created; the task list below should reload so the new
    /// checklist items appear under their sections.
    TripAdded,
}

pub struct TravelPanel {
    store: Store,
    recipe_id: u64,
    /// The tag the panel is open on. An app may be attached to several tags,
    /// so trips are generated into the selected one.
    tag_id: u64,
    tag_label: String,
    /// The add-trip popover is open.
    adding: bool,
    /// When the popover closes from an outside mousedown, the matching
    /// click on the "+ New trip" button still arrives afterwards: it must
    /// not reopen the popover (that press was the toggle intent, already
    /// fulfilled). Only presses within this window are swallowed.
    last_outside_close: Option<std::time::Instant>,
    name: Entity<InputState>,
    /// Selected trip length: "1".."13", "30+", or "N" (custom).
    days: Option<String>,
    /// Value entered when "N" is the selected length.
    custom_days: Entity<InputState>,
    /// Selected activities, sorted.
    activities: Vec<String>,
    _add: Option<Task<()>>,
}

impl TravelPanel {
    pub fn new(
        store: Store,
        recipe_id: u64,
        tag_id: u64,
        tag_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            store,
            recipe_id,
            tag_id,
            tag_label,
            adding: false,
            last_outside_close: None,
            name: cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_placeholder("Trip name (e.g. Costa Rica)", window, cx);
                input
            }),
            days: None,
            custom_days: cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_placeholder("Days (e.g. 10)", window, cx);
                input
            }),
            activities: Vec::new(),
            _add: None,
        }
    }

    /// Point the panel at another managed tag (reused across selections).
    pub fn set_tag(
        &mut self,
        recipe_id: u64,
        tag_id: u64,
        tag_label: String,
        cx: &mut Context<Self>,
    ) {
        self.recipe_id = recipe_id;
        self.tag_id = tag_id;
        self.tag_label = tag_label;
        self.close_add(cx);
    }

    pub fn open_add(&mut self) {
        self.adding = true;
    }

    /// Whether the add-trip popover is open (used by the window-wide
    /// Escape observer to close it first).
    pub fn is_adding(&self) -> bool {
        self.adding
    }

    /// Toggle the popover. Swallows the click when it directly follows an
    /// outside mousedown that already closed the popover (re-clicking the
    /// button): that press was the close intent, not a reopen.
    pub fn toggle_add(&mut self, cx: &mut Context<Self>) {
        if self
            .last_outside_close
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(400))
        {
            self.last_outside_close = None;
            return;
        }
        self.adding = !self.adding;
        cx.notify();
    }

    fn close_add_outside(&mut self, cx: &mut Context<Self>) {
        self.last_outside_close = Some(std::time::Instant::now());
        self.close_add(cx);
    }

    pub fn close_add(&mut self, cx: &mut Context<Self>) {
        self.adding = false;
        cx.notify();
    }

    fn toggle_activity(&mut self, activity: &str) {
        if let Some(index) = self.activities.iter().position(|a| a == activity) {
            self.activities.remove(index);
        } else {
            self.activities.push(activity.to_string());
            self.activities.sort();
        }
    }

    /// The resolved trip length: the selected button, or the custom input
    /// when "N" is selected.
    fn selected_days(&self, cx: &App) -> Option<String> {
        match self.days.as_deref() {
            None => None,
            Some("N") => {
                let custom = self.custom_days.read(cx).text().to_string();
                let custom = custom.trim().to_string();
                (!custom.is_empty()).then_some(custom)
            }
            Some(days) => Some(days.to_string()),
        }
    }

    fn add_trip(&mut self, cx: &mut Context<Self>) {
        let Some(days) = self.selected_days(cx) else {
            return;
        };
        let recipe_id = self.recipe_id;
        let tag_id = self.tag_id;
        if recipe_id == 0 || tag_id == 0 {
            return;
        }
        let typed_name = self.name.read(cx).text().to_string();
        let name = typed_name.trim().to_string();
        let name = if name.is_empty() {
            fallback_trip_name(&days, &self.activities)
        } else {
            name
        };
        let activities = self.activities.clone();
        let store = self.store.clone();
        self._add = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = store
                .create_trip(recipe_id, tag_id, name, days, activities, cx)
                .await
            {
                tracing::error!("failed to create trip: {e}");
            }
            this.update(cx, |this, cx| {
                this.adding = false;
                this.days = None;
                this.activities.clear();
                this._add = None;
                cx.emit(TravelPanelEvent::TripAdded);
                cx.notify();
            })
            .ok();
        }));
    }
}

impl EventEmitter<TravelPanelEvent> for TravelPanel {}

impl TravelPanel {
    /// The "+ New trip" button, rendered by the Layout at the end of the
    /// task list's title row. Same treatment as the task-details buttons:
    /// ghost, compact, small, hairline outline.
    pub fn new_trip_button(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        Button::new("new-trip")
            .ghost()
            .compact()
            .with_size(Size::Small)
            .border_1()
            .border_color(rgb(HAIRLINE))
            .text_color(rgb(0xa3a3a3))
            .label("+ New trip")
            .tooltip("Add a trip: its checklist appears below in Pack and Before leaving sections")
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_add(cx);
            }))
            .into_any_element()
    }

    /// The add-trip popover: the same absolute card component the task
    /// details pickers use, anchored top-right under the "+ New trip"
    /// button. Closed by toggling the button, an outside mousedown, or
    /// Esc (via the Layout's window-wide keystroke observer). Rendered by
    /// the Layout as the LAST child of the managed panel so GPUI paints
    /// it above the task list (paint order follows tree order; there is no
    /// z-index).
    pub fn popover(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.adding {
            return div().into_any_element();
        }
        let days = self.days.clone();
        let activities = self.activities.clone();
        let can_add = self.selected_days(cx).is_some();

        let day_chips: Vec<gpui::AnyElement> = DAY_OPTIONS
            .iter()
            .map(|option| {
                let value = option.to_string();
                let selected = days.as_deref() == Some(option);
                chip(
                    format!("trip-days-{option}"),
                    option.to_string(),
                    selected,
                    cx.listener(move |this, _, _, _| {
                        this.days = Some(value.clone());
                    }),
                )
            })
            .collect();
        let activity_chips: Vec<gpui::AnyElement> = ACTIVITY_OPTIONS
            .iter()
            .map(|option| {
                let value = option.to_string();
                let selected = activities.iter().any(|a| a == option);
                chip(
                    format!("trip-activity-{option}"),
                    activity_label(option).to_string(),
                    selected,
                    cx.listener(move |this, _, _, _| {
                        this.toggle_activity(&value);
                    }),
                )
            })
            .collect();

        div()
            // Top-right card under the "+ New trip" button: fixed width
            // aligned with the task list's right padding, pushed below the
            // title row (32px list padding + ~32px title height + 8px gap)
            // so it never overlaps the button.
            .absolute()
            .top(px(72.))
            .right(px(32.))
            .w(px(320.))
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(HAIRLINE))
            .rounded_md()
            .px_3()
            .py_2()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_add_outside(cx);
            }))
            .v_flex()
            .gap_2()
            .child(field_label("Trip name"))
            .child(Input::new(&self.name).with_size(Size::Small))
            .child(field_label("Trip length"))
            .child(div().h_flex().items_center().gap_1p5().children(day_chips))
            .when(days.as_deref() == Some("N"), |this| {
                this.child(Input::new(&self.custom_days).with_size(Size::Small))
            })
            .child(field_label("Activities"))
            .child(div().h_flex().items_center().gap_1p5().children(activity_chips))
            .child(
                // No tooltip here: the popover unmounts the moment this is
                // clicked, orphaning any visible tooltip on screen.
                Button::new("add-trip-confirm")
                    .compact()
                    .label("Add trip")
                    .disabled(!can_add)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_trip(cx);
                    })),
            )
            .into_any_element()
    }
}

/// Small field label, matching the task-details pickers.
fn field_label(label: &str) -> gpui::AnyElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(rgb(0xa3a3a3))
        .child(label.to_string())
        .into_any_element()
}

/// Small toggle chip for the day/activity pickers.
fn chip(
    id: String,
    label: String,
    selected: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(if selected { rgb(0x4a6fa5) } else { rgb(HAIRLINE) })
        .bg(if selected { rgb(0x2f4057) } else { rgb(0x242424) })
        .text_sm()
        .text_color(if selected { rgb(0xdbe6f5) } else { rgb(0xa3a3a3) })
        .hover(|s| s.bg(if selected { rgb(0x2f4057) } else { rgb(0x2a2a2a) }))
        .on_click(on_click)
        .child(label)
        .into_any_element()
}
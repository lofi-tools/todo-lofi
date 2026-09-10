//! The special panel for a managed tag owned by the travel-checklists
//! automation. At the top the user builds a trip by choosing a length
//! (1/2/3/5/8/13/30+/custom days) and activities (hiking, swimming,
//! wedding, camping); each trip then generates two checklist sections,
//! "Pack" and "Before leaving", with items that are ordinary tasks.

use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Task, Window, div, px, rgb, prelude::FluentBuilder,
};
use gpui_component::{Disableable, Sizable, Size, StyledExt};
use gpui_component::button::Button;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::*;
use gpui_component::scroll::ScrollableElement;
use storage::prelude::*;

use crate::store::Store;
use crate::theme::{APP_BG, HAIRLINE};

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

/// Auto-generated trip name, e.g. "Hiking, Swimming · 5-day".
fn trip_name(days: &str, activities: &[String]) -> String {
    let day_part = format!("{days}-day");
    if activities.is_empty() {
        return day_part;
    }
    let labels: Vec<String> = activities.iter().map(|a| activity_label(a).to_string()).collect();
    format!("{} · {day_part}", labels.join(", "))
}

pub struct TravelPanel {
    store: Store,
    tag_id: u64,
    tag_label: String,
    trips: Vec<TripWithItems>,
    /// Selected trip length: "1".."13", "30+", or "N" (custom).
    days: Option<String>,
    /// Value entered when "N" is the selected length.
    custom_days: Entity<InputState>,
    /// Selected activities, sorted.
    activities: Vec<String>,
    _fetch: Option<Task<()>>,
}

impl TravelPanel {
    pub fn new(
        store: Store,
        tag_id: u64,
        tag_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self {
            store,
            tag_id,
            tag_label,
            trips: Vec::new(),
            days: None,
            custom_days: cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_placeholder("Days (e.g. 10)", window, cx);
                input
            }),
            activities: Vec::new(),
            _fetch: None,
        };
        panel.refresh(cx);
        panel
    }

    /// Point the panel at another managed tag (reused across selections).
    pub fn set_tag(&mut self, tag_id: u64, tag_label: String, cx: &mut Context<Self>) {
        self.tag_id = tag_id;
        self.tag_label = tag_label;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.tag_id == 0 {
            self.trips.clear();
            return;
        }
        let store = self.store.clone();
        let tag_id = self.tag_id;
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let trips = store.list_trips_with_items(tag_id, cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.trips = trips;
                this._fetch = None;
                cx.notify();
            })
            .ok();
        }));
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

    fn toggle_activity(&mut self, activity: &str) {
        if let Some(index) = self.activities.iter().position(|a| a == activity) {
            self.activities.remove(index);
        } else {
            self.activities.push(activity.to_string());
            self.activities.sort();
        }
    }

    fn add_trip(&mut self, cx: &mut Context<Self>) {
        let Some(days) = self.selected_days(cx) else {
            return;
        };
        let tag_id = self.tag_id;
        let name = trip_name(&days, &self.activities);
        let activities = self.activities.clone();
        let store = self.store.clone();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = store.add_trip(tag_id, name, days, activities, cx).await {
                tracing::error!("failed to add trip: {e}");
            }
            let trips = store.list_trips_with_items(tag_id, cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.trips = trips;
                this.days = None;
                this.activities.clear();
                this._fetch = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn toggle_item(&mut self, task_id: u64, done: bool, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let tag_id = self.tag_id;
        self._fetch = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = store.toggle_task_done(task_id, done, cx).await {
                tracing::error!(?e, "failed to toggle trip item");
            }
            let trips = store.list_trips_with_items(tag_id, cx).await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.trips = trips;
                this._fetch = None;
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for TravelPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .overflow_y_scrollbar()
            .bg(rgb(APP_BG))
            .child(
                div()
                    .p_8()
                    .v_flex()
                    .gap_4()
                    .child(
                        div()
                            .text_2xl()
                            .font_bold()
                            .text_color(rgb(0xe5e5e5))
                            .child(self.tag_label.clone()),
                    )
                    .child(self.builder_card(window, cx))
                    .children(self.trips.iter().map(|view| self.trip_card(view, cx))),
            )
    }
}

impl TravelPanel {
    /// The trip builder: length chips (+ custom input for "N"), activity
    /// chips, and the "Add trip" button.
    fn builder_card(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let days = self.days.clone();
        let activities = self.activities.clone();
        let selected_days = self.selected_days(cx);
        let can_add = selected_days.is_some();

        let day_chips: Vec<gpui::AnyElement> = DAY_OPTIONS
            .iter()
            .map(|option| {
                let value = option.to_string();
                let selected = days.as_deref() == Some(option);
                chip(
                    format!("days-{option}"),
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
                    format!("activity-{option}"),
                    activity_label(option).to_string(),
                    selected,
                    cx.listener(move |this, _, _, _| {
                        this.toggle_activity(&value);
                    }),
                )
            })
            .collect();

        div()
            .border_1()
            .border_color(rgb(0x2e2e2e))
            .rounded_lg()
            .bg(rgb(0x232323))
            .p_4()
            .v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xe5e5e5))
                    .child("Add a trip"),
            )
            .child(
                div()
                    .v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .child("Trip length"),
                    )
                    .child(div().h_flex().items_center().gap_1p5().children(day_chips))
                    .when(days.as_deref() == Some("N"), |this| {
                        this.child(Input::new(&self.custom_days).with_size(Size::Small))
                    }),
            )
            .child(
                div()
                    .v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .child("Activities"),
                    )
                    .child(div().h_flex().items_center().gap_1p5().children(activity_chips)),
            )
            .child(
                Button::new("add-trip")
                    .compact()
                    .label("Add trip")
                    .disabled(!can_add)
                    .tooltip("Generate the pack and before-leaving checklists")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_trip(cx);
                    })),
            )
            .into_any_element()
    }

    fn trip_card(&self, view: &TripWithItems, cx: &mut Context<Self>) -> gpui::AnyElement {
        let sections: Vec<gpui::AnyElement> = view
            .sections
            .iter()
            .map(|section| {
                let items: Vec<gpui::AnyElement> = section
                    .items
                    .iter()
                    .map(|item| self.item_row(item, cx))
                    .collect();
                div()
                    .v_flex()
                    .gap_1()
                    .mt_2()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(0xcccccc))
                            .child(section.name.clone()),
                    )
                    .children(items)
                    .into_any_element()
            })
            .collect();
        div()
            .border_1()
            .border_color(rgb(0x2e2e2e))
            .rounded_lg()
            .bg(rgb(0x232323))
            .p_4()
            .v_flex()
            .gap_1()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .child(div().font_semibold().child(view.trip.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .child(format!("{} days", view.trip.days)),
                    ),
            )
            .children(sections)
            .into_any_element()
    }

    fn item_row(&self, item: &TaskWithMeta, cx: &mut Context<Self>) -> gpui::AnyElement {
        let task_id = item.task.id;
        let done = item.task.done;
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                Checkbox::new(("trip-item", task_id))
                    .with_size(px(18.))
                    .checked(done)
                    .on_click(cx.listener(move |this, new_done, _, cx| {
                        this.toggle_item(task_id, *new_done, cx);
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if done { rgb(0x6b6b6b) } else { rgb(0xd4d4d4) })
                    .child(item.task.title.clone()),
            )
            .into_any_element()
    }
}

/// Small toggle chip used for the day/activity pickers.
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
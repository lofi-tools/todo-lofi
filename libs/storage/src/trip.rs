//! Managed tags and trips.
//!
//! An automation can own a *managed tag*: a tag marked with the recipe id
//! that created it. Enabling the automation creates the tag; disabling
//! removes it and everything it owns.
//!
//! Today the only managed tag is travel checklists. Each trip is a
//! workflow run of the recipe (its params record the trip's name, days,
//! and activities); the checklist items are ordinary tasks carrying that
//! run's `workflow_run_id`, tagged with the managed tag's section tags —
//! "Pack" and "Before leaving" — so the existing task list renders a
//! trip's checklist sectioned like any tag with sections, and the run
//! auto-completes when the checklist is done.

use crate::{QueryResult, TodoStore};
use serde_json::json;
use snafu::ResultExt;

pub const PACK_SECTION: &str = "Pack";
pub const LEAVE_SECTION: &str = "Before leaving";
/// Sub-sections of Pack, one level only: display names carry the top
/// section plus the sub name after a slash, e.g. "Pack / food".
pub const PACK_SLEEP_SECTION: &str = "Pack / sleep & bathroom";
pub const PACK_TRAVEL_SECTION: &str = "Pack / travel";
pub const PACK_OUTDOORS_SECTION: &str = "Pack / outdoors";
pub const PACK_SWIM_SECTION: &str = "Pack / swim";
pub const PACK_WEDDING_SECTION: &str = "Pack / wedding";
pub const PACK_STAY_SECTION: &str = "Pack / stay";

/// The managed tag's checklist sections: (tag-key suffix, display name),
/// in display order (sub-sections nested under Pack). "Pack" itself
/// carries no items — every checklist item lives in a sub-section.
const SECTION_DEFS: [(&str, &str); 8] = [
    ("pack", PACK_SECTION),
    ("pack-sleep-bathroom", PACK_SLEEP_SECTION),
    ("pack-travel", PACK_TRAVEL_SECTION),
    ("pack-outdoors", PACK_OUTDOORS_SECTION),
    ("pack-swim", PACK_SWIM_SECTION),
    ("pack-wedding", PACK_WEDDING_SECTION),
    ("pack-stay", PACK_STAY_SECTION),
    ("before-leaving", LEAVE_SECTION),
];

fn add(items: &mut Vec<(String, String)>, section: &str, title: &str) {
    items.push((section.to_string(), title.to_string()));
}

/// Everyday clothes for the "Pack / stay" sub-section, scaled to the
/// trip length: one set per day, capped at a week's worth plus a laundry
/// plan for longer trips. `days` is the trip-length chip ("5", "30+",
/// or a custom number); anything unparseable falls back to 5.
fn stay_items(days: &str) -> Vec<String> {
    let parsed: u32 = days
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(5);
    let days = parsed.clamp(1, 30);
    // A week's worth is the most anyone packs; beyond that laundry
    // covers the rest.
    let sets = days.min(7);
    let mut items = vec![
        format!("Underwear × {sets}"),
        format!("Socks × {sets}"),
        format!("T-shirts and tops × {}", sets.min(5).max(2)),
    ];
    if days > 7 {
        items.push("Plan laundry mid-trip".to_string());
    }
    items
}

/// The checklist a trip generates: `(section, title)` pairs. Base items
/// apply to every trip; activities add their own; longer trips add
/// extras. Every Pack item lives in a sub-section — the top-level Pack
/// group carries no direct items.
pub fn travel_items(days: &str, activities: &[String]) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = Vec::new();
    add(&mut items, PACK_SLEEP_SECTION, "Toiletries");
    add(&mut items, PACK_SLEEP_SECTION, "Medication");
    add(&mut items, PACK_TRAVEL_SECTION, "Passport and ID");
    add(&mut items, PACK_TRAVEL_SECTION, "Phone and charger");

    for activity in activities {
        match activity.as_str() {
            "hiking" => {
                for title in [
                    "Hiking boots",
                    "Water bottle",
                    "Daypack",
                    "Rain jacket",
                    "Sunscreen",
                ] {
                    add(&mut items, PACK_OUTDOORS_SECTION, title);
                }
            }
            "swimming" => {
                for title in ["Swimsuit", "Beach towel", "Goggles", "Flip-flops"] {
                    add(&mut items, PACK_SWIM_SECTION, title);
                }
            }
            "wedding" => {
                for title in ["Formal outfit", "Dress shoes", "Wedding gift"] {
                    add(&mut items, PACK_WEDDING_SECTION, title);
                }
            }
            "camping" => {
                for title in ["Tent", "Sleeping bag", "Headlamp", "First aid kit"] {
                    add(&mut items, PACK_OUTDOORS_SECTION, title);
                }
            }
            _ => {}
        }
    }
    if matches!(days, "13" | "30+") {
        add(&mut items, PACK_STAY_SECTION, "Extra set of clothes");
    }
    if days == "30+" {
        add(&mut items, PACK_STAY_SECTION, "Laundry bag");
        add(&mut items, PACK_TRAVEL_SECTION, "Travel adapter");
    }

    add(&mut items, PACK_SLEEP_SECTION, "Travel pillow");
    add(&mut items, PACK_SLEEP_SECTION, "Sleep mask and earplugs");
    add(&mut items, PACK_SLEEP_SECTION, "Travel towel");

    add(&mut items, PACK_TRAVEL_SECTION, "Offline copies of tickets");
    add(&mut items, PACK_TRAVEL_SECTION, "Power bank");
    add(&mut items, PACK_TRAVEL_SECTION, "Reusable water bottle");

    if activities.iter().any(|a| a == "hiking" || a == "camping") {
        add(&mut items, PACK_OUTDOORS_SECTION, "Bug spray");
        add(&mut items, PACK_OUTDOORS_SECTION, "Trail snacks");
    }
    if activities.iter().any(|a| a == "wedding") {
        add(&mut items, PACK_WEDDING_SECTION, "Wrinkle-release spray");
        add(&mut items, PACK_WEDDING_SECTION, "Mini sewing kit");
    }
    for title in stay_items(days) {
        add(&mut items, PACK_STAY_SECTION, &title);
    }

    add(&mut items, LEAVE_SECTION, "Confirm bookings (flights, accommodation)");
    add(&mut items, LEAVE_SECTION, "Arrange house care (pets, plants)");
    add(&mut items, LEAVE_SECTION, "Lock up and set the alarm");

    for activity in activities {
        match activity.as_str() {
            "hiking" => {
                for title in ["Check trail conditions and weather", "Download offline maps"] {
                    add(&mut items, LEAVE_SECTION, title);
                }
            }
            "swimming" => add(&mut items, LEAVE_SECTION, "Check beach or pool hours"),
            "wedding" => {
                for title in ["Confirm RSVP", "Check the dress code"] {
                    add(&mut items, LEAVE_SECTION, title);
                }
            }
            "camping" => {
                for title in ["Reserve the campsite", "Check tent and gear"] {
                    add(&mut items, LEAVE_SECTION, title);
                }
            }
            _ => {}
        }
    }
    if days == "30+" {
        add(&mut items, LEAVE_SECTION, "Pause mail and subscriptions");
    }
    items
}

/// Section tag name under the managed tag, e.g. `managed:packing-list:pack`.
fn section_tag_name(managed_tag_name: &str, section_key: &str) -> String {
    format!("{managed_tag_name}:{section_key}")
}

/// Tag-key suffix for a checklist section ("Pack / stay" ->
/// "pack-stay").
fn section_key(section: &str) -> &'static str {
    match section {
        "Before leaving" => "before-leaving",
        "Pack / sleep & bathroom" => "pack-sleep-bathroom",
        "Pack / travel" => "pack-travel",
        "Pack / outdoors" => "pack-outdoors",
        "Pack / swim" => "pack-swim",
        "Pack / wedding" => "pack-wedding",
        "Pack / stay" => "pack-stay",
        _ => "pack",
    }
}

impl TodoStore {
    /// The recipe that manages `tag_id` through an app binding, if any.
    pub async fn managed_recipe_for_tag(&mut self, tag_id: u64) -> QueryResult<Option<u64>> {
        for binding in self.bindings_for_tag(tag_id).await? {
            if let Some(recipe_id) = self.recipe_for_app(binding.app_id).await? {
                return Ok(Some(recipe_id));
            }
        }
        Ok(None)
    }

    /// One tag managed by `recipe_id` through its app, if any.
    async fn tag_managed_by_recipe(&mut self, recipe_id: u64) -> QueryResult<Option<crate::Tag>> {
        let Some(app) = self.app_for_recipe(recipe_id).await? else {
            return Ok(None);
        };
        for binding in self.bindings_for_app(app.id).await? {
            return Ok(Some(self.get_tag(binding.tag_id).await?));
        }
        Ok(None)
    }

    /// Enable a managed-tag automation: create the tag it owns (marked
    /// with the recipe id) plus its checklist section tags, idempotently.
    /// No run is created — trips are built from the Automations panel.
    pub async fn enable_managed_recipe(&mut self, recipe_id: u64) -> QueryResult<crate::Tag> {
        let recipe_row = self.get_recipe(recipe_id).await?;
        let parsed = crate::workflow::parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| crate::QueryErr::UnexpectedValue { message })?;
        let tag_name = parsed
            .managed_tag
            .clone()
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: "recipe does not manage a tag".to_string(),
            })?;
        let tag = if let Some(tag) = self.get_tag_by_name(&tag_name).await? {
            tag
        } else {
            self.create_tag_with_display_name(tag_name.clone(), Some(parsed.name.clone()))
                .await?
        };
        // Register the app and attach it *partially*: the tag stays the
        // user's, and the app owns only the sections it creates.
        let app = self
            .upsert_app(
                "recipe",
                &recipe_row.slug,
                &parsed.name,
                parsed.description.clone(),
            )
            .await?;
        self.set_recipe_app(recipe_id, app.id).await?;
        self.set_app_enabled(app.id, true).await?;
        // Idempotent: give the tag the recipe's display name, even when it
        // pre-existed without one.
        toasty::sql::statement(r#"UPDATE tags SET display_name = ?1 WHERE id = ?2"#)
            .bind(parsed.name.as_str())
            .bind(tag.id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "name managed tag",
            })?;
        self.attach_app_to_tag(
            app.id,
            tag.id,
            crate::managed::BindingRole::Partial,
            true,
        )
        .await?;
        self.ensure_recipe_sections(recipe_id, tag.id).await?;
        Ok(tag)
    }

    /// Create the recipe's checklist sections inside `tag_id`, owned by the
    /// recipe's app. Each section is both a child tag (so tasks attach to
    /// it) and a `tag_sections` row (so the task list groups by it).
    /// Idempotent, so attaching an existing user tag provisions its
    /// sections the same way enabling the automation does.
    pub async fn ensure_recipe_sections(
        &mut self,
        recipe_id: u64,
        tag_id: u64,
    ) -> QueryResult<()> {
        let Some(app) = self.app_for_recipe(recipe_id).await? else {
            return Ok(());
        };
        // Only recipes that declare a managed tag have checklist sections;
        // attaching a plain automation to a tag provisions nothing.
        let recipe_row = self.get_recipe(recipe_id).await?;
        let parsed = crate::workflow::parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| crate::QueryErr::UnexpectedValue { message })?;
        if parsed.managed_tag.is_none() {
            return Ok(());
        }
        let tag = self.get_tag(tag_id).await?;
        for (key, display) in SECTION_DEFS {
            let section_name = section_tag_name(&tag.name, key);
            if self.get_tag_by_name(&section_name).await?.is_none() {
                let section = self
                    .create_tag_with_display_name(section_name, Some(display.to_string()))
                    .await?;
                self.add_tag_implication(section.id, tag.id).await?;
            }
            let section = self.add_tag_section(tag_id, display.to_string()).await?;
            self.set_section_managed(section.id, Some(app.id), false)
                .await?;
        }
        Ok(())
    }

    /// Disable a managed-tag automation's app: tombstone its open items,
    /// downgrade or remove its sections, and delete its runs. The tag
    /// itself survives as an ordinary user tag.
    pub async fn remove_managed_tag_content(&mut self, recipe_id: u64) -> QueryResult<()> {
        if let Some(app) = self.app_for_recipe(recipe_id).await? {
            self.disable_app(app.id, true).await?;
        }
        let run_ids: Vec<u64> = self
            .list_workflow_runs()
            .await?
            .into_iter()
            .filter(|run| run.recipe_id == recipe_id)
            .map(|run| run.id)
            .collect();
        for run_id in run_ids {
            crate::WorkflowRun::delete_by_id(&mut self.db, run_id)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "delete trip run",
                })?;
        }
        Ok(())
    }

    /// Create a trip of a managed-tag recipe inside `tag_id`: replaces the
    /// app's own unfinished items there, then starts a workflow run whose
    /// params record the trip (name, days, activities) and spawns the
    /// checklist items as ordinary tasks carrying the run's id, tagged with
    /// the recipe's section tags of that tag.
    pub async fn create_trip(
        &mut self,
        recipe_id: u64,
        tag_id: u64,
        name: String,
        days: String,
        activities: Vec<String>,
    ) -> QueryResult<crate::WorkflowRun> {
        let tag = self.get_tag(tag_id).await?;
        let app = self
            .app_for_recipe(recipe_id)
            .await?
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: "recipe does not manage a tag".to_string(),
            })?;
        let bound = self
            .bindings_for_tag(tag_id)
            .await?
            .iter()
            .any(|binding| binding.app_id == app.id);
        if !bound {
            return Err(crate::QueryErr::UnexpectedValue {
                message: format!("tag '{}' is not managed by this recipe", tag.label()),
            });
        }
        // Regeneration: the app's own unfinished, unmodified items in this
        // tag are discarded and recreated below. User tasks and completed
        // items are untouched.
        for stale in self.managed_tasks_in_tag(tag_id, app.id).await? {
            self.tombstone_task(stale).await?;
        }
        self.ensure_recipe_sections(recipe_id, tag_id).await?;
        let mut activities = activities;
        activities.sort();
        activities.dedup();
        let activities_json = serde_json::to_string(&activities).unwrap_or_else(|_| "[]".to_string());
        let run = self
            .create_managed_run(
                recipe_id,
                json!({
                    "name": name,
                    "days": days.clone(),
                    // Param schemas only support scalars; the activity list
                    // travels as a JSON string.
                    "activities": activities_json,
                }),
            )
            .await?;
        let generated = travel_items(&days, &activities);
        for (section, title) in generated {
            let task = self
                .create_task(
                    crate::Task::create()
                        .title(title)
                        .workflow_run_id(Some(run.id)),
                )
                .await?;
            let tag_name = section_tag_name(&tag.name, section_key(&section));
            self.assign_tag_to_task(task.id, &tag_name).await?;
            self.set_task_managed(
                task.id,
                app.id,
                crate::managed::ManagedMode::Managed,
                false,
            )
            .await?;
        }
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_travel_recipe(store: &mut TodoStore) -> u64 {
        store
            .create_recipe(
                "travel",
                serde_json::json!({
                    "name": "Travel checklists",
                    "managed_tag": "managed:packing-list",
                    "params": {
                        "name": { "type": "string", "default": "" },
                        "days": { "type": "string", "default": "5" },
                        "activities": { "type": "string", "default": "" }
                    },
                    "nodes": [{ "id": "trip", "kind": "action", "title": "Trip checklist" }],
                    "edges": []
                }),
            )
            .await
            .expect("recipe should validate")
            .id
    }

    #[test]
    fn test_travel_items_base() {
        let items = travel_items("5", &[]);
        let by_section = |section: &str| -> Vec<&str> {
            items
                .iter()
                .filter(|(s, _)| s == section)
                .map(|(_, title)| title.as_str())
                .collect()
        };
        // No direct Pack items: everything lives in a sub-section.
        assert!(by_section(PACK_SECTION).is_empty());
        assert!(by_section(PACK_SLEEP_SECTION).contains(&"Toiletries"));
        assert!(by_section(PACK_SLEEP_SECTION).contains(&"Medication"));
        assert!(by_section(PACK_TRAVEL_SECTION).contains(&"Passport and ID"));
        assert!(by_section(PACK_TRAVEL_SECTION).contains(&"Phone and charger"));
        let leave = by_section(LEAVE_SECTION);
        assert!(leave.contains(&"Confirm bookings (flights, accommodation)"));
    }

    #[test]
    fn test_travel_items_no_direct_pack_items_with_activities() {
        // Even with every activity and long-trip extras, nothing lands
        // directly in Pack.
        let items = travel_items(
            "30+",
            &["hiking", "swimming", "wedding", "camping"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        );
        assert!(
            items.iter().all(|(section, _)| section != PACK_SECTION),
            "every Pack item must be in a sub-section"
        );
        let titles: Vec<(&str, &str)> = items
            .iter()
            .map(|(s, t)| (s.as_str(), t.as_str()))
            .collect();
        assert!(titles.contains(&("Pack / outdoors", "Hiking boots")));
        assert!(titles.contains(&("Pack / outdoors", "Tent")));
        assert!(titles.contains(&("Pack / swim", "Swimsuit")));
        assert!(titles.contains(&("Pack / wedding", "Wedding gift")));
        assert!(titles.contains(&("Pack / stay", "Laundry bag")));
        assert!(titles.contains(&("Pack / travel", "Travel adapter")));
    }

    #[test]
    fn test_travel_items_activities_add_items() {
        let items = travel_items("3", &["hiking".to_string(), "camping".to_string()]);
        let titles: Vec<&str> = items.iter().map(|(_, title)| title.as_str()).collect();
        for expected in [
            "Hiking boots",
            "Water bottle",
            "Tent",
            "Sleeping bag",
            "Check trail conditions and weather",
            "Reserve the campsite",
        ] {
            assert!(titles.contains(&expected), "missing {expected}");
        }
        assert!(!titles.contains(&"Wedding gift"));
    }

    #[test]
    fn test_travel_items_subsections() {
        // Always-packed sub-sections land under Pack with slash names.
        let items = travel_items("5", &[]);
        let titles: Vec<(&str, &str)> = items
            .iter()
            .map(|(section, title)| (section.as_str(), title.as_str()))
            .collect();
        assert!(titles.contains(&("Pack / sleep & bathroom", "Travel pillow")));
        assert!(titles.contains(&("Pack / travel", "Power bank")));
        assert!(titles.contains(&("Pack / stay", "Underwear × 5")));
        assert!(titles.contains(&("Pack / stay", "Socks × 5")));
        // Optional sub-sections only appear with their activity.
        assert!(!titles.iter().any(|(s, _)| *s == "Pack / outdoors"));
        assert!(!titles.iter().any(|(s, _)| *s == "Pack / wedding"));

        let hike = travel_items("3", &["hiking".to_string()]);
        let hike_sections: Vec<&str> =
            hike.iter().map(|(s, _)| s.as_str()).collect();
        assert!(hike_sections.contains(&"Pack / outdoors"));
        assert!(!hike_sections.contains(&"Pack / wedding"));

        let wedding = travel_items("2", &["wedding".to_string()]);
        let wedding_sections: Vec<&str> =
            wedding.iter().map(|(s, _)| s.as_str()).collect();
        assert!(wedding_sections.contains(&"Pack / wedding"));
        assert!(!wedding_sections.contains(&"Pack / outdoors"));
    }

    #[test]
    fn test_stay_items_scale_with_days() {
        assert!(stay_items("1").contains(&"Underwear × 1".to_string()));
        assert!(stay_items("5").contains(&"Socks × 5".to_string()));
        // Capped at a week, with a laundry plan beyond it.
        let long = stay_items("30+");
        assert!(long.contains(&"Underwear × 7".to_string()));
        assert!(long.contains(&"Plan laundry mid-trip".to_string()));
        let short = stay_items("3");
        assert!(!short.contains(&"Plan laundry mid-trip".to_string()));
    }

    #[test]
    fn test_travel_items_long_trips_add_extras() {
        let long = travel_items("30+", &[]);
        let long_titles: Vec<&str> = long.iter().map(|(_, t)| t.as_str()).collect();
        assert!(long_titles.contains(&"Laundry bag"));
        assert!(long_titles.contains(&"Pause mail and subscriptions"));

        let day = travel_items("1", &[]);
        let day_titles: Vec<&str> = day.iter().map(|(_, t)| t.as_str()).collect();
        assert!(!day_titles.contains(&"Laundry bag"));
    }

    #[tokio::test]
    async fn test_create_trip_in_an_existing_user_tag() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let recipe_id = make_travel_recipe(&mut store).await;
        // The user already keeps a travel list with a task of their own.
        let tag = store.create_tag("Travel").await?;
        let mine = store
            .create_task(crate::Task::create().title("Book dog sitter"))
            .await?;
        store.assign_tag_to_task(mine.id, "Travel").await?;

        let app = store
            .upsert_app("recipe", "packing-list", "Travel checklists", None)
            .await?;
        store.set_recipe_app(recipe_id, app.id).await?;
        store
            .attach_app_to_tag(
                app.id,
                tag.id,
                crate::managed::BindingRole::Partial,
                false,
            )
            .await?;
        store.ensure_recipe_sections(recipe_id, tag.id).await?;
        // The sections land under the user's tag, not the recipe's own.
        assert!(
            store
                .get_children(tag.id)
                .await?
                .iter()
                .any(|child| child.display_name.as_deref() == Some(PACK_TRAVEL_SECTION))
        );

        let first = store
            .create_trip(
                recipe_id,
                tag.id,
                "Costa Rica".to_string(),
                "5".to_string(),
                Vec::new(),
            )
            .await?;
        let items = store.list_tasks_by_tag(tag.id).await?;
        assert!(
            items.iter().any(|t| t.task.id == mine.id),
            "the user's own task is untouched"
        );
        assert!(
            items.iter().any(|t| t.workflow_run_id == Some(first.id)),
            "the trip's checklist is generated into the tag"
        );

        // A second trip replaces the first checklist but spares the user's
        // task.
        let second = store
            .create_trip(
                recipe_id,
                tag.id,
                "Iceland".to_string(),
                "8".to_string(),
                Vec::new(),
            )
            .await?;
        let items = store.list_tasks_by_tag(tag.id).await?;
        assert!(items.iter().any(|t| t.task.id == mine.id));
        assert!(
            items.iter().all(|t| t.workflow_run_id != Some(first.id)),
            "the previous checklist is replaced"
        );
        assert!(items.iter().any(|t| t.workflow_run_id == Some(second.id)));
        Ok(())
    }

    #[tokio::test]
    async fn test_managed_tag_lifecycle() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let recipe_id = make_travel_recipe(&mut store).await;

        let tag = store.enable_managed_recipe(recipe_id).await?;
        assert_eq!(tag.name, "managed:packing-list");
        assert_eq!(tag.label(), "Travel checklists");
        // Idempotent: enabling twice keeps the same tag and sections.
        let again = store.enable_managed_recipe(recipe_id).await?;
        assert_eq!(again.id, tag.id);
        assert_eq!(store.list_tags().await?.len(), 9); // managed + 8 sections

        // Section children exist, ordered for display (sub-sections nested
        // under Pack).
        let children = store.get_children(tag.id).await?;
        assert_eq!(children.len(), 8);
        let sections = store.tag_sections(tag.id).await?;
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Pack",
                "Pack / sleep & bathroom",
                "Pack / travel",
                "Pack / outdoors",
                "Pack / swim",
                "Pack / wedding",
                "Pack / stay",
                "Before leaving",
            ]
        );

        // A trip is a workflow run: items carry its run id and land in the
        // section tags, so the task list groups them like any tag.
        let run = store
            .create_trip(
                recipe_id,
                tag.id,
                "Hiking · 5 days".to_string(),
                "5".to_string(),
                vec!["hiking".to_string()],
            )
            .await?;
        assert_eq!(run.params.0["days"], "5");
        let items = store.list_tasks_by_tag(tag.id).await?;
        assert!(items.len() >= 8);
        assert!(items.iter().all(|t| t.workflow_run_id == Some(run.id)));
        assert!(items.iter().all(|t| t.direct_tags.len() == 1));

        // Sectioned display: every Pack item under its slash
        // sub-section, leave items under Before leaving, in the tag's
        // section order. The bare Pack group carries no direct items.
        let ids: Vec<u64> = items.iter().map(|t| t.task.id).collect();
        let (order, map) = store.section_groups_for_tasks(tag.id, &ids).await?;
        assert_eq!(
            order,
            vec![
                "Pack / sleep & bathroom".to_string(),
                "Pack / travel".to_string(),
                "Pack / outdoors".to_string(),
                "Pack / stay".to_string(),
                "Before leaving".to_string(),
            ]
        );
        assert!(
            ids.iter()
                .all(|id| map.get(id).map(String::as_str) != Some("Pack")),
            "no item may sit directly in Pack"
        );
        let sleep_ids: Vec<u64> = ids
            .iter()
            .filter(|id| {
                map.get(id).map(String::as_str) == Some("Pack / sleep & bathroom")
            })
            .copied()
            .collect();
        assert!(sleep_ids.len() >= 2);

        // Completing every item completes the trip's run.
        for id in &ids {
            store.update_task_done(*id, true).await?;
        }
        assert_eq!(store.find_run(run.id).await?.unwrap().status, "completed");

        // Disabling removes the run and the app's ownership, but the tag
        // survives as the user's and completed items are kept: every item
        // was completed above, so every used section survives too.
        store.remove_managed_tag_content(recipe_id).await?;
        let tag = store
            .get_tag_by_name("managed:packing-list")
            .await?
            .expect("the tag is the user's and survives");
        assert_eq!(store.tag_owner(tag.id).await?, None);
        assert!(store.bindings_for_tag(tag.id).await?.is_empty());
        assert!(store.list_workflow_runs().await?.is_empty());
        // Main tag plus the five sections that held completed items; the
        // empty sections (Pack, Pack / swim, Pack / wedding) are gone.
        assert_eq!(store.list_tags().await?.len(), 6);
        // Completed items survive as ordinary tasks.
        assert!(
            store
                .list_tasks()
                .await?
                .iter()
                .all(|t| t.deleted_at.is_none())
        );
        Ok(())
    }
}
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
pub const PACK_WEDDING_SECTION: &str = "Pack / wedding";
pub const PACK_STAY_SECTION: &str = "Pack / stay";

/// The managed tag's checklist sections: (tag-key suffix, display name),
/// in display order (sub-sections nested under Pack).
const SECTION_DEFS: [(&str, &str); 7] = [
    ("pack", PACK_SECTION),
    ("pack-sleep-bathroom", PACK_SLEEP_SECTION),
    ("pack-travel", PACK_TRAVEL_SECTION),
    ("pack-outdoors", PACK_OUTDOORS_SECTION),
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
/// apply to every trip; activities add their own; longer trips add extras.
pub fn travel_items(days: &str, activities: &[String]) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = Vec::new();
    add(&mut items, PACK_SECTION, "Passport and ID");
    add(&mut items, PACK_SECTION, "Phone and charger");
    add(&mut items, PACK_SECTION, "Toiletries");
    add(&mut items, PACK_SECTION, "Medication");

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
                    add(&mut items, PACK_SECTION, title);
                }
            }
            "swimming" => {
                for title in ["Swimsuit", "Beach towel", "Goggles", "Flip-flops"] {
                    add(&mut items, PACK_SECTION, title);
                }
            }
            "wedding" => {
                for title in ["Formal outfit", "Dress shoes", "Wedding gift"] {
                    add(&mut items, PACK_SECTION, title);
                }
            }
            "camping" => {
                for title in ["Tent", "Sleeping bag", "Headlamp", "First aid kit"] {
                    add(&mut items, PACK_SECTION, title);
                }
            }
            _ => {}
        }
    }
    if matches!(days, "13" | "30+") {
        add(&mut items, PACK_SECTION, "Extra set of clothes");
    }
    if days == "30+" {
        add(&mut items, PACK_SECTION, "Laundry bag");
        add(&mut items, PACK_SECTION, "Travel adapter");
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
        "Pack / wedding" => "pack-wedding",
        "Pack / stay" => "pack-stay",
        _ => "pack",
    }
}

impl TodoStore {
    /// The recipe that owns `tag_id`, if the tag is a managed tag.
    pub async fn managed_recipe_for_tag(&mut self, tag_id: u64) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(
            r#"SELECT managed_by_recipe_id FROM tags WHERE id = ?1 LIMIT 1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "recipe for managed tag",
        })?;
        Ok(rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        }))
    }

    /// The tag owned by `recipe_id`, if any.
    async fn tag_managed_by_recipe(&mut self, recipe_id: u64) -> QueryResult<Option<crate::Tag>> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tags WHERE managed_by_recipe_id = ?1 LIMIT 1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(recipe_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "managed tag for recipe",
        })?;
        let Some(id) = rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        }) else {
            return Ok(None);
        };
        Ok(Some(self.get_tag(id).await?))
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
        // Idempotent: mark the tag as managed and give it the recipe's
        // display name, even when it pre-existed without one.
        toasty::sql::statement(
            r#"UPDATE tags SET managed_by_recipe_id = ?1, display_name = ?2 WHERE id = ?3"#,
        )
        .bind(recipe_id as i64)
        .bind(parsed.name.as_str())
        .bind(tag.id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "mark managed tag",
        })?;
        // Checklist sections: child tags under the managed tag, ordered
        // via `tag_sections` so the task list groups items under them.
        for (key, display) in SECTION_DEFS {
            let section_name = section_tag_name(&tag_name, key);
            if self.get_tag_by_name(&section_name).await?.is_none() {
                let section = self
                    .create_tag_with_display_name(section_name.clone(), Some(display.to_string()))
                    .await?;
                self.add_tag_implication(section.id, tag.id).await?;
            }
            self.add_tag_section(tag.id, display.to_string()).await?;
        }
        Ok(tag)
    }

    /// Remove the managed tag of `recipe_id` and everything it owns: all
    /// of the recipe's runs (their step tasks are tombstoned first) and
    /// the tag itself with its section tags.
    pub async fn remove_managed_tag_content(&mut self, recipe_id: u64) -> QueryResult<()> {
        let run_ids: Vec<u64> = self
            .list_workflow_runs()
            .await?
            .into_iter()
            .filter(|run| run.recipe_id == recipe_id)
            .map(|run| run.id)
            .collect();
        for run_id in run_ids {
            let rows = toasty::sql::query(
                r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND deleted_at IS NULL"#,
            )
            .column_types([toasty::stmt::Type::I64])
            .bind(run_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list trip items to remove",
            })?;
            for row in rows {
                if let Some(id) = match &row {
                    toasty::stmt::Value::Record(record) => {
                        record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
                    }
                    _ => None,
                } {
                    self.tombstone_task(id).await?;
                }
            }
            crate::WorkflowRun::delete_by_id(&mut self.db, run_id)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "delete trip run",
                })?;
        }
        let Some(tag) = self.tag_managed_by_recipe(recipe_id).await? else {
            return Ok(());
        };
        for child in self.get_children(tag.id).await? {
            self.remove_tag_implication(child.id, tag.id).await?;
            self.delete_tag(child.id).await?;
        }
        for section in self.tag_sections(tag.id).await? {
            self.remove_tag_section(section.id).await?;
        }
        self.delete_tag(tag.id).await?;
        Ok(())
    }

    /// Create a trip of a managed-tag recipe: starts a workflow run whose
    /// params record the trip (name, days, activities), then spawns the
    /// checklist items as ordinary tasks carrying the run's id, tagged
    /// with the recipe's section tags.
    pub async fn create_trip(
        &mut self,
        recipe_id: u64,
        name: String,
        days: String,
        activities: Vec<String>,
    ) -> QueryResult<crate::WorkflowRun> {
        let recipe_row = self.get_recipe(recipe_id).await?;
        let parsed = crate::workflow::parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| crate::QueryErr::UnexpectedValue { message })?;
        let managed_tag_name = parsed
            .managed_tag
            .clone()
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: "recipe does not manage a tag".to_string(),
            })?;
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
            let tag_name = section_tag_name(&managed_tag_name, section_key(&section));
            self.assign_tag_to_task(task.id, &tag_name).await?;
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
        let pack: Vec<&str> = items
            .iter()
            .filter(|(section, _)| section == PACK_SECTION)
            .map(|(_, title)| title.as_str())
            .collect();
        assert!(pack.contains(&"Passport and ID"));
        assert!(pack.contains(&"Toiletries"));
        assert!(!pack.contains(&"Tent"));
        let leave: Vec<&str> = items
            .iter()
            .filter(|(section, _)| section == LEAVE_SECTION)
            .map(|(_, title)| title.as_str())
            .collect();
        assert!(leave.contains(&"Confirm bookings (flights, accommodation)"));
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
    async fn test_managed_tag_lifecycle() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let recipe_id = make_travel_recipe(&mut store).await;

        let tag = store.enable_managed_recipe(recipe_id).await?;
        assert_eq!(tag.name, "managed:packing-list");
        assert_eq!(tag.label(), "Travel checklists");
        // Idempotent: enabling twice keeps the same tag and sections.
        let again = store.enable_managed_recipe(recipe_id).await?;
        assert_eq!(again.id, tag.id);
        assert_eq!(store.list_tags().await?.len(), 8); // managed + 7 sections

        // Section children exist, ordered for display (sub-sections nested
        // under Pack).
        let children = store.get_children(tag.id).await?;
        assert_eq!(children.len(), 7);
        let sections = store.tag_sections(tag.id).await?;
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Pack",
                "Pack / sleep & bathroom",
                "Pack / travel",
                "Pack / outdoors",
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

        // Sectioned display: Pack items under Pack, sub-section items
        // under their slash group, leave items under Before leaving, in
        // the tag's section order.
        let ids: Vec<u64> = items.iter().map(|t| t.task.id).collect();
        let (order, map) = store.section_groups_for_tasks(tag.id, &ids).await?;
        assert_eq!(
            order,
            vec![
                "Pack".to_string(),
                "Pack / sleep & bathroom".to_string(),
                "Pack / travel".to_string(),
                "Pack / outdoors".to_string(),
                "Pack / stay".to_string(),
                "Before leaving".to_string(),
            ]
        );
        let pack_ids: Vec<u64> = ids
            .iter()
            .filter(|id| map.get(id).map(String::as_str) == Some("Pack"))
            .copied()
            .collect();
        assert!(pack_ids.len() >= 4);

        // Completing every item completes the trip's run.
        for id in &ids {
            store.update_task_done(*id, true).await?;
        }
        assert_eq!(store.find_run(run.id).await?.unwrap().status, "completed");

        // Disabling removes the tag, its sections, and the run.
        store.remove_managed_tag_content(recipe_id).await?;
        assert!(store.get_tag_by_name("managed:packing-list").await?.is_none());
        assert!(store.list_workflow_runs().await?.is_empty());
        assert_eq!(store.list_tags().await?.len(), 0);
        // Items are tombstoned, not hard-deleted.
        assert!(store.list_tasks().await?.iter().all(|t| t.deleted_at.is_some()));
        Ok(())
    }
}
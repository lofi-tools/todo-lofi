//! Managed tags and trips.
//!
//! An automation can own a *managed tag*: a tag marked with the recipe id
//! that created it. Enabling the automation creates the tag (and its
//! special panel in the UI); disabling removes it and everything it owns.
//!
//! Today the only managed tag is travel checklists. Trips live in the
//! `trips` table under the managed tag; each trip's checklist items are
//! ordinary `tasks` rows carrying `trip_id` + `trip_section` (the
//! checklist: "Pack" or "Before leaving"), tagged with the managed tag so
//! they show up in the task list too. Trip items never sync.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;
use toasty::Model;

pub const PACK_SECTION: &str = "Pack";
pub const LEAVE_SECTION: &str = "Before leaving";

#[derive(Debug, Clone, Model)]
pub struct Trip {
    #[key]
    #[auto]
    pub id: u64,
    pub tag_id: u64,
    /// Display name, e.g. "Hiking · Swimming — 5 days".
    pub name: String,
    /// Trip length as chosen in the builder: "1".."13", "30+", or "N"
    /// (custom).
    pub days: String,
    pub activities: toasty::Json<Vec<String>>,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
}

/// One checklist section of a trip with its item tasks.
#[derive(Debug, Clone)]
pub struct TripSection {
    pub name: String,
    pub items: Vec<crate::TaskWithMeta>,
}

/// A trip plus its checklist sections, for the travel panel.
#[derive(Debug, Clone)]
pub struct TripWithItems {
    pub trip: Trip,
    pub sections: Vec<TripSection>,
}

/// The checklist a trip generates: `(section, title)` pairs. Base items
/// apply to every trip; activities add their own; longer trips add extras.
fn add(items: &mut Vec<(String, String)>, section: &str, title: &str) {
    items.push((section.to_string(), title.to_string()));
}

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

/// First column of a raw SQL row as an id, if the row is a record.
fn first_id(row: &toasty::stmt::Value) -> Option<u64> {
    match row {
        toasty::stmt::Value::Record(record) => {
            record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
        }
        _ => None,
    }
}

fn parse_trip_row(record: &toasty::stmt::Value) -> Option<Trip> {
    let toasty::stmt::Value::Record(record) = record else {
        return None;
    };
    let id = record.first().and_then(|v| v.to_i64())? as u64;
    let tag_id = record.get(1).and_then(|v| v.to_i64())? as u64;
    let name = record.get(2).and_then(|v| v.as_str())?.to_string();
    let days = record.get(3).and_then(|v| v.as_str())?.to_string();
    let activities = record
        .get(4)
        .and_then(|v| v.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    let created_at = record
        .get(5)
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.parse::<jiff::Timestamp>().ok())?;
    Some(Trip {
        id,
        tag_id,
        name,
        days,
        activities: toasty::Json(activities),
        created_at,
    })
}

impl TodoStore {
    /// The recipe that manages `tag_id`, if the tag is owned by an
    /// automation.
    pub async fn managed_recipe_for_tag(&mut self, tag_id: u64) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(
            r#"SELECT managed_by_recipe_id FROM tags WHERE id = ?1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "managed recipe for tag",
        })?;
        Ok(rows
            .first()
            .and_then(first_id)
            .filter(|id| *id > 0))
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
        let Some(id) = rows.first().and_then(first_id) else {
            return Ok(None);
        };
        Ok(Some(self.get_tag(id).await?))
    }

    /// Enable a managed-tag automation: create the tag it owns (marked
    /// with the recipe id) if it does not exist yet. No run is created —
    /// the tag's panel generates content on demand.
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
        if let Some(tag) = self.get_tag_by_name(&tag_name).await? {
            return Ok(tag);
        }
        let tag = self
            .create_tag_with_display_name(tag_name.clone(), Some(parsed.name.clone()))
            .await?;
        toasty::sql::statement(
            r#"UPDATE tags SET managed_by_recipe_id = ?1 WHERE id = ?2"#,
        )
        .bind(recipe_id as i64)
        .bind(tag.id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "mark managed tag",
        })?;
        Ok(tag)
    }

    /// Remove the managed tag of `recipe_id` and everything it owns:
    /// trip item tasks are tombstoned, trips deleted, the tag dropped.
    pub async fn remove_managed_tag_content(&mut self, recipe_id: u64) -> QueryResult<()> {
        let Some(tag) = self.tag_managed_by_recipe(recipe_id).await? else {
            return Ok(());
        };
        let trips = self.list_trips(tag.id).await?;
        for trip in trips {
            let rows = toasty::sql::query(
                r#"SELECT id FROM tasks WHERE trip_id = ?1 AND deleted_at IS NULL"#,
            )
            .column_types([toasty::stmt::Type::I64])
            .bind(trip.id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list trip items to remove",
            })?;
            for row in rows {
                if let Some(id) = first_id(&row) {
                    self.tombstone_task(id).await?;
                }
            }
            toasty::sql::statement(r#"DELETE FROM trips WHERE id = ?1"#)
                .bind(trip.id as i64)
                .exec(&mut self.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "delete trip",
                })?;
        }
        self.delete_tag(tag.id).await?;
        Ok(())
    }

    /// Trips under the managed tag, oldest first.
    pub async fn list_trips(&mut self, tag_id: u64) -> QueryResult<Vec<Trip>> {
        let rows = toasty::sql::query(
            r#"SELECT id, tag_id, name, days, activities, created_at
               FROM trips WHERE tag_id = ?1 ORDER BY created_at, id"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list trips",
        })?;
        Ok(rows.iter().filter_map(parse_trip_row).collect())
    }

    /// Create a trip under the managed tag and spawn its checklist items
    /// as tasks (tagged with the managed tag, carrying `trip_id` and
    /// `trip_section`).
    pub async fn create_trip(
        &mut self,
        tag_id: u64,
        name: String,
        days: String,
        activities: Vec<String>,
    ) -> QueryResult<Trip> {
        let tag = self.get_tag(tag_id).await?;
        let mut activities = activities;
        activities.sort();
        activities.dedup();
        let trip = Trip::create()
            .tag_id(tag_id)
            .name(name.clone())
            .days(days.clone())
            .activities(toasty::Json(activities.clone()))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "create trip",
            })?;
        for (section, title) in travel_items(&days, &activities) {
            let task = self
                .create_task(
                    crate::Task::create()
                        .title(title)
                        .trip_id(Some(trip.id))
                        .trip_section(Some(section)),
                )
                .await?;
            self.assign_tag_to_task(task.id, &tag.name).await?;
        }
        Ok(trip)
    }

    /// Item tasks of one trip's checklist section, in creation order.
    async fn trip_items_in_section(
        &mut self,
        trip_id: u64,
        section: &str,
    ) -> QueryResult<Vec<crate::TaskWithMeta>> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE trip_id = ?1 AND trip_section = ?2
               AND deleted_at IS NULL ORDER BY id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(trip_id as i64)
        .bind(section)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list trip section items",
        })?;
        let mut items = Vec::new();
        for row in rows {
            if let Some(id) = first_id(&row) {
                items.push(self.get_task_with_meta(id).await?);
            }
        }
        Ok(items)
    }

    /// Trips of the managed tag with their checklist sections, for the
    /// travel panel.
    pub async fn list_trips_with_items(&mut self, tag_id: u64) -> QueryResult<Vec<TripWithItems>> {
        let mut out = Vec::new();
        for trip in self.list_trips(tag_id).await? {
            let mut sections = Vec::new();
            for section_name in [PACK_SECTION, LEAVE_SECTION] {
                let items = self.trip_items_in_section(trip.id, section_name).await?;
                if !items.is_empty() {
                    sections.push(TripSection {
                        name: section_name.to_string(),
                        items,
                    });
                }
            }
            out.push(TripWithItems { trip, sections });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let items = travel_items(
            "3",
            &["hiking".to_string(), "camping".to_string()],
        );
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
        // Unrelated activity items stay out.
        assert!(!titles.contains(&"Wedding gift"));
    }

    #[test]
    fn test_travel_items_long_trips_add_extras() {
        let short = travel_items("30+", &[]);
        let short_titles: Vec<&str> = short.iter().map(|(_, t)| t.as_str()).collect();
        assert!(short_titles.contains(&"Laundry bag"));
        assert!(short_titles.contains(&"Pause mail and subscriptions"));

        let day = travel_items("1", &[]);
        let day_titles: Vec<&str> = day.iter().map(|(_, t)| t.as_str()).collect();
        assert!(!day_titles.contains(&"Laundry bag"));
    }

    #[tokio::test]
    async fn test_managed_tag_lifecycle() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let recipe_id = store
            .create_recipe(
                "travel",
                serde_json::json!({
                    "name": "Travel checklists",
                    "managed_tag": "managed:packing-list",
                    "nodes": [{ "id": "a", "kind": "action", "title": "A" }],
                    "edges": []
                }),
            )
            .await?
            .id;

        assert!(store.tag_managed_by_recipe(recipe_id).await?.is_none());
        let tag = store.enable_managed_recipe(recipe_id).await?;
        assert_eq!(tag.name, "managed:packing-list");
        assert_eq!(tag.label(), "Travel checklists");
        assert_eq!(store.managed_recipe_for_tag(tag.id).await?, Some(recipe_id));
        // Idempotent: enabling twice keeps the same tag.
        let again = store.enable_managed_recipe(recipe_id).await?;
        assert_eq!(again.id, tag.id);
        assert_eq!(store.list_tags().await?.len(), 1);

        // Disable removes the tag and everything it owns.
        let trip = store
            .create_trip(tag.id, "Hiking · 5 days".to_string(), "5".to_string(), vec!["hiking".to_string()])
            .await?;
        let items = store.trip_items_in_section(trip.id, PACK_SECTION).await?;
        assert!(items.len() >= 5);

        store.remove_managed_tag_content(recipe_id).await?;
        assert!(store.tag_managed_by_recipe(recipe_id).await?.is_none());
        assert!(store.list_trips(tag.id).await?.is_empty());
        assert_eq!(store.list_tags().await?.len(), 0);
        // Items are tombstoned, not hard-deleted.
        assert!(store.list_tasks().await?.iter().all(|t| t.deleted_at.is_some()));
        Ok(())
    }

    #[tokio::test]
    async fn test_create_trip_spawns_tagged_items() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let tag = store.create_tag("Travel").await?;
        let trip = store
            .create_trip(
                tag.id,
                "Beach · 2 days".to_string(),
                "2".to_string(),
                vec!["swimming".to_string(), "swimming".to_string()],
            )
            .await?;
        assert_eq!(trip.activities.0, vec!["swimming".to_string()]);

        let sections = store.list_trips_with_items(tag.id).await?;
        assert_eq!(sections.len(), 1);
        let names: Vec<&str> = sections[0].sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Pack", "Before leaving"]);

        // Every item is a real task tagged with the managed tag.
        let by_tag = store.list_tasks_by_tag(tag.id).await?;
        let item_ids: Vec<u64> = sections[0]
            .sections
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.task.id))
            .collect();
        assert_eq!(by_tag.len(), item_ids.len());
        for id in item_ids {
            let meta = store.get_task_with_meta(id).await?;
            assert!(meta.trip_id == Some(trip.id));
            assert!(meta.trip_section.is_some());
            assert!(meta.direct_tags.contains(&"Travel".to_string()));
        }
        Ok(())
    }
}
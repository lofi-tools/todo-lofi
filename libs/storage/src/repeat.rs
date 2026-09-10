//! Repeating task templates. A template stores a name and a repeat period
//! in days; the tasks that materialize the repeat are linked through
//! `repeat_task_occurrences`, with the task the template was created from
//! as the first occurrence.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;
use toasty::Model;

#[derive(Debug, Clone, Model)]
pub struct RepeatTaskTemplate {
    #[key]
    #[auto]
    pub id: u64,
    pub name: String,
    /// Repeat period in days (1 = daily, 7 = weekly, 30 = monthly,
    /// 365 = yearly, any N = custom).
    pub interval_days: u64,
    /// Time of day for each occurrence, as minutes since midnight.
    /// `None` means no specific time.
    pub time_of_day: Option<u64>,
    /// Weekdays for "every week on Mon/…" as JSON vec of 0=Mon..6=Sun.
    pub weekdays: Option<toasty::Json<Vec<u8>>>,
    /// Month day for "every Nth" (1-31, -1 = last day).
    pub month_day: Option<i64>,
    /// Strict `every!` flag: no skipping of missed dates.
    #[default(false)]
    pub strict: bool,
    /// Original timezone for recurrence evaluation.
    pub timezone: Option<String>,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
}

fn parse_template_row(row: &toasty::stmt::Value) -> Option<RepeatTaskTemplate> {
    if let toasty::stmt::Value::Record(record) = row {
        let id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
        let name = record
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let interval_days = record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
        let time_of_day = record.get(3).and_then(|v| v.to_i64()).map(|m| m as u64);
        let created_at = record.get(4).and_then(|v| v.as_str())?.parse().ok()?;
        Some(RepeatTaskTemplate {
            id,
            name,
            interval_days,
            time_of_day,
            weekdays: None,
            month_day: None,
            strict: false,
            timezone: None,
            created_at,
        })
    } else {
        None
    }
}

impl TodoStore {
    /// The repeat template that `task_id` is an occurrence of, if any.
    pub async fn repeat_template_for_task(
        &mut self,
        task_id: u64,
    ) -> QueryResult<Option<RepeatTaskTemplate>> {
        let rows = toasty::sql::query(
            r#"
            SELECT rtt.id, rtt.name, rtt.interval_days, rtt.time_of_day, rtt.created_at
            FROM repeat_task_occurrences rto
            JOIN repeat_task_templates rtt ON rtt.id = rto.template_id
            WHERE rto.task_id = ?1
            "#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load repeat template for task",
        })?;
        Ok(rows.iter().find_map(parse_template_row))
    }

    /// Link `task_id` as the first occurrence of a repeat template. When
    /// the task is already linked, its template's frequency (and name) are
    /// updated instead of creating a second template.
    pub async fn set_repeat(
        &mut self,
        task_id: u64,
        name: String,
        interval_days: u64,
        time_of_day: Option<u64>,
    ) -> QueryResult<RepeatTaskTemplate> {
        if let Some(existing) = self.repeat_template_for_task(task_id).await? {
            RepeatTaskTemplate::update_by_id(existing.id)
                .name(name.clone())
                .interval_days(interval_days)
                .time_of_day(time_of_day)
                .exec(&mut self.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "update repeat template",
                })?;
            return Ok(RepeatTaskTemplate {
                id: existing.id,
                name,
                interval_days,
                time_of_day,
                weekdays: existing.weekdays,
                month_day: existing.month_day,
                strict: existing.strict,
                timezone: existing.timezone,
                created_at: existing.created_at,
            });
        }
        let template = RepeatTaskTemplate::create()
            .name(name)
            .interval_days(interval_days)
            .time_of_day(time_of_day)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "create repeat template",
            })?;
        toasty::sql::statement(
            r#"INSERT INTO repeat_task_occurrences (template_id, task_id, occurrence_index)
               VALUES (?1, ?2, 0)
               ON CONFLICT (template_id, task_id) DO NOTHING"#,
        )
        .bind(template.id as i64)
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "link repeat occurrence",
        })?;
        Ok(template)
    }

    /// Delete the repeat template that `task_id` is an occurrence of
    /// (occurrence rows cascade). A task without a template is a no-op.
    pub async fn remove_repeat(&mut self, task_id: u64) -> QueryResult<()> {
        let Some(template) = self.repeat_template_for_task(task_id).await? else {
            return Ok(());
        };
        RepeatTaskTemplate::delete_by_id(&mut self.db, template.id)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "delete repeat template",
            })?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use crate::TodoStore;
    use crate::prelude::*;

    #[tokio::test]
    async fn test_set_repeat_links_first_occurrence() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let task = storage
            .create_task(Task::create().title("Water plants"))
            .await?;

        assert!(storage.repeat_template_for_task(task.id).await?.is_none());

        let template = storage
            .set_repeat(task.id, "Water plants".to_string(), 7, Some(18 * 60))
            .await?;
        assert_eq!(template.name, "Water plants");
        assert_eq!(template.interval_days, 7);
        assert_eq!(template.time_of_day, Some(18 * 60));

        let found = storage.repeat_template_for_task(task.id).await?;
        let found = found.unwrap();
        assert_eq!(found.interval_days, 7);
        assert_eq!(found.time_of_day, Some(18 * 60));

        // Re-linking updates the frequency instead of creating a duplicate.
        let updated = storage
            .set_repeat(task.id, "Water plants".to_string(), 14, None)
            .await?;
        assert_eq!(updated.id, template.id);
        assert_eq!(updated.interval_days, 14);
        assert_eq!(updated.time_of_day, None);
        assert_eq!(
            storage
                .repeat_template_for_task(task.id)
                .await?
                .unwrap()
                .interval_days,
            14
        );

        // An unrelated task is not linked.
        let other = storage.create_task(Task::create().title("Other")).await?;
        assert!(storage.repeat_template_for_task(other.id).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_remove_repeat() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let task = storage.create_task(Task::create().title("Repeating")).await?;
        storage
            .set_repeat(task.id, "Repeating".to_string(), 1, None)
            .await?;
        assert!(storage.repeat_template_for_task(task.id).await?.is_some());

        storage.remove_repeat(task.id).await?;
        assert!(storage.repeat_template_for_task(task.id).await?.is_none());

        // Removing again is a no-op.
        storage.remove_repeat(task.id).await?;
        Ok(())
    }
}
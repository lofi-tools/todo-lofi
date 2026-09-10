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
    /// Start time of day for each occurrence, as minutes since midnight.
    /// Each materialized occurrence is created with `blocked_until` set to
    /// today's start time, so the task is hidden until it becomes doable.
    /// `None` means occurrences start immediately.
    pub start_time_of_day: Option<u64>,
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

/// Columns selected in template order below: id, name, interval_days,
/// time_of_day, created_at, weekdays, month_day, strict, timezone,
/// start_time_of_day.
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
        let weekdays = record
            .get(5)
            .and_then(|v| v.as_str())
            .and_then(|raw| serde_json::from_str(raw).ok())
            .map(toasty::Json);
        let month_day = record.get(6).and_then(|v| v.to_i64());
        let strict = record.get(7).and_then(|v| v.to_i64()).unwrap_or(0) != 0;
        let timezone = record
            .get(8)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let start_time_of_day = record.get(9).and_then(|v| v.to_i64()).map(|m| m as u64);
        Some(RepeatTaskTemplate {
            id,
            name,
            interval_days,
            time_of_day,
            start_time_of_day,
            weekdays,
            month_day,
            strict,
            timezone,
            created_at,
        })
    } else {
        None
    }
}

const TEMPLATE_COLUMNS: &str = "rtt.id, rtt.name, rtt.interval_days, rtt.time_of_day, \
     rtt.created_at, rtt.weekdays, rtt.month_day, rtt.strict, rtt.timezone, rtt.start_time_of_day";

impl TodoStore {
    /// The repeat template that `task_id` is an occurrence of, if any.
    pub async fn repeat_template_for_task(
        &mut self,
        task_id: u64,
    ) -> QueryResult<Option<RepeatTaskTemplate>> {
        let rows = toasty::sql::query(
            r#"
            SELECT {TEMPLATE_COLUMNS}
            FROM repeat_task_occurrences rto
            JOIN repeat_task_templates rtt ON rtt.id = rto.template_id
            WHERE rto.task_id = ?1
            "#
            .replace("{TEMPLATE_COLUMNS}", TEMPLATE_COLUMNS),
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
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
        start_time_of_day: Option<u64>,
    ) -> QueryResult<RepeatTaskTemplate> {
        if let Some(existing) = self.repeat_template_for_task(task_id).await? {
            RepeatTaskTemplate::update_by_id(existing.id)
                .name(name.clone())
                .interval_days(interval_days)
                .time_of_day(time_of_day)
                .start_time_of_day(start_time_of_day)
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
                start_time_of_day,
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
            .start_time_of_day(start_time_of_day)
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

    /// All repeat templates, e.g. for the daily materializer.
    pub async fn list_repeat_templates(&mut self) -> QueryResult<Vec<RepeatTaskTemplate>> {
        let rows = toasty::sql::query(
            r#"SELECT id, name, interval_days, time_of_day, created_at,
                      weekdays, month_day, strict, timezone, start_time_of_day
               FROM repeat_task_templates ORDER BY id"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list repeat templates",
        })?;
        // `parse_template_row` reads `rtt.`-prefixed rows positionally, so
        // the column order above must match `TEMPLATE_COLUMNS`.
        Ok(rows.iter().filter_map(parse_template_row).collect())
    }

    /// Occurrence task ids of a template, oldest first.
    async fn repeat_occurrences(&mut self, template_id: u64) -> QueryResult<Vec<u64>> {
        let rows = toasty::sql::query(
            r#"SELECT task_id FROM repeat_task_occurrences
               WHERE template_id = ?1 ORDER BY occurrence_index"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(template_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list repeat occurrences",
        })?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row
                && let Some(id) = record.first().and_then(|v| v.to_i64())
            {
                ids.push(id as u64);
            }
        }
        Ok(ids)
    }

    /// Materialize upcoming occurrences for every daily template: each
    /// date whose start (or deadline) falls within the next 2 days gets an
    /// occurrence unless one already exists — even a tombstoned one, so a
    /// deleted occurrence is never resurrected. The next occurrence
    /// replaces previous ones: open occurrences from past dates are
    /// tombstoned (never completed). Returns the number of tasks created.
    pub async fn materialize_daily_occurrences(
        &mut self,
        now_secs: u64,
    ) -> QueryResult<usize> {
        let templates = self.list_repeat_templates().await?;
        let mut created = 0;
        for template in templates
            .into_iter()
            .filter(|t| t.interval_days == 1 && t.time_of_day.is_some())
        {
            created += self
                .materialize_upcoming_occurrences(&template, now_secs)
                .await?;
        }
        Ok(created)
    }

    async fn materialize_upcoming_occurrences(
        &mut self,
        template: &RepeatTaskTemplate,
        now_secs: u64,
    ) -> QueryResult<usize> {
        use jiff::ToSpan;
        let zone = template_zone(template);
        let today = jiff::Timestamp::from_second(now_secs as i64)
            .map(|stamp| stamp.to_zoned(zone.clone()).date())
            .unwrap_or_else(|_| jiff::Timestamp::now().to_zoned(zone.clone()).date());
        let mut created = 0;
        for offset in 0..8 {
            let date = today.checked_add(offset.days()).unwrap_or(today);
            let Some((blocked_until, deadline)) =
                occurrence_times_for_date(template, &zone, date)
            else {
                continue;
            };
            let anchor = blocked_until.unwrap_or(deadline);
            if anchor > now_secs + MATERIALIZE_WINDOW_SECS {
                break;
            }
            if self
                .ensure_date_occurrence(template, blocked_until, deadline)
                .await?
            {
                created += 1;
            }
        }
        // Replace previous occurrences: tombstone open ones from past dates.
        let day_start = today
            .at(0, 0, 0, 0)
            .to_zoned(zone)
            .ok()
            .map(|zoned| zoned.timestamp().as_second() as u64)
            .unwrap_or(now_secs);
        for task_id in self.repeat_occurrences(template.id).await? {
            let Ok(task) = self.get_task(task_id).await else {
                continue;
            };
            if !task.done
                && task.deleted_at.is_none()
                && task.deadline.is_none_or(|deadline| deadline < day_start)
            {
                self.tombstone_task(task_id).await?;
            }
        }
        Ok(created)
    }

    /// Create the occurrence for one date unless any linked occurrence
    /// (live, done or tombstoned) already carries its deadline.
    async fn ensure_date_occurrence(
        &mut self,
        template: &RepeatTaskTemplate,
        blocked_until: Option<u64>,
        deadline: u64,
    ) -> QueryResult<bool> {
        let mut latest: Option<crate::Task> = None;
        for task_id in self.repeat_occurrences(template.id).await? {
            let Ok(task) = self.get_task(task_id).await else {
                continue;
            };
            if task.deadline == Some(deadline) {
                return Ok(false);
            }
            if task.deleted_at.is_none()
                && latest.as_ref().is_none_or(|best: &crate::Task| {
                    task.deadline.unwrap_or(0) > best.deadline.unwrap_or(0)
                })
            {
                latest = Some(task);
            }
        }
        let (importance_factor, urgency_factor) = latest
            .as_ref()
            .map(|task| (task.importance_factor, task.urgency_factor))
            .unwrap_or((1.0, 1.0));
        let created = self
            .create_task(
                crate::Task::create()
                    .title(template.name.clone())
                    .deadline(Some(deadline))
                    .blocked_until(blocked_until)
                    .importance_factor(importance_factor)
                    .urgency_factor(urgency_factor),
            )
            .await?;
        if let Some(source) = latest {
            for tag in self.get_direct_task_tags(source.id).await? {
                self.assign_tag_to_task(created.id, &tag.name).await?;
            }
        }
        let index = self.repeat_occurrences(template.id).await?.len() as i64;
        toasty::sql::statement(
            r#"INSERT INTO repeat_task_occurrences (template_id, task_id, occurrence_index)
               VALUES (?1, ?2, ?3)
               ON CONFLICT (template_id, task_id) DO NOTHING"#,
        )
        .bind(template.id as i64)
        .bind(created.id as i64)
        .bind(index)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "link repeat occurrence",
        })?;
        Ok(true)
    }
}

/// Timezone for recurrence evaluation: the template's zone, else UTC.
/// Task instances store UTC epochs only.
fn template_zone(template: &RepeatTaskTemplate) -> jiff::tz::TimeZone {
    template
        .timezone
        .as_deref()
        .and_then(|name| jiff::tz::TimeZone::get(name).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC)
}

fn civil_at(
    date: jiff::civil::Date,
    minutes: u64,
    zone: &jiff::tz::TimeZone,
) -> Option<u64> {
    date.at((minutes / 60) as i8, (minutes % 60) as i8, 0, 0)
        .to_zoned(zone.clone())
        .ok()
        .map(|zoned| zoned.timestamp().as_second() as u64)
}

/// Occurrences materialize while startable (or due) within this horizon.
const MATERIALIZE_WINDOW_SECS: u64 = 2 * 86400;

/// `(blocked_until, deadline)` UTC epochs for one civil date in the
/// template's timezone. `None` without a due time.
pub fn occurrence_times_for_date(
    template: &RepeatTaskTemplate,
    zone: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
) -> Option<(Option<u64>, u64)> {
    let due = template.time_of_day?;
    Some((
        template
            .start_time_of_day
            .and_then(|start| civil_at(date, start, zone)),
        civil_at(date, due, zone)?,
    ))
}

/// Today's `(blocked_until, deadline)` UTC epochs for a daily template in
/// its timezone: the start time hides the task until it becomes doable,
/// the due time is the deadline. `None` without a due time.
pub fn occurrence_times(
    template: &RepeatTaskTemplate,
    now_secs: u64,
) -> Option<(Option<u64>, u64)> {
    let zone = template_zone(template);
    let date = jiff::Timestamp::from_second(now_secs as i64)
        .ok()?
        .to_zoned(zone.clone())
        .date();
    occurrence_times_for_date(template, &zone, date)
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::occurrence_times;
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
            .set_repeat(task.id, "Water plants".to_string(), 7, Some(18 * 60), None)
            .await?;
        assert_eq!(template.name, "Water plants");
        assert_eq!(template.interval_days, 7);
        assert_eq!(template.time_of_day, Some(18 * 60));
        assert_eq!(template.start_time_of_day, None);

        let found = storage.repeat_template_for_task(task.id).await?;
        let found = found.unwrap();
        assert_eq!(found.interval_days, 7);
        assert_eq!(found.time_of_day, Some(18 * 60));

        // Re-linking updates the frequency instead of creating a duplicate.
        let updated = storage
            .set_repeat(task.id, "Water plants".to_string(), 14, None, Some(8 * 60))
            .await?;
        assert_eq!(updated.id, template.id);
        assert_eq!(updated.interval_days, 14);
        assert_eq!(updated.time_of_day, None);
        assert_eq!(updated.start_time_of_day, Some(8 * 60));
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
            .set_repeat(task.id, "Repeating".to_string(), 1, None, None)
            .await?;
        assert!(storage.repeat_template_for_task(task.id).await?.is_some());

        storage.remove_repeat(task.id).await?;
        assert!(storage.repeat_template_for_task(task.id).await?.is_none());

        // Removing again is a no-op.
        storage.remove_repeat(task.id).await?;
        Ok(())
    }

    fn utc_midday() -> u64 {
        let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
        now.date()
            .at(12, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second() as u64
    }

    #[tokio::test]
    async fn test_materialize_daily_occurrence() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let task = storage
            .create_task(
                Task::create()
                    .title("Feed dorito")
                    .importance_factor(3.0),
            )
            .await?;
        storage
            .set_repeat(
                task.id,
                "Feed dorito".to_string(),
                1,
                Some(18 * 60 + 30),
                Some(17 * 60 + 50),
            )
            .await?;

        // Midday UTC: today and tomorrow fall inside the 2-day window
        // (day+2's 5:50pm is ~54h out); both materialize at once.
        let noon = utc_midday();
        assert_eq!(storage.materialize_daily_occurrences(noon).await?, 2);

        let template = storage.repeat_template_for_task(task.id).await?.unwrap();
        let occurrences = storage.repeat_occurrences(template.id).await?;
        assert_eq!(occurrences.len(), 3);
        let today = storage.get_task(occurrences[1]).await?;
        assert_eq!(today.title, "Feed dorito");
        assert_eq!(today.importance_factor, 3.0);
        assert_eq!(today.deadline, Some(noon + 6 * 3600 + 30 * 60));
        assert_eq!(today.blocked_until, Some(noon + 5 * 3600 + 50 * 60));
        let tomorrow = storage.get_task(occurrences[2]).await?;
        assert_eq!(tomorrow.deadline, Some(noon + 86400 + 6 * 3600 + 30 * 60));
        // Instances store UTC epochs only.
        assert!(today.timezone.is_none());

        // The first task (no deadline yet) was replaced.
        let first = storage.get_task(occurrences[0]).await?;
        assert!(first.deleted_at.is_some());

        // Listed under Upcoming until 5:50pm: present, but sorted below
        // doable tasks (it is the only row here).
        let listed = storage.list_tasks_by_priority().await?;
        assert!(listed.iter().any(|t| t.id == today.id));

        // Idempotent: a second run creates nothing.
        assert_eq!(storage.materialize_daily_occurrences(noon).await?, 0);

        // Next day: day+2 slides into the window (1 created); the
        // previous open occurrence is replaced.
        let tomorrow_noon = noon + 86400;
        assert_eq!(
            storage
                .materialize_daily_occurrences(tomorrow_noon)
                .await?,
            1
        );
        let replaced = storage.get_task(today.id).await?;
        assert!(replaced.deleted_at.is_some());
        let occurrences = storage.repeat_occurrences(template.id).await?;
        let next = storage.get_task(*occurrences.last().unwrap()).await?;
        assert_eq!(
            next.deadline,
            Some(tomorrow_noon + 86400 + 6 * 3600 + 30 * 60)
        );
        assert!(next.deleted_at.is_none());
        Ok(())
    }

    #[test]
    fn test_occurrence_times_uses_template_timezone() {
        let template = RepeatTaskTemplate {
            id: 1,
            name: "x".to_string(),
            interval_days: 1,
            time_of_day: Some(18 * 60 + 30),
            start_time_of_day: Some(17 * 60 + 50),
            weekdays: None,
            month_day: None,
            strict: false,
            timezone: Some("America/New_York".to_string()),
            created_at: jiff::Timestamp::now(),
        };
        // Noon UTC = 8am in New York (EDT): same civil date.
        let noon_utc: u64 = "2026-09-10T12:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second() as u64;
        let (start, deadline) = occurrence_times(&template, noon_utc).unwrap();
        // 17:50 / 18:30 EDT = 21:50 / 22:30 UTC.
        let day: u64 = "2026-09-10T00:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second() as u64;
        assert_eq!(start, Some(day + 21 * 3600 + 50 * 60));
        assert_eq!(deadline, day + 22 * 3600 + 30 * 60);
    }
}
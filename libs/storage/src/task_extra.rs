//! Per-task extension data.
//!
//! A namespaced key/value store any app or extension can write to, so a new
//! feature no longer needs a dedicated `tasks` column (see
//! `docs/spec/coding-interview-readonly-session-spec.md` §8). Values are JSON
//! text; the coding workflow keeps its specs here under
//! [`CODING_NAMESPACE`] / [`SPEC_KEY`].

use snafu::ResultExt;

use crate::TodoStore;

/// Namespace of the coding workflow's task data.
pub const CODING_NAMESPACE: &str = "coding";
/// Key holding a task's spec markdown (a JSON string).
pub const SPEC_KEY: &str = "spec";

/// One namespaced slice of a task's extension data.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskExtra {
    pub task_id: u64,
    pub namespace: String,
    pub key: String,
    pub value: serde_json::Value,
}

impl TodoStore {
    /// Read one namespaced value. `None` when the key was never written.
    pub async fn get_task_extra(
        &mut self,
        task_id: u64,
        namespace: &str,
        key: &str,
    ) -> crate::QueryResult<Option<serde_json::Value>> {
        let rows = toasty::sql::query(
            r#"SELECT value FROM task_extra
               WHERE task_id = ?1 AND namespace = ?2 AND key = ?3"#,
        )
        .column_types([toasty::stmt::Type::String])
        .bind(task_id as i64)
        .bind(namespace)
        .bind(key)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "get task extra",
        })?;
        let raw = match rows.first() {
            Some(toasty::stmt::Value::Record(record)) => {
                record.first().and_then(|value| value.as_str())
            }
            _ => None,
        };
        match raw {
            Some(raw) => Ok(Some(serde_json::from_str(raw)?)),
            None => Ok(None),
        }
    }

    /// Upsert one namespaced value.
    pub async fn set_task_extra(
        &mut self,
        task_id: u64,
        namespace: &str,
        key: &str,
        value: serde_json::Value,
    ) -> crate::QueryResult<()> {
        let value = serde_json::to_string(&value)?;
        let now = jiff::Timestamp::now().to_string();
        toasty::sql::statement(
            r#"INSERT INTO task_extra (task_id, namespace, key, value, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)
               ON CONFLICT(task_id, namespace, key)
               DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at"#,
        )
        .bind(task_id as i64)
        .bind(namespace)
        .bind(key)
        .bind(&value)
        .bind(&now)
        .bind(&now)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "set task extra",
        })?;
        Ok(())
    }

    /// Every key of one namespace on a task, ordered by key.
    pub async fn task_extra_for_namespace(
        &mut self,
        task_id: u64,
        namespace: &str,
    ) -> crate::QueryResult<Vec<TaskExtra>> {
        let rows = toasty::sql::query(
            r#"SELECT key, value FROM task_extra
               WHERE task_id = ?1 AND namespace = ?2 ORDER BY key"#,
        )
        .column_types([toasty::stmt::Type::String, toasty::stmt::Type::String])
        .bind(task_id as i64)
        .bind(namespace)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list task extra",
        })?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(record) = row else {
                continue;
            };
            let Some(key) = record.first().and_then(|value| value.as_str()) else {
                continue;
            };
            let Some(raw) = record.get(1).and_then(|value| value.as_str()) else {
                continue;
            };
            entries.push(TaskExtra {
                task_id,
                namespace: namespace.to_string(),
                key: key.to_string(),
                value: serde_json::from_str(raw)?,
            });
        }
        Ok(entries)
    }

    /// Drop every namespace for a task. Called when a task is deleted, since
    /// SQLite does not enforce the reference (AGENTS.md: no silent orphans).
    pub async fn delete_task_extra(&mut self, task_id: u64) -> crate::QueryResult<()> {
        toasty::sql::statement(r#"DELETE FROM task_extra WHERE task_id = ?1"#)
            .bind(task_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "delete task extra",
            })?;
        Ok(())
    }

    /// A task's spec markdown, or `None` when nothing was written.
    pub async fn get_task_spec(&mut self, task_id: u64) -> crate::QueryResult<Option<String>> {
        let value = self
            .get_task_extra(task_id, CODING_NAMESPACE, SPEC_KEY)
            .await?;
        Ok(match value {
            Some(serde_json::Value::String(spec)) => Some(spec),
            _ => None,
        })
    }

    /// Write a task's spec markdown. `None` leaves the stored value alone, and
    /// a step row never carries a spec (decision #31), so it is refused.
    pub async fn set_task_spec(
        &mut self,
        task_id: u64,
        spec: Option<String>,
    ) -> crate::QueryResult<()> {
        self.ensure_not_a_step(task_id, "spec").await?;
        let Some(spec) = spec else {
            return Ok(());
        };
        self.set_task_extra(
            task_id,
            CODING_NAMESPACE,
            SPEC_KEY,
            serde_json::Value::String(spec),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spec_round_trips_through_task_extra() {
        let mut store = TodoStore::for_test().await.expect("store");
        let task = store
            .create_task(crate::Task::create().title("t".to_string()))
            .await
            .expect("task");
        assert_eq!(store.get_task_spec(task.id).await.expect("read"), None);
        store
            .save_task_spec(task.id, Some("# Spec".to_string()))
            .await
            .expect("write");
        let spec = store.get_task_spec(task.id).await.expect("read");
        assert_eq!(spec.as_deref(), Some("# Spec"));
        let meta = store.get_task_with_meta(task.id).await.expect("meta");
        assert_eq!(meta.spec.as_deref(), Some("# Spec"));
        // Upsert is idempotent and the namespace is isolated.
        store
            .set_task_extra(task.id, "other", "spec", serde_json::json!("not ours"))
            .await
            .expect("other namespace");
        store
            .save_task_spec(task.id, Some("# Spec 2".to_string()))
            .await
            .expect("rewrite");
        assert_eq!(
            store.get_task_spec(task.id).await.expect("read").as_deref(),
            Some("# Spec 2")
        );
        let entries = store
            .task_extra_for_namespace(task.id, CODING_NAMESPACE)
            .await
            .expect("namespace");
        assert_eq!(entries.len(), 1);
        // Deleting the task takes its extension data with it.
        store.delete_task(task.id).await.expect("delete");
        assert_eq!(
            store.get_task_spec(task.id).await.expect("read after delete"),
            None
        );
    }

    /// `0026_task_extra.sql` backfills the old `tasks.spec` column through
    /// `json_quote`. Run that exact expression against a literal so the
    /// function's availability and the escaping it produces stay covered.
    #[tokio::test]
    async fn the_migration_backfill_expression_is_readable() {
        let mut store = TodoStore::for_test().await.expect("store");
        let task = store
            .create_task(crate::Task::create().title("t".to_string()))
            .await
            .expect("task");
        toasty::sql::statement(
            r#"INSERT INTO task_extra (task_id, namespace, key, value, created_at, updated_at)
               SELECT ?1, 'coding', 'spec', json_quote('# Spec'), 'x', 'x'"#,
        )
        .bind(task.id as i64)
        .exec(&mut store.db)
        .await
        .expect("backfill expression");
        assert_eq!(
            store.get_task_spec(task.id).await.expect("spec").as_deref(),
            Some("# Spec")
        );
    }
}

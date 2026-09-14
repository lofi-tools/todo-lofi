//! Generic external-integration identity map.
//!
//! Multiple integrations (Todoist today, others later) link remote objects
//! to local tasks/tags. Identity lives in link tables scoped by
//! `integration_id`, never in a single `external_id` column, so one local
//! task can sync with several providers at once.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;

/// One connected account. The `id` is the integration id used by all link
/// rows and sync state.
#[derive(Debug, Clone)]
pub struct Integration {
    pub id: u64,
    pub provider: String,
    pub account_label: Option<String>,
    pub created_at: jiff::Timestamp,
}

/// One-way imported remote comment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ExternalComment {
    pub external_id: String,
    pub text: String,
    pub author: Option<String>,
    pub created_at: Option<String>,
}

/// Remote task link with its merge timestamp.
#[derive(Debug, Clone)]
pub struct TaskLink {
    pub integration_id: u64,
    pub external_id: String,
    pub task_id: u64,
    pub external_updated_at: Option<jiff::Timestamp>,
}

/// Remote tag link.
#[derive(Debug, Clone)]
pub struct TagLink {
    pub integration_id: u64,
    pub external_id: String,
    pub tag_id: u64,
    pub source_kind: String,
    pub namespaced: bool,
}

fn parse_timestamp(value: &toasty::stmt::Value) -> Option<jiff::Timestamp> {
    match value {
        toasty::stmt::Value::String(s) if !s.is_empty() => s.parse().ok(),
        _ => None,
    }
}

fn parse_opt_string(value: &toasty::stmt::Value) -> Option<String> {
    match value {
        toasty::stmt::Value::String(s) if !s.is_empty() => Some(s.clone()),
        toasty::stmt::Value::String(_) => None,
        _ => value.as_str().map(str::to_owned),
    }
}

impl TodoStore {
    pub async fn create_integration(
        &mut self,
        provider: &str,
        account_label: Option<String>,
    ) -> QueryResult<Integration> {
        let created_at = jiff::Timestamp::now();
        let now = created_at.to_string();
        toasty::sql::statement(
            r#"INSERT INTO integrations (provider, account_label, created_at) VALUES (?1, ?2, ?3)"#,
        )
        .bind(provider)
        .bind(account_label.clone().unwrap_or_default())
        .bind(now.clone())
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "create integration",
        })?;
        let id = self.last_insert_id().await?;
        // Every integration is an app in the unified model, so links can
        // attach it and capture new tasks in the tags it syncs.
        let label = account_label
            .clone()
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| {
                let mut chars = provider.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + chars.as_str()
                    }
                }
            });
        let app = self
            .upsert_app("integration", &format!("{provider}-{id}"), &label, None)
            .await?;
        self.set_app_enabled(app.id, true).await?;
        self.set_integration_app(id, app.id).await?;
        Ok(Integration {
            id,
            provider: provider.to_string(),
            account_label,
            created_at,
        })
    }

    pub(crate) async fn last_insert_id(&mut self) -> QueryResult<u64> {
        let rows = toasty::sql::query("SELECT last_insert_rowid()")
            .column_types([toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "last insert id",
            })?;
        match rows.first() {
            Some(toasty::stmt::Value::Record(record)) => Ok(record
                .first()
                .and_then(|v| v.to_i64())
                .unwrap_or(0) as u64),
            _ => Ok(0),
        }
    }

    pub async fn list_integrations(&mut self) -> QueryResult<Vec<Integration>> {
        let rows = toasty::sql::query(
            r#"SELECT id, provider, account_label, created_at FROM integrations ORDER BY id"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list integrations",
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let provider = record
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let account_label = record.get(2).and_then(parse_opt_string);
                let created_at = record
                    .get(3)
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(jiff::Timestamp::now);
                out.push(Integration {
                    id,
                    provider,
                    account_label,
                    created_at,
                });
            }
        }
        Ok(out)
    }

    pub async fn delete_integration(&mut self, integration_id: u64) -> QueryResult<()> {
        for table in ["external_task_links", "external_tag_links", "sync_state"] {
            toasty::sql::statement(format!(
                "DELETE FROM {table} WHERE integration_id = ?1"
            ))
            .bind(integration_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "delete integration links",
            })?;
        }
        // The integration's app goes with it: its capture bindings would
        // otherwise keep marking new tasks as owned by an app that no
        // longer syncs anywhere.
        if let Some(app) = self.app_for_integration(integration_id).await? {
            self.disable_app(app.id, false).await?;
        }
        toasty::sql::statement("DELETE FROM integrations WHERE id = ?1")
            .bind(integration_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "delete integration",
            })?;
        Ok(())
    }

    /// Upsert a task link; refreshes the remote updated timestamp used by
    /// per-field merge (§4.2 of the Todoist spec).
    pub async fn link_task(
        &mut self,
        integration_id: u64,
        external_id: &str,
        task_id: u64,
        external_updated_at: Option<jiff::Timestamp>,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"INSERT INTO external_task_links
               (integration_id, external_id, task_id, external_updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT (integration_id, external_id)
               DO UPDATE SET task_id = excluded.task_id,
                             external_updated_at = excluded.external_updated_at"#,
        )
        .bind(integration_id as i64)
        .bind(external_id)
        .bind(task_id as i64)
        .bind(external_updated_at.map(|t| t.to_string()).unwrap_or_default())
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "link external task",
        })?;
        Ok(())
    }

    pub async fn task_link(
        &mut self,
        integration_id: u64,
        external_id: &str,
    ) -> QueryResult<Option<TaskLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, task_id, external_updated_at
               FROM external_task_links WHERE integration_id = ?1 AND external_id = ?2"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "lookup task link",
        })?;
        Ok(rows.into_iter().next().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => Some(TaskLink {
                integration_id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                external_id: record
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                task_id: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                external_updated_at: record.get(3).and_then(parse_timestamp),
            }),
            _ => None,
        }))
    }

    /// All remote links for one local task (across integrations).
    pub async fn task_links_for_task(&mut self, task_id: u64) -> QueryResult<Vec<TaskLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, task_id, external_updated_at
               FROM external_task_links WHERE task_id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "links for task",
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|row| match row {
                toasty::stmt::Value::Record(record) => Some(TaskLink {
                    integration_id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                    external_id: record
                        .get(1)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    task_id: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                    external_updated_at: record.get(3).and_then(parse_timestamp),
                }),
                _ => None,
            })
            .collect())
    }

    pub async fn unlink_task(&mut self, integration_id: u64, external_id: &str) -> QueryResult<()> {
        toasty::sql::statement(
            r#"DELETE FROM external_task_links WHERE integration_id = ?1 AND external_id = ?2"#,
        )
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "unlink external task",
        })?;
        Ok(())
    }

    /// Remove the pairing between a remote project and a local tag. Synced
    /// tasks keep their local copies; they just stop syncing.
    pub async fn unlink_tag(&mut self, integration_id: u64, external_id: &str) -> QueryResult<()> {
        toasty::sql::statement(
            r#"DELETE FROM external_tag_links WHERE integration_id = ?1 AND external_id = ?2"#,
        )
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "unlink external tag",
        })?;
        Ok(())
    }

    pub async fn link_tag(
        &mut self,
        integration_id: u64,
        external_id: &str,
        tag_id: u64,
        source_kind: &str,
        namespaced: bool,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"INSERT INTO external_tag_links
               (integration_id, external_id, tag_id, source_kind, namespaced)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT (integration_id, external_id)
               DO UPDATE SET tag_id = excluded.tag_id,
                             source_kind = excluded.source_kind,
                             namespaced = excluded.namespaced"#,
        )
        .bind(integration_id as i64)
        .bind(external_id)
        .bind(tag_id as i64)
        .bind(source_kind)
        .bind(i64::from(namespaced))
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "link external tag",
        })?;
        if let Some(app) = self.app_for_integration(integration_id).await? {
            // A task added to the linked tag is propagated to the provider.
            self.ensure_capture_binding(app.id, tag_id).await?;
        }
        Ok(())
    }

    pub async fn tag_link(
        &mut self,
        integration_id: u64,
        external_id: &str,
    ) -> QueryResult<Option<TagLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, tag_id, source_kind, namespaced
               FROM external_tag_links WHERE integration_id = ?1 AND external_id = ?2"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
        ])
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "lookup tag link",
        })?;
        Ok(rows.into_iter().next().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => Some(TagLink {
                integration_id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                external_id: record
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                tag_id: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                source_kind: record
                    .get(3)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                namespaced: record.get(4).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
            }),
            _ => None,
        }))
    }

    /// All tag links for one integration (projects/sections/labels).
    /// Used by sync to find which remote projects are selected.
    pub async fn tag_links_for_integration(
        &mut self,
        integration_id: u64,
    ) -> QueryResult<Vec<TagLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, tag_id, source_kind, namespaced
               FROM external_tag_links WHERE integration_id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
        ])
        .bind(integration_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list tag links for integration",
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|row| match row {
                toasty::stmt::Value::Record(record) => Some(TagLink {
                    integration_id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                    external_id: record
                        .get(1)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    tag_id: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                    source_kind: record
                        .get(3)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    namespaced: record.get(4).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
                }),
                _ => None,
            })
            .collect())
    }

    /// All task links for one integration. Used by sync to tombstone tasks
    /// deleted on the remote side.
    pub async fn task_links_for_integration(
        &mut self,
        integration_id: u64,
    ) -> QueryResult<Vec<TaskLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, task_id, external_updated_at
               FROM external_task_links WHERE integration_id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .bind(integration_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list task links for integration",
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|row| match row {
                toasty::stmt::Value::Record(record) => Some(TaskLink {
                    integration_id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                    external_id: record
                        .get(1)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    task_id: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                    external_updated_at: record.get(3).and_then(parse_timestamp),
                }),
                _ => None,
            })
            .collect())
    }

    /// Provider per linked tag (`tag_id → provider`), for badging synced
    /// tags in the UI. Unlinked tags are absent from the map.
    pub async fn tag_link_providers(
        &mut self,
    ) -> QueryResult<std::collections::HashMap<u64, String>> {
        let rows = toasty::sql::query(
            r#"SELECT l.tag_id, i.provider FROM external_tag_links l
               JOIN integrations i ON i.id = l.integration_id"#,
        )
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::String])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "tag link providers",
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let tag_id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                if let Some(provider) = record.get(1).and_then(|v| v.as_str()) {
                    map.insert(tag_id, provider.to_string());
                }
            }
        }
        Ok(map)
    }

    /// Record a per-project sync watermark after success (§4.7).
    pub async fn record_watermark(
        &mut self,
        integration_id: u64,
        project_id: &str,
        watermark: &str,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"INSERT INTO sync_state (integration_id, project_id, watermark)
               VALUES (?1, ?2, ?3)
               ON CONFLICT (integration_id, project_id)
               DO UPDATE SET watermark = excluded.watermark"#,
        )
        .bind(integration_id as i64)
        .bind(project_id)
        .bind(watermark)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "record watermark",
        })?;
        Ok(())
    }

    pub async fn watermark(
        &mut self,
        integration_id: u64,
        project_id: &str,
    ) -> QueryResult<Option<String>> {
        let rows = toasty::sql::query(
            r#"SELECT watermark FROM sync_state WHERE integration_id = ?1 AND project_id = ?2"#,
        )
        .column_types([toasty::stmt::Type::String])
        .bind(integration_id as i64)
        .bind(project_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load watermark",
        })?;
        Ok(rows.into_iter().next().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.as_str()).map(str::to_owned)
            }
            _ => None,
        }))
    }

    /// Tombstone a task for mirrored deletions; tombstoned rows stay in the
    /// DB but are hidden from the UI.
    pub async fn tombstone_task(&mut self, id: u64) -> QueryResult<()> {
        crate::Task::update_by_id(id)
            .deleted_at(Some(jiff::Timestamp::now()))
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::TodoStore;
    use crate::prelude::*;

    #[tokio::test]
    async fn test_task_links_are_per_integration() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = storage.create_integration("todoist", None).await?;
        let second = storage.create_integration("todoist", None).await?;
        let task = storage.create_task(Task::create().title("Synced")).await?;

        storage
            .link_task(first.id, "ext-1", task.id, None)
            .await?;
        storage
            .link_task(second.id, "ext-9", task.id, None)
            .await?;

        let links = storage.task_links_for_task(task.id).await?;
        assert_eq!(links.len(), 2);

        let found = storage.task_link(first.id, "ext-1").await?.unwrap();
        assert_eq!(found.task_id, task.id);
        assert!(storage.task_link(first.id, "ext-9").await?.is_none());

        storage.unlink_task(first.id, "ext-1").await?;
        assert!(storage.task_link(first.id, "ext-1").await?.is_none());
        assert_eq!(storage.task_links_for_task(task.id).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_deleting_an_integration_releases_its_capture_bindings() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("todoist", None).await?;
        let app = storage
            .app_for_integration(integration.id)
            .await?
            .expect("creating an integration registers its app");
        let tag = storage.create_tag("Work").await?;
        storage
            .link_tag(integration.id, "proj-1", tag.id, "project", false)
            .await?;
        assert_eq!(storage.bindings_for_tag(tag.id).await?.len(), 1);

        storage.delete_integration(integration.id).await?;
        assert!(storage.bindings_for_tag(tag.id).await?.is_empty());
        assert!(
            !storage
                .app_by_id(app.id)
                .await?
                .expect("the app row stays for the history")
                .enabled
        );
        // Tasks added to the tag are the user's own again.
        let task = storage
            .create_task(Task::create().title("After disconnect"))
            .await?;
        storage.assign_tag_to_task(task.id, "Work").await?;
        assert!(storage.capture_task(task.id).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_tag_links_and_watermarks() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("todoist", None).await?;
        let tag = storage.create_tag("Work").await?;

        storage
            .link_tag(integration.id, "proj-1", tag.id, "project", false)
            .await?;
        let link = storage.tag_link(integration.id, "proj-1").await?.unwrap();
        assert_eq!(link.tag_id, tag.id);
        assert!(!link.namespaced);

        assert!(storage.watermark(integration.id, "proj-1").await?.is_none());
        storage
            .record_watermark(integration.id, "proj-1", "w1")
            .await?;
        assert_eq!(
            storage.watermark(integration.id, "proj-1").await?.as_deref(),
            Some("w1")
        );
        Ok(())
    }
}

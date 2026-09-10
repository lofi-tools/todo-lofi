//! Tag settings and sections.
//!
//! Settings hold per-tag configuration that is neither the tag's identity
//! (name/display name) nor its place in the implication DAG:
//! - `sync_target`: which remote object the tag syncs with, as
//!   `(integration_id, external_id)` pointing at an `external_tag_links`
//!   row's remote side (project/section for Todoist).
//! - `dirs`: local directories backing a dir-project tag. The legacy
//!   `project:{single-path}` name carries one dir; settings hold the full
//!   list so a project can span several directories.
//! - Sections: named subdivisions living inside one tag (e.g. Todoist
//!   sections under their project tag).
//!
//! The tables are schemaless-friendly on purpose: new settings arrive as
//! new nullable columns/JSON keys, never as model changes.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;
use std::collections::{HashMap, HashSet};

/// Sync target of a tag: the remote object it syncs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTarget {
    pub integration_id: u64,
    pub external_id: String,
}

/// Settings for one tag. Absent rows mean defaults (no sync, no dirs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagSettings {
    pub tag_id: u64,
    pub sync_target: Option<SyncTarget>,
    pub dirs: Vec<String>,
}

/// A named section inside a tag, ordered by `position`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSection {
    pub id: u64,
    pub tag_id: u64,
    pub name: String,
    pub position: i64,
}

fn parse_dirs(value: &toasty::stmt::Value) -> Vec<String> {
    match value {
        toasty::stmt::Value::String(raw) if !raw.is_empty() => {
            serde_json::from_str(raw).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

impl TodoStore {
    async fn read_settings(&mut self, tag_id: u64) -> QueryResult<TagSettings> {
        let rows = toasty::sql::query(
            r#"SELECT tag_id, sync_integration_id, sync_external_id, dirs
               FROM tag_settings WHERE tag_id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load tag settings",
        })?;
        let Some(toasty::stmt::Value::Record(record)) = rows.into_iter().next() else {
            return Ok(TagSettings {
                tag_id,
                ..Default::default()
            });
        };
        let sync_target = match (
            record.get(1).and_then(|v| v.to_i64()),
            record.get(2).and_then(|v| v.as_str()),
        ) {
            (Some(integration_id), Some(external_id)) if !external_id.is_empty() => {
                Some(SyncTarget {
                    integration_id: integration_id as u64,
                    external_id: external_id.to_string(),
                })
            }
            _ => None,
        };
        Ok(TagSettings {
            tag_id,
            sync_target,
            dirs: record.get(3).map(parse_dirs).unwrap_or_default(),
        })
    }

    /// Settings for `tag_id`, or defaults when no row exists yet.
    pub async fn tag_settings(&mut self, tag_id: u64) -> QueryResult<TagSettings> {
        self.read_settings(tag_id).await
    }

    async fn write_settings(&mut self, settings: &TagSettings) -> QueryResult<()> {
        let (sync_integration_id, sync_external_id) = match &settings.sync_target {
            Some(target) => (target.integration_id as i64, target.external_id.clone()),
            None => (-1, String::new()),
        };
        let dirs = serde_json::to_string(&settings.dirs).unwrap_or_else(|_| "[]".to_string());
        toasty::sql::statement(
            r#"INSERT INTO tag_settings (tag_id, sync_integration_id, sync_external_id, dirs)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT (tag_id) DO UPDATE
               SET sync_integration_id = excluded.sync_integration_id,
                   sync_external_id = excluded.sync_external_id,
                   dirs = excluded.dirs"#,
        )
        .bind(settings.tag_id as i64)
        .bind(sync_integration_id)
        .bind(sync_external_id)
        .bind(dirs)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "save tag settings",
        })?;
        Ok(())
    }

    /// Point a tag at a remote object. Clears when `target` is `None`.
    pub async fn set_tag_sync_target(
        &mut self,
        tag_id: u64,
        target: Option<SyncTarget>,
    ) -> QueryResult<()> {
        let mut settings = self.read_settings(tag_id).await?;
        settings.sync_target = target;
        self.write_settings(&settings).await
    }

    /// Replace the directory list backing a dir-project tag.
    pub async fn set_tag_dirs(
        &mut self,
        tag_id: u64,
        dirs: Vec<String>,
    ) -> QueryResult<()> {
        let mut settings = self.read_settings(tag_id).await?;
        settings.dirs = dirs;
        self.write_settings(&settings).await
    }

    /// Add one directory to a dir-project tag (no duplicates).
    pub async fn add_tag_dir(&mut self, tag_id: u64, dir: String) -> QueryResult<()> {
        let mut settings = self.read_settings(tag_id).await?;
        if !settings.dirs.contains(&dir) {
            settings.dirs.push(dir);
            self.write_settings(&settings).await?;
        }
        Ok(())
    }

    /// Remove one directory from a dir-project tag.
    pub async fn remove_tag_dir(&mut self, tag_id: u64, dir: &str) -> QueryResult<()> {
        let mut settings = self.read_settings(tag_id).await?;
        if settings.dirs.iter().any(|d| d == dir) {
            settings.dirs.retain(|d| d != dir);
            self.write_settings(&settings).await?;
        }
        Ok(())
    }

    fn parse_section(row: &toasty::stmt::Value) -> Option<TagSection> {
        let toasty::stmt::Value::Record(record) = row else {
            return None;
        };
        Some(TagSection {
            id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
            tag_id: record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
            name: record.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            position: record.get(3).and_then(|v| v.to_i64()).unwrap_or(0),
        })
    }

    /// Sections of a tag in display order.
    pub async fn tag_sections(&mut self, tag_id: u64) -> QueryResult<Vec<TagSection>> {
        let rows = toasty::sql::query(
            r#"SELECT id, tag_id, name, position FROM tag_sections
               WHERE tag_id = ?1 ORDER BY position, id"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
        ])
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list tag sections",
        })?;
        Ok(rows.iter().filter_map(Self::parse_section).collect())
    }

    /// Create a section (appended at the end), or return the existing one
    /// when the name is already taken within the tag.
    pub async fn add_tag_section(
        &mut self,
        tag_id: u64,
        name: String,
    ) -> QueryResult<TagSection> {
        if let Some(existing) = self
            .tag_sections(tag_id)
            .await?
            .into_iter()
            .find(|s| s.name == name)
        {
            return Ok(existing);
        }
        let position = self
            .tag_sections(tag_id)
            .await?
            .into_iter()
            .map(|s| s.position)
            .max()
            .unwrap_or(-1)
            + 1;
        toasty::sql::statement(
            r#"INSERT INTO tag_sections (tag_id, name, position) VALUES (?1, ?2, ?3)"#,
        )
        .bind(tag_id as i64)
        .bind(&name)
        .bind(position)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "add tag section",
        })?;
        let id = self.last_section_id().await?;
        Ok(TagSection {
            id,
            tag_id,
            name,
            position,
        })
    }

    async fn last_section_id(&mut self) -> QueryResult<u64> {
        let rows = toasty::sql::query("SELECT last_insert_rowid()")
            .column_types([toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "last section id",
            })?;
        match rows.first() {
            Some(toasty::stmt::Value::Record(record)) => Ok(record
                .first()
                .and_then(|v| v.to_i64())
                .unwrap_or(0) as u64),
            _ => Ok(0),
        }
    }

    /// Rename a section within its tag.
    pub async fn rename_tag_section(
        &mut self,
        section_id: u64,
        name: String,
    ) -> QueryResult<()> {
        toasty::sql::statement(r#"UPDATE tag_sections SET name = ?1 WHERE id = ?2"#)
            .bind(&name)
            .bind(section_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "rename tag section",
            })?;
        Ok(())
    }

    /// Reorder a tag's sections to the given id sequence.
    pub async fn reorder_tag_sections(
        &mut self,
        tag_id: u64,
        ordered_ids: &[u64],
    ) -> QueryResult<()> {
        for (position, id) in ordered_ids.iter().enumerate() {
            toasty::sql::statement(
                r#"UPDATE tag_sections SET position = ?1 WHERE id = ?2 AND tag_id = ?3"#,
            )
            .bind(position as i64)
            .bind(*id as i64)
            .bind(tag_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "reorder tag sections",
            })?;
        }
        Ok(())
    }

    /// Delete a section. Tasks are never section-linked directly (sections
    /// group tags, not tasks), so nothing else needs rewiring.
    pub async fn remove_tag_section(&mut self, section_id: u64) -> QueryResult<()> {
        toasty::sql::statement(r#"DELETE FROM tag_sections WHERE id = ?1"#)
            .bind(section_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "remove tag section",
            })?;
        Ok(())
    }

    /// Group tasks under the sections of `tag_id` for sectioned display.
    ///
    /// A task belongs to the first (by child-tag id) direct tag that is a
    /// child of `tag_id` — Todoist sections import as exactly such child
    /// tags. Returns the section names in display order (`tag_sections`
    /// position first, then any remaining alphabetically) plus the
    /// task-id → section-name map. Tasks with no section tag are absent
    /// from the map and render above the first header.
    pub async fn section_groups_for_tasks(
        &mut self,
        tag_id: u64,
        task_ids: &[u64],
    ) -> QueryResult<(Vec<String>, HashMap<u64, String>)> {
        let children = self.get_children(tag_id).await?;
        if children.is_empty() || task_ids.is_empty() {
            return Ok((Vec::new(), HashMap::new()));
        }
        let child_ids: HashSet<u64> = children.iter().map(|t| t.id).collect();
        let label_by_id: HashMap<u64, String> =
            children.into_iter().map(|t| (t.id, t.label())).collect();

        let id_list: Vec<String> = task_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let rows = toasty::sql::query(format!(
            "SELECT task_id, tag_id FROM direct_task_tags WHERE task_id IN ({}) ORDER BY tag_id",
            placeholders.join(",")
        ))
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "direct tags for section grouping",
        })?;

        // ORDER BY tag_id keeps the first matching child tag per task.
        let mut task_section: HashMap<u64, String> = HashMap::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let task = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let tag = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                if child_ids.contains(&tag) && !task_section.contains_key(&task) {
                    if let Some(label) = label_by_id.get(&tag) {
                        task_section.insert(task, label.clone());
                    }
                }
            }
        }
        if task_section.is_empty() {
            return Ok((Vec::new(), HashMap::new()));
        }

        let mut used: Vec<String> = task_section.values().cloned().collect();
        used.sort();
        used.dedup();
        let ordered_rows = self.tag_sections(tag_id).await?;
        let mut order: Vec<String> = ordered_rows
            .into_iter()
            .map(|s| s.name)
            .filter(|name| used.contains(name))
            .collect();
        for name in used {
            if !order.contains(&name) {
                order.push(name);
            }
        }
        Ok((order, task_section))
    }
}

#[cfg(test)]
mod tests {
    use crate::TodoStore;

    #[tokio::test]
    async fn test_tag_settings_defaults() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag = storage.create_tag("Work").await?;
        let settings = storage.tag_settings(tag.id).await?;
        assert_eq!(settings.tag_id, tag.id);
        assert!(settings.sync_target.is_none());
        assert!(settings.dirs.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_tag_sync_target_roundtrip() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag = storage.create_tag("Work").await?;
        let integration = storage.create_integration("todoist", None).await?;

        storage
            .set_tag_sync_target(
                tag.id,
                Some(crate::SyncTarget {
                    integration_id: integration.id,
                    external_id: "proj-1".to_string(),
                }),
            )
            .await?;
        let settings = storage.tag_settings(tag.id).await?;
        let target = settings.sync_target.unwrap();
        assert_eq!(target.integration_id, integration.id);
        assert_eq!(target.external_id, "proj-1");

        storage.set_tag_sync_target(tag.id, None).await?;
        assert!(storage.tag_settings(tag.id).await?.sync_target.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_tag_dirs_add_remove() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag = storage.create_tag("api").await?;

        storage
            .set_tag_dirs(tag.id, vec!["/a/api".to_string(), "/b/api".to_string()])
            .await?;
        assert_eq!(storage.tag_settings(tag.id).await?.dirs.len(), 2);

        storage.add_tag_dir(tag.id, "/a/api".to_string()).await?;
        assert_eq!(storage.tag_settings(tag.id).await?.dirs.len(), 2);

        storage.add_tag_dir(tag.id, "/c/api".to_string()).await?;
        assert_eq!(storage.tag_settings(tag.id).await?.dirs.len(), 3);

        storage.remove_tag_dir(tag.id, "/b/api").await?;
        assert_eq!(
            storage.tag_settings(tag.id).await?.dirs,
            vec!["/a/api".to_string(), "/c/api".to_string()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_tag_sections_crud() -> anyhow::Result<()> {        let mut storage = TodoStore::for_test().await?;
        let tag = storage.create_tag("Work").await?;

        assert!(storage.tag_sections(tag.id).await?.is_empty());

        let first = storage.add_tag_section(tag.id, "Backlog".to_string()).await?;
        let second = storage.add_tag_section(tag.id, "Doing".to_string()).await?;
        // Duplicate names return the existing section.
        let again = storage.add_tag_section(tag.id, "Backlog".to_string()).await?;
        assert_eq!(again.id, first.id);

        let sections = storage.tag_sections(tag.id).await?;
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].name, "Backlog");
        assert_eq!(sections[1].name, "Doing");

        storage.reorder_tag_sections(tag.id, &[second.id, first.id]).await?;
        let sections = storage.tag_sections(tag.id).await?;
        assert_eq!(sections[0].name, "Doing");

        storage.rename_tag_section(first.id, "Icebox".to_string()).await?;
        let sections = storage.tag_sections(tag.id).await?;
        assert!(sections.iter().any(|s| s.name == "Icebox"));

        // Sections are per tag: another tag starts empty.
        let other = storage.create_tag("Home").await?;
        assert!(storage.tag_sections(other.id).await?.is_empty());

        storage.remove_tag_section(second.id).await?;
        assert_eq!(storage.tag_sections(tag.id).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_section_groups_for_tasks() -> anyhow::Result<()> {
        use crate::prelude::*;
        let mut storage = TodoStore::for_test().await?;
        let project = storage.create_tag("Work").await?;
        let backlog = storage.create_tag("Backlog").await?;
        let doing = storage.create_tag("Doing").await?;
        storage.add_tag_implication(backlog.id, project.id).await?;
        storage.add_tag_implication(doing.id, project.id).await?;
        storage.add_tag_section(project.id, "Doing".to_string()).await?;
        storage.add_tag_section(project.id, "Backlog".to_string()).await?;

        let unsectioned = storage
            .create_task(Task::create().title("Loose"))
            .await?;
        storage.assign_tag_to_task(unsectioned.id, &project.name).await?;
        let first = storage.create_task(Task::create().title("One")).await?;
        storage.assign_tag_to_task(first.id, &backlog.name).await?;
        let second = storage.create_task(Task::create().title("Two")).await?;
        storage.assign_tag_to_task(second.id, &doing.name).await?;

        let ids = vec![unsectioned.id, first.id, second.id];
        let (order, map) = storage.section_groups_for_tasks(project.id, &ids).await?;
        // Display order follows tag_sections position, not tag id order.
        assert_eq!(order, vec!["Doing".to_string(), "Backlog".to_string()]);
        assert_eq!(map.get(&first.id).map(String::as_str), Some("Backlog"));
        assert_eq!(map.get(&second.id).map(String::as_str), Some("Doing"));
        assert!(!map.contains_key(&unsectioned.id));

        // Tags without section children group nothing.
        let plain = storage.create_tag("Plain").await?;
        let (order, map) = storage.section_groups_for_tasks(plain.id, &ids).await?;
        assert!(order.is_empty() && map.is_empty());
        Ok(())
    }
}

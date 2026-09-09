use crate::{QueryResult, TodoStore};
use derive_entity_id::EntityId;
use snafu::ResultExt;
use std::collections::{HashMap, HashSet, VecDeque};
use toasty::Model;

#[derive(EntityId, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
#[entity_id(prefix = "tag")]
pub struct TagId(u64);

#[derive(Debug, Clone, Model)]
pub struct Tag {
    #[key]
    #[auto]
    pub id: u64,
    #[unique]
    pub name: String,
    /// Optional human-facing label. Tags derived from folder paths use a
    /// unique opaque `name` (so different directories never collide) and
    /// carry the directory name here for display.
    pub display_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TagNode {
    pub id: u64,
    pub name: String,
    pub children: Vec<TagNode>,
}

fn parse_tag_row(record: &toasty::stmt::Value) -> Option<Tag> {
    if let toasty::stmt::Value::Record(record) = record {
        let id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
        let name = record
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let display_name = record
            .get(2)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Some(Tag {
            id,
            name,
            display_name,
        })
    } else {
        None
    }
}

impl TodoStore {
    pub async fn create_tag(&mut self, name: impl Into<String>) -> QueryResult<Tag> {
        self.create_tag_with_display_name(name, None).await
    }

    pub async fn create_tag_with_display_name(
        &mut self,
        name: impl Into<String>,
        display_name: Option<String>,
    ) -> QueryResult<Tag> {
        let name = name.into();
        let tag = Tag::create()
            .name(name.clone())
            .display_name(display_name)
            .exec(&mut self.db)
            .await
            .context(crate::error::CreateTagSnafu { name })?;
        Ok(tag)
    }

    /// Get or create the tag backing a local project folder. The tag name is
    /// derived from the absolute folder path so it is unique per directory
    /// (two different repos both named `api` never collide), while the
    /// display name stays the plain directory name.
    pub async fn get_or_create_project_tag(
        &mut self,
        path: &std::path::Path,
    ) -> QueryResult<Tag> {
        let name = format!(
            "project:{}",
            path.canonicalize()
                .unwrap_or_else(|_| path.to_path_buf())
                .display()
        );
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        if let Some(tag) = self.get_tag_by_name(&name).await? {
            return Ok(tag);
        }
        self.create_tag_with_display_name(name, Some(display_name))
            .await
    }

    pub async fn get_tag(&mut self, id: u64) -> QueryResult<Tag> {
        Tag::get_by_id(&mut self.db, id)
            .await
            .context(crate::error::GetTagSnafu { id })
    }

    pub async fn get_tag_by_name(&mut self, name: &str) -> QueryResult<Option<Tag>> {
        let rows =
            toasty::sql::query(r#"SELECT id, name, display_name FROM tags WHERE LOWER(name) = LOWER(?1)"#)
                .column_types([
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                ])
                .bind(name)
                .exec(&mut self.db)
                .await
                .context(crate::error::FindTagByNameSnafu {
                    name: name.to_string(),
                })?;

        Ok(rows.into_iter().next().and_then(|row| parse_tag_row(&row)))
    }

    pub async fn list_tags(&mut self) -> QueryResult<Vec<Tag>> {
        let tags = Tag::all()
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list all tags",
            })?;
        Ok(tags)
    }

    pub async fn delete_tag(&mut self, id: u64) -> QueryResult<()> {
        Tag::delete_by_id(&mut self.db, id)
            .await
            .context(crate::error::DeleteTagSnafu { id })?;
        Ok(())
    }

    pub async fn add_tag_implication(
        &mut self,
        implier_id: u64,
        implied_id: u64,
    ) -> QueryResult<()> {
        if implier_id == implied_id {
            return Err(crate::QueryErr::UnexpectedValue {
                message: "A tag cannot imply itself".to_string(),
            });
        }

        let existing = toasty::sql::query(
            r#"SELECT 1 FROM tag_implications WHERE implier_id = ?1 AND implied_id = ?2"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(implier_id as i64)
        .bind(implied_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::AddTagImplicationSnafu {
            implier_id,
            implied_id,
        })?;

        if existing.is_empty() {
            if self.would_create_cycle(implier_id, implied_id).await? {
                return Err(crate::QueryErr::UnexpectedValue {
                    message: format!(
                        "Adding implication {}->{} would create a cycle",
                        implier_id, implied_id
                    ),
                });
            }

            toasty::sql::statement(
                r#"INSERT INTO tag_implications (implier_id, implied_id) VALUES (?1, ?2)"#,
            )
            .bind(implier_id as i64)
            .bind(implied_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::AddTagImplicationSnafu {
                implier_id,
                implied_id,
            })?;
        }

        Ok(())
    }

    async fn would_create_cycle(&mut self, implier_id: u64, implied_id: u64) -> QueryResult<bool> {
        let rows = toasty::sql::query(r#"SELECT implier_id, implied_id FROM tag_implications"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "check for cycles",
            })?;

        let mut graph: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let from = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let to = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                graph.entry(from).or_default().push(to);
            }
        }

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        if let Some(children) = graph.get(&implied_id) {
            for &child in children {
                if child == implier_id {
                    return Ok(true);
                }
                queue.push_back(child);
            }
        }

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current) {
                continue;
            }
            if let Some(children) = graph.get(&current) {
                for &child in children {
                    if child == implier_id {
                        return Ok(true);
                    }
                    queue.push_back(child);
                }
            }
        }

        Ok(false)
    }

    pub async fn remove_tag_implication(
        &mut self,
        implier_id: u64,
        implied_id: u64,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"DELETE FROM tag_implications WHERE implier_id = ?1 AND implied_id = ?2"#,
        )
        .bind(implier_id as i64)
        .bind(implied_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::RemoveTagImplicationSnafu {
            implier_id,
            implied_id,
        })?;
        Ok(())
    }

    pub async fn get_top_level_tags(&mut self) -> QueryResult<Vec<Tag>> {
        let rows = toasty::sql::query(
            r#"
            SELECT t.id, t.name, t.display_name
            FROM tags t
            WHERE t.id NOT IN (SELECT implier_id FROM tag_implications)
            "#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "get top-level tags",
        })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }

    pub async fn get_children(&mut self, parent_id: u64) -> QueryResult<Vec<Tag>> {
        let rows = toasty::sql::query(
            r#"
            SELECT t.id, t.name, t.display_name
            FROM tags t
            JOIN tag_implications ti ON ti.implier_id = t.id
            WHERE ti.implied_id = ?1
            "#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(parent_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: format!("get children of tag {}", parent_id),
        })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }

    pub async fn get_parents(&mut self, child_id: u64) -> QueryResult<Vec<Tag>> {
        let rows = toasty::sql::query(
            r#"
            SELECT t.id, t.name, t.display_name
            FROM tags t
            JOIN tag_implications ti ON ti.implied_id = t.id
            WHERE ti.implier_id = ?1
            "#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(child_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: format!("get parents of tag {}", child_id),
        })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }

    pub async fn assign_tag_to_task(&mut self, task_id: u64, tag_name: &str) -> QueryResult<()> {
        let lower = tag_name.to_lowercase();

        let already_assigned = toasty::sql::query(
            r#"SELECT 1 FROM direct_task_tags dtt
               JOIN tags t ON t.id = dtt.tag_id
               WHERE dtt.task_id = ?1 AND LOWER(t.name) = ?2"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(task_id as i64)
        .bind(lower.as_str())
        .exec(&mut self.db)
        .await
        .context(crate::error::AssignTagToTaskSnafu {
            task_id,
            tag_name: tag_name.to_string(),
        })?;

        if already_assigned.is_empty() {
            let tag = match self.get_tag_by_name(tag_name).await? {
                Some(tag) => tag,
                None => self.create_tag(tag_name).await?,
            };

            toasty::sql::statement(
                r#"INSERT INTO direct_task_tags (task_id, tag_id) VALUES (?1, ?2)"#,
            )
            .bind(task_id as i64)
            .bind(tag.id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::AssignTagToTaskSnafu {
                task_id,
                tag_name: tag_name.to_string(),
            })?;
        }

        Ok(())
    }

    pub async fn remove_tag_from_task(&mut self, task_id: u64, tag_id: u64) -> QueryResult<()> {
        toasty::sql::statement(
            r#"DELETE FROM direct_task_tags WHERE task_id = ?1 AND tag_id = ?2"#,
        )
        .bind(task_id as i64)
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::RemoveTagFromTaskSnafu { task_id, tag_id })?;
        Ok(())
    }

    pub async fn get_direct_task_tags(&mut self, task_id: u64) -> QueryResult<Vec<Tag>> {
        let rows = toasty::sql::query(
            r#"
            SELECT t.id, t.name, t.display_name
            FROM tags t
            JOIN direct_task_tags dtt ON dtt.tag_id = t.id
            WHERE dtt.task_id = ?1
            "#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::LoadTaskTagsSnafu { task_id })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }

    pub async fn get_inferred_task_tags(&mut self, task_id: u64) -> QueryResult<Vec<Tag>> {
        let direct_rows =
            toasty::sql::query(r#"SELECT tag_id FROM direct_task_tags WHERE task_id = ?1"#)
                .column_types([toasty::stmt::Type::I64])
                .bind(task_id as i64)
                .exec(&mut self.db)
                .await
                .context(crate::error::LoadTaskTagsSnafu { task_id })?;

        let direct_tag_ids: Vec<u64> = direct_rows
            .iter()
            .filter_map(|row| {
                if let toasty::stmt::Value::Record(record) = row {
                    record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
                } else {
                    None
                }
            })
            .collect();

        let imp_rows = toasty::sql::query(r#"SELECT implier_id, implied_id FROM tag_implications"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: format!("load inferred tags for task {}", task_id),
            })?;

        let mut graph: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in imp_rows {
            if let toasty::stmt::Value::Record(record) = row {
                let from = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let to = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                graph.entry(from).or_default().push(to);
            }
        }

        let mut all_ids: HashSet<u64> = direct_tag_ids.iter().copied().collect();
        let mut queue: VecDeque<u64> = direct_tag_ids.into();

        while let Some(current) = queue.pop_front() {
            if let Some(parents) = graph.get(&current) {
                for &parent in parents {
                    if all_ids.insert(parent) {
                        queue.push_back(parent);
                    }
                }
            }
        }

        if all_ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_list: Vec<String> = all_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let query = format!(
            "SELECT t.id, t.name, t.display_name FROM tags t WHERE t.id IN ({})",
            placeholders.join(",")
        );

        let rows = toasty::sql::query(&query)
            .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: format!("fetch inferred tag details for task {}", task_id),
            })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }

    pub async fn get_all_descendants(&mut self, tag_id: u64) -> QueryResult<Vec<Tag>> {
        let imp_rows = toasty::sql::query(r#"SELECT implier_id, implied_id FROM tag_implications"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: format!("load implications for descendants of tag {}", tag_id),
            })?;

        let mut children_map: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in imp_rows {
            if let toasty::stmt::Value::Record(record) = row {
                let child = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let parent = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                children_map.entry(parent).or_default().push(child);
            }
        }

        let mut all_ids: HashSet<u64> = HashSet::new();
        let mut queue: VecDeque<u64> = VecDeque::new();

        if let Some(direct_children) = children_map.get(&tag_id) {
            for &child in direct_children {
                if all_ids.insert(child) {
                    queue.push_back(child);
                }
            }
        }

        while let Some(current) = queue.pop_front() {
            if let Some(children) = children_map.get(&current) {
                for &child in children {
                    if all_ids.insert(child) {
                        queue.push_back(child);
                    }
                }
            }
        }

        if all_ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_list: Vec<String> = all_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let query = format!(
            "SELECT t.id, t.name, t.display_name FROM tags t WHERE t.id IN ({})",
            placeholders.join(",")
        );

        let rows = toasty::sql::query(&query)
            .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: format!("fetch descendant tag details for tag {}", tag_id),
            })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use crate::TodoStore;
    use crate::prelude::*;

    #[tokio::test]
    async fn test_create_tag() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let tag = storage.create_tag("Python").await?;
        assert_eq!(tag.name, "Python");

        let retrieved = storage.get_tag(tag.id).await?;
        assert_eq!(retrieved.name, "Python");

        Ok(())
    }

    #[tokio::test]
    async fn test_get_or_create_project_tag() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let path = std::path::Path::new("/tmp/somewhere/api");
        let tag = storage.get_or_create_project_tag(path).await?;
        // Unique name derived from the path, display name is the dir name.
        assert_eq!(tag.name, "project:/tmp/somewhere/api");
        assert_eq!(tag.display_name.as_deref(), Some("api"));

        // Same path returns the existing tag (no duplicate).
        let again = storage.get_or_create_project_tag(path).await?;
        assert_eq!(again.id, tag.id);
        assert_eq!(storage.list_tags().await?.len(), 1);

        // A different path with the same dir name does not collide.
        let other = storage.get_or_create_project_tag(std::path::Path::new("/tmp/elsewhere/api")).await?;
        assert_ne!(other.id, tag.id);
        assert_eq!(other.name, "project:/tmp/elsewhere/api");
        assert_eq!(other.display_name.as_deref(), Some("api"));

        Ok(())
    }

    #[tokio::test]
    async fn test_project_tag_display_name_survives_list_queries() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        // A project tag: opaque unique name + dir-name display label.
        storage
            .get_or_create_project_tag(std::path::Path::new("/Users/me/dev/about-me"))
            .await?;

        // Every SQL-backed read must return the display name (not the raw
        // `project:/...` name) so UIs can show the short label.
        let top = storage.get_top_level_tags().await?;
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].display_name.as_deref(), Some("about-me"));

        let by_name = storage.get_tag_by_name("project:/Users/me/dev/about-me").await?;
        assert_eq!(by_name.unwrap().display_name.as_deref(), Some("about-me"));

        let all = storage.list_tags().await?;
        assert_eq!(all[0].display_name.as_deref(), Some("about-me"));

        Ok(())
    }

    #[tokio::test]
    async fn test_task_assigned_to_project_tag_appears_in_tag_list() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let tag = storage
            .get_or_create_project_tag(std::path::Path::new("/Users/me/dev/about-me"))
            .await?;

        let task = storage
            .create_task(Task::create().title("Improve about-me"))
            .await?;
        // Assign by the opaque project tag name, exactly like the app does
        // when a task is added under a selected project tag.
        storage.assign_tag_to_task(task.id, &tag.name).await?;

        let tasks = storage.list_tasks_by_tag(tag.id).await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
        assert_eq!(tasks[0].direct_tags, vec![tag.name.clone()]);
        assert_eq!(tasks[0].inferred_tags, vec![tag.name]);

        Ok(())
    }

    #[tokio::test]
    async fn test_add_tag_implication() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let python = storage.create_tag("Python").await?;
        let programming = storage.create_tag("Programming").await?;

        storage
            .add_tag_implication(python.id, programming.id)
            .await?;

        let parents = storage.get_parents(python.id).await?;
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0].id, programming.id);

        Ok(())
    }

    #[tokio::test]
    async fn test_add_self_implication_fails() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let tag = storage.create_tag("Self").await?;
        let result = storage.add_tag_implication(tag.id, tag.id).await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_prevent_cycle() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let a = storage.create_tag("A").await?;
        let b = storage.create_tag("B").await?;
        let c = storage.create_tag("C").await?;

        storage.add_tag_implication(a.id, b.id).await?;
        storage.add_tag_implication(b.id, c.id).await?;

        let result = storage.add_tag_implication(c.id, a.id).await;
        assert!(result.is_err());

        Ok(())
    }

    #[tokio::test]
    async fn test_cascade_delete_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(Task::create().title("Test task".to_string()))
            .await?;

        let tag = storage.create_tag("Python").await?;
        storage.assign_tag_to_task(task.id, &tag.name).await?;

        let tags = storage.get_direct_task_tags(task.id).await?;
        assert_eq!(tags.len(), 1);

        storage.delete_task(task.id).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_get_top_level_tags() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let cs = storage.create_tag("CS").await?;
        let programming = storage.create_tag("Programming").await?;
        let python = storage.create_tag("Python").await?;

        storage
            .add_tag_implication(python.id, programming.id)
            .await?;
        storage.add_tag_implication(programming.id, cs.id).await?;

        let top = storage.get_top_level_tags().await?;
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].id, cs.id);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_children() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let programming = storage.create_tag("Programming").await?;
        let python = storage.create_tag("Python").await?;
        let java = storage.create_tag("Java").await?;

        storage
            .add_tag_implication(python.id, programming.id)
            .await?;
        storage.add_tag_implication(java.id, programming.id).await?;

        let children = storage.get_children(programming.id).await?;
        assert_eq!(children.len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_task_tag_inference() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let cs = storage.create_tag("CS").await?;
        let programming = storage.create_tag("Programming").await?;
        let python = storage.create_tag("Python").await?;

        storage
            .add_tag_implication(python.id, programming.id)
            .await?;
        storage.add_tag_implication(programming.id, cs.id).await?;

        let task = storage
            .create_task(Task::create().title("Learn Python".to_string()))
            .await?;

        storage.assign_tag_to_task(task.id, &python.name).await?;

        let inferred = storage.get_inferred_task_tags(task.id).await?;
        assert_eq!(inferred.len(), 3);

        let names: Vec<String> = inferred.iter().map(|t| t.name.clone()).collect();
        assert!(names.contains(&"Python".to_string()));
        assert!(names.contains(&"Programming".to_string()));
        assert!(names.contains(&"CS".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_paths_dag() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let react = storage.create_tag("React").await?;
        let frontend = storage.create_tag("Frontend").await?;
        let javascript = storage.create_tag("JavaScript").await?;

        storage.add_tag_implication(react.id, frontend.id).await?;
        storage.add_tag_implication(react.id, javascript.id).await?;

        let task = storage
            .create_task(Task::create().title("React project".to_string()))
            .await?;

        storage.assign_tag_to_task(task.id, &react.name).await?;

        let inferred = storage.get_inferred_task_tags(task.id).await?;
        assert_eq!(inferred.len(), 3);

        let names: Vec<String> = inferred.iter().map(|t| t.name.clone()).collect();
        assert!(names.contains(&"React".to_string()));
        assert!(names.contains(&"Frontend".to_string()));
        assert!(names.contains(&"JavaScript".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks_by_tag_multi_level() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let programming = storage.create_tag("Programming").await?;
        let frontend = storage.create_tag("Frontend").await?;
        let react = storage.create_tag("React").await?;
        let backend = storage.create_tag("Backend").await?;
        let rust = storage.create_tag("Rust").await?;

        storage.add_tag_implication(react.id, frontend.id).await?;
        storage
            .add_tag_implication(frontend.id, programming.id)
            .await?;
        storage.add_tag_implication(rust.id, backend.id).await?;
        storage
            .add_tag_implication(backend.id, programming.id)
            .await?;

        let task_react = storage
            .create_task(Task::create().title("React app".to_string()))
            .await?;
        storage
            .assign_tag_to_task(task_react.id, &react.name)
            .await?;

        let task_rust = storage
            .create_task(Task::create().title("Rust CLI".to_string()))
            .await?;
        storage.assign_tag_to_task(task_rust.id, &rust.name).await?;

        let task_python = storage
            .create_task(Task::create().title("Python script".to_string()))
            .await?;
        let python = storage.create_tag("Python").await?;
        storage
            .assign_tag_to_task(task_python.id, &python.name)
            .await?;

        let programming_tasks = storage.list_tasks_by_tag(programming.id).await?;
        assert_eq!(programming_tasks.len(), 2);
        let titles: Vec<&str> = programming_tasks.iter().map(|t| t.title.as_str()).collect();
        assert!(titles.contains(&"React app"));
        assert!(titles.contains(&"Rust CLI"));

        let frontend_tasks = storage.list_tasks_by_tag(frontend.id).await?;
        assert_eq!(frontend_tasks.len(), 1);
        assert_eq!(frontend_tasks[0].title, "React app");

        let backend_tasks = storage.list_tasks_by_tag(backend.id).await?;
        assert_eq!(backend_tasks.len(), 1);
        assert_eq!(backend_tasks[0].title, "Rust CLI");

        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks_by_tag_populates_inferred_tags() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let programming = storage.create_tag("Programming").await?;
        let frontend = storage.create_tag("Frontend").await?;
        let react = storage.create_tag("React").await?;

        storage.add_tag_implication(react.id, frontend.id).await?;
        storage
            .add_tag_implication(frontend.id, programming.id)
            .await?;

        let task = storage
            .create_task(Task::create().title("React app".to_string()))
            .await?;
        storage.assign_tag_to_task(task.id, &react.name).await?;

        let programming_tasks = storage.list_tasks_by_tag(programming.id).await?;
        assert_eq!(programming_tasks.len(), 1);

        let task_with_meta = &programming_tasks[0];
        assert_eq!(task_with_meta.title, "React app");
        assert_eq!(task_with_meta.inferred_tags.len(), 3);
        assert!(task_with_meta.inferred_tags.contains(&"React".to_string()));
        assert!(
            task_with_meta
                .inferred_tags
                .contains(&"Frontend".to_string())
        );
        assert!(
            task_with_meta
                .inferred_tags
                .contains(&"Programming".to_string())
        );

        let react_tasks = storage.list_tasks_by_tag(react.id).await?;
        assert_eq!(react_tasks.len(), 1);
        assert_eq!(react_tasks[0].inferred_tags.len(), 3);

        Ok(())
    }
}

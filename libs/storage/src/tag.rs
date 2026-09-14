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

impl Tag {
    /// User-facing label: display name when set, otherwise the plain name.
    pub fn label(&self) -> String {
        self.display_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.name.clone())
    }

    /// Project tags back a local folder: unique `project:{path}` name with
    /// the directory name as display label.
    pub fn is_project(&self) -> bool {
        self.name.starts_with("project:")
    }
}

#[derive(Debug, Clone)]
pub struct TagNode {
    pub id: u64,
    pub name: String,
    pub children: Vec<TagNode>,
}

/// How deep the render walk descends before it stops. The store refuses to
/// create a cycle, but one that reached the table another way must still not
/// hang a render, so depth is bounded in addition to the per-path guard.
const MAX_TREE_DEPTH: usize = 32;

/// One tag in the nav tree, expanded and already in render order.
#[derive(Debug, Clone)]
pub struct TagTreeNode {
    pub tag: Tag,
    /// Backs a local directory: folder icon, agent pane, app-binding exclusion.
    pub is_directory_backed: bool,
    /// A section of the parent it hangs under (its label matches one of that
    /// parent's `tag_sections`), rather than a placed project or child tag.
    pub is_section: bool,
    pub children: Vec<TagTreeNode>,
}

impl TagTreeNode {
    /// Depth-first rows in render order. A tag with several parents appears
    /// once per path.
    pub fn flatten(&self) -> Vec<TagTreeRow> {
        let mut rows = Vec::new();
        let mut path = Vec::new();
        self.push_rows(0, &mut path, &mut rows);
        rows
    }

    fn push_rows(&self, depth: usize, path: &mut Vec<String>, out: &mut Vec<TagTreeRow>) {
        path.push(self.tag.name.clone());
        out.push(TagTreeRow {
            depth,
            path: path.clone(),
            tag: self.tag.clone(),
            is_directory_backed: self.is_directory_backed,
            is_section: self.is_section,
        });
        for child in &self.children {
            child.push_rows(depth + 1, path, out);
        }
        path.pop();
    }
}

/// One nav row: a tag plus where it sits in the tree. `path` is the ancestor
/// chain of tag *names*, which is what selecting the row navigates by.
#[derive(Debug, Clone)]
pub struct TagTreeRow {
    pub tag: Tag,
    /// Indent depth; 0 for a top-level tag.
    pub depth: usize,
    pub path: Vec<String>,
    pub is_directory_backed: bool,
    pub is_section: bool,
}

/// In-memory view of the implication DAG used to build one `tag_tree` result.
struct TreeBuild<'a> {
    by_id: &'a HashMap<u64, Tag>,
    children: &'a HashMap<u64, Vec<u64>>,
    sections: &'a HashMap<u64, Vec<String>>,
    directory_backed: &'a HashSet<u64>,
}

impl TreeBuild<'_> {
    /// Projects first, then plain tags, then sections; each group by label
    /// (case-insensitively), finally by id so the order is total.
    fn order_key(&self, parent: Option<u64>, id: u64) -> (u8, String, u64) {
        let group = if self.directory_backed.contains(&id) {
            0
        } else if parent.is_some_and(|parent| self.is_section_of(parent, id)) {
            2
        } else {
            1
        };
        let label = self
            .by_id
            .get(&id)
            .map(|tag| tag.label().to_lowercase())
            .unwrap_or_default();
        (group, label, id)
    }

    /// A child tag is a section of `parent` when its label matches a section
    /// name on that parent — the same rule `section_child_tag` uses.
    fn is_section_of(&self, parent: u64, id: u64) -> bool {
        let Some(tag) = self.by_id.get(&id) else {
            return false;
        };
        let label = tag.label();
        self.sections
            .get(&parent)
            .is_some_and(|names| names.iter().any(|name| name == &label))
    }

    fn node(
        &self,
        id: u64,
        parent: Option<u64>,
        path: &mut HashSet<u64>,
        depth: usize,
    ) -> Option<TagTreeNode> {
        let tag = self.by_id.get(&id)?.clone();
        // Per-path guard: the same tag repeats under different parents on
        // purpose, so only the current branch is checked, not every id already
        // emitted somewhere else in the tree.
        if !path.insert(id) {
            return None;
        }
        let mut child_ids = self.children.get(&id).cloned().unwrap_or_default();
        child_ids.sort_by_key(|child| self.order_key(Some(id), *child));
        child_ids.dedup();
        let mut children = Vec::new();
        if depth < MAX_TREE_DEPTH {
            for child in child_ids {
                if let Some(node) = self.node(child, Some(id), path, depth + 1) {
                    children.push(node);
                }
            }
        }
        path.remove(&id);
        Some(TagTreeNode {
            is_directory_backed: self.directory_backed.contains(&id),
            is_section: parent.is_some_and(|parent| self.is_section_of(parent, id)),
            tag,
            children,
        })
    }
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

    /// Seed data tag: demo content owned by the builtin app, so it is never
    /// pushed to an integration.
    pub async fn create_seed_tag(&mut self, name: impl Into<String>) -> QueryResult<Tag> {
        let tag = self
            .create_tag_with_display_name(name, None)
            .await?;
        let demo = self.demo_app().await?;
        self.set_tag_managed(tag.id, Some(demo.id)).await?;
        Ok(tag)
    }

    /// Seed project tag: a `project:{path}` tag with a human-friendly display
    /// name. Directory-backed tags stay user-owned (an app may not manage
    /// them); the demo tasks inside carry the builtin app's ownership instead.
    pub async fn create_seed_project_tag(
        &mut self,
        path: &std::path::Path,
        label: impl Into<String>,
    ) -> QueryResult<Tag> {
        let name = format!("project:{}", path.display());
        let display_name = label.into();
        let tag = Tag::create()
            .name(name.clone())
            .display_name(Some(display_name))
            .exec(&mut self.db)
            .await
            .context(crate::error::CreateTagSnafu { name })?;
        Ok(tag)
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

    /// Delete a tag, its placement edges, and its settings.
    ///
    /// Edges are removed in *both* directions: a tag can be both placed (its
    /// `implier_id` rows) and a parent (its `implied_id` rows). Leaving the
    /// incoming rows behind makes a placed child unreachable — it is no longer
    /// top-level (it still appears as an implier) and the parent that used to
    /// render it is gone — so it would disappear from the tree entirely. Its
    /// children therefore fall back to the top level.
    ///
    /// Transactional so a failure cannot leave the tag gone but its edges
    /// behind (or the reverse). Re-entrant, so calling this from inside
    /// `disable_app`'s transaction joins that transaction rather than nesting
    /// a second `BEGIN`.
    pub async fn delete_tag(&mut self, id: u64) -> QueryResult<()> {
        self.with_transaction(|store| {
            Box::pin(async move {
                for statement in [
                    "DELETE FROM tag_implications WHERE implier_id = ?1 OR implied_id = ?1",
                    "DELETE FROM tag_sections WHERE tag_id = ?1",
                    "DELETE FROM tag_settings WHERE tag_id = ?1",
                ] {
                    toasty::sql::statement(statement)
                        .bind(id as i64)
                        .exec(&mut store.db)
                        .await
                        .context(crate::error::QueryTagsSnafu {
                            context: "delete tag cleanup",
                        })?;
                }
                Tag::delete_by_id(&mut store.db, id)
                    .await
                    .context(crate::error::DeleteTagSnafu { id })?;
                Ok(())
            })
        })
        .await
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
            ORDER BY LOWER(COALESCE(NULLIF(t.display_name, ''), t.name)), t.id
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
              AND ti.implier_id IS NOT NULL
            ORDER BY LOWER(COALESCE(NULLIF(t.display_name, ''), t.name)), t.id
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

    /// Ids of every tag that backs a local directory: a `project:{path}` name
    /// or a non-empty `tag_settings.dirs`. This is the app-facing definition of
    /// "this tag is a project" — the navbar's folder icon, the agent pane, and
    /// app-binding eligibility all use it, so a tag that only has `dirs` set is
    /// a project everywhere rather than an ordinary tag with a stray setting.
    pub async fn directory_backed_tag_ids(&mut self) -> QueryResult<HashSet<u64>> {
        let rows = toasty::sql::query(
            r#"
            SELECT t.id
            FROM tags t
            LEFT JOIN tag_settings s ON s.tag_id = t.id
            WHERE t.name LIKE 'project:%'
               OR (s.dirs IS NOT NULL AND s.dirs <> '' AND s.dirs <> '[]')
            "#,
        )
        .column_types([toasty::stmt::Type::I64])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list directory-backed tags",
        })?;
        Ok(rows
            .iter()
            .filter_map(|row| match row {
                toasty::stmt::Value::Record(record) => {
                    record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
                }
                _ => None,
            })
            .collect())
    }

    /// Whether one tag backs a local directory; the single-tag form of
    /// [`Self::directory_backed_tag_ids`].
    pub async fn tag_is_directory_backed(&mut self, tag_id: u64) -> QueryResult<bool> {
        if self.get_tag(tag_id).await?.is_project() {
            return Ok(true);
        }
        Ok(!self.tag_settings(tag_id).await?.dirs.is_empty())
    }

    /// The whole tag hierarchy, fully expanded and already in render order.
    ///
    /// The nav shows every level at once (there is no disclosure state), so the
    /// tree is built in memory from all tags, all implication edges, and all
    /// sections — three queries total, rather than one per expanded tag. A tag
    /// placed under several parents appears once per parent, each copy with its
    /// own subtree.
    pub async fn tag_tree(&mut self) -> QueryResult<Vec<TagTreeNode>> {
        let by_id: HashMap<u64, Tag> = self
            .list_tags()
            .await?
            .into_iter()
            .map(|tag| (tag.id, tag))
            .collect();
        let directory_backed = self.directory_backed_tag_ids().await?;

        let imp_rows = toasty::sql::query(r#"SELECT implier_id, implied_id FROM tag_implications"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "load tag tree edges",
            })?;
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut has_parent: HashSet<u64> = HashSet::new();
        for row in imp_rows {
            if let toasty::stmt::Value::Record(record) = row
                && let (Some(child), Some(parent)) = (
                    record.first().and_then(|v| v.to_i64()),
                    record.get(1).and_then(|v| v.to_i64()),
                )
            {
                children.entry(parent as u64).or_default().push(child as u64);
                // A tag whose parent row is missing (hand-edited database)
                // still counts as a child, so it is not also rendered as a
                // root while its edge is skipped during the walk.
                has_parent.insert(child as u64);
            }
        }

        let section_rows = toasty::sql::query(r#"SELECT tag_id, name FROM tag_sections"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::String])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "load tag tree sections",
            })?;
        let mut sections: HashMap<u64, Vec<String>> = HashMap::new();
        for row in section_rows {
            if let toasty::stmt::Value::Record(record) = row
                && let (Some(tag_id), Some(name)) = (
                    record.first().and_then(|v| v.to_i64()),
                    record.get(1).and_then(|v| v.as_str()),
                )
            {
                sections
                    .entry(tag_id as u64)
                    .or_default()
                    .push(name.to_string());
            }
        }

        let build = TreeBuild {
            by_id: &by_id,
            children: &children,
            sections: &sections,
            directory_backed: &directory_backed,
        };
        let mut roots: Vec<u64> = by_id
            .keys()
            .copied()
            .filter(|id| !has_parent.contains(id))
            .collect();
        roots.sort_by_key(|id| build.order_key(None, *id));

        let mut tree = Vec::new();
        for root in roots {
            let mut path = HashSet::new();
            if let Some(node) = build.node(root, None, &mut path, 0) {
                tree.push(node);
            }
        }
        Ok(tree)
    }

    /// [`Self::tag_tree`] flattened to render rows, each with its indent depth
    /// and the names of the path it was reached through.
    pub async fn tag_tree_rows(&mut self) -> QueryResult<Vec<TagTreeRow>> {
        Ok(self
            .tag_tree()
            .await?
            .iter()
            .flat_map(TagTreeNode::flatten)
            .collect())
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

    /// Replace a task's direct tags: create and assign the ones it does not
    /// already carry, and unassign the ones removed by the edit. Each edited
    /// label is resolved against existing tags by name or display label
    /// (case-insensitive), so unchanged labels keep their tag — including
    /// app-managed `project:` tags, whose editable text is the directory
    /// name, not the opaque `project:` name.
    pub async fn set_task_tags(&mut self, task_id: u64, tags: &[String]) -> QueryResult<()> {
        self.guard_and_mark_modified(task_id).await?;
        let desired: Vec<String> = tags
            .iter()
            .map(|tag| tag.trim().trim_start_matches('#').to_string())
            .filter(|tag| !tag.is_empty())
            .collect();

        let all = self.list_tags().await?;
        let mut kept_ids: Vec<u64> = Vec::new();
        for tag in &desired {
            let lower = tag.to_lowercase();
            let matches = all.iter().find(|candidate| {
                candidate.name.to_lowercase() == lower
                    || candidate.label().to_lowercase() == lower
            });
            match matches {
                Some(existing) if !kept_ids.contains(&existing.id) => {
                    self.assign_tag_to_task(task_id, &existing.name).await?;
                    kept_ids.push(existing.id);
                }
                Some(_) => {}
                None => {
                    let created = self.create_tag(tag.to_lowercase()).await?;
                    self.assign_tag_to_task(task_id, &created.name).await?;
                    kept_ids.push(created.id);
                }
            }
        }

        let current = self.get_direct_task_tags(task_id).await?;
        for tag in &current {
            if !kept_ids.contains(&tag.id) {
                self.remove_tag_from_task(task_id, tag.id).await?;
            }
        }
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

    /// Direct tags of the task's ancestor chain (nearest ancestor first),
    /// deduplicated. A subtask automatically inherits these on load; the
    /// chain walk is bounded by visited ids so a corrupt cycle cannot loop.
    pub async fn inherited_task_tags(&mut self, task_id: u64) -> QueryResult<Vec<Tag>> {
        let mut out: Vec<Tag> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut visited: HashSet<u64> = HashSet::new();
        let mut current_id = task_id;
        loop {
            let Some(parent_id) = self.get_task(current_id).await?.parent_id else {
                break;
            };
            if !visited.insert(parent_id) {
                break;
            }
            current_id = parent_id;
            for tag in self.get_direct_task_tags(current_id).await? {
                if seen.insert(tag.id) {
                    out.push(tag);
                }
            }
        }
        Ok(out)
    }

    pub async fn get_inferred_task_tags(&mut self, task_id: u64) -> QueryResult<Vec<Tag>> {
        let direct_rows =
            toasty::sql::query(r#"SELECT tag_id FROM direct_task_tags WHERE task_id = ?1"#)
                .column_types([toasty::stmt::Type::I64])
                .bind(task_id as i64)
                .exec(&mut self.db)
                .await
                .context(crate::error::LoadTaskTagsSnafu { task_id })?;

        let seed: HashSet<u64> = direct_rows
            .iter()
            .filter_map(|row| {
                if let toasty::stmt::Value::Record(record) = row {
                    record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
                } else {
                    None
                }
            })
            .collect();
        self.inferred_tags_from_seed(&seed).await
    }

    /// Effective tags for a seed of tag ids (direct and/or inherited): the
    /// seed plus everything implied by it through the tag DAG.
    pub(crate) async fn inferred_tags_from_seed(
        &mut self,
        seed: &HashSet<u64>,
    ) -> QueryResult<Vec<Tag>> {
        let imp_rows = toasty::sql::query(r#"SELECT implier_id, implied_id FROM tag_implications"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "load implied tags",
            })?;

        let mut graph: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in imp_rows {
            if let toasty::stmt::Value::Record(record) = row {
                let from = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let to = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                graph.entry(from).or_default().push(to);
            }
        }

        let mut all_ids: HashSet<u64> = seed.clone();
        let mut queue: VecDeque<u64> = all_ids.iter().copied().collect();
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
                context: "fetch effective tag details",
            })?;

        let mut tags = Vec::new();
        for row in rows {
            if let Some(tag) = parse_tag_row(&row) {
                tags.push(tag);
            }
        }
        Ok(tags)
    }

    /// Leaf tags from an already-computed effective tag set: the most
    /// specific tags, dropping any tag implied by another tag in the set.
    /// E.g. if "programming" implies "work", a task tagged "programming"
    /// shows only "programming".
    pub(crate) async fn leaf_tags_from_all(&mut self, all: &[Tag]) -> QueryResult<Vec<Tag>> {
        if all.len() < 2 {
            return Ok(all.to_vec());
        }
        let imp_rows = toasty::sql::query(r#"SELECT implier_id, implied_id FROM tag_implications"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "load leaf tags",
            })?;

        let mut graph: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in imp_rows {
            if let toasty::stmt::Value::Record(record) = row {
                let from = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let to = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                graph.entry(from).or_default().push(to);
            }
        }

        let ids: HashSet<u64> = all.iter().map(|t| t.id).collect();
        let mut implied: HashSet<u64> = HashSet::new();
        for tag in all {
            let mut queue: VecDeque<u64> =
                graph.get(&tag.id).cloned().unwrap_or_default().into();
            let mut seen: HashSet<u64> = HashSet::new();
            while let Some(current) = queue.pop_front() {
                if !seen.insert(current) {
                    continue;
                }
                if ids.contains(&current) {
                    implied.insert(current);
                }
                if let Some(next) = graph.get(&current) {
                    for &n in next {
                        queue.push_back(n);
                    }
                }
            }
        }

        Ok(all.iter().filter(|t| !implied.contains(&t.id)).cloned().collect())
    }

    /// Leaf tags for a task's own direct tags (plus DAG inference): kept
    /// for callers that do not want inherited-tag semantics.
    pub async fn get_leaf_task_tags(&mut self, task_id: u64) -> QueryResult<Vec<Tag>> {
        let all = self.get_inferred_task_tags(task_id).await?;
        self.leaf_tags_from_all(&all).await
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
        assert_eq!(tasks[0].direct_tags, vec![tag.label()]);
        assert_eq!(tasks[0].inferred_tags, vec![tag.label()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_set_task_tags_replaces_direct_tags() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(Task::create().title("Tagged task"))
            .await?;
        storage
            .set_task_tags(task.id, &["alpha".to_string(), "beta".to_string()])
            .await?;

        let direct = storage.get_direct_task_tags(task.id).await?;
        let names = direct.iter().map(|tag| tag.label()).collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);

        // Editing to a new set: keep beta, drop alpha, add gamma.
        storage
            .set_task_tags(
                task.id,
                &["beta".to_string(), "GAMMA".to_string(), "beta".to_string()],
            )
            .await?;
        let direct = storage.get_direct_task_tags(task.id).await?;
        let names = direct.iter().map(|tag| tag.label()).collect::<Vec<_>>();
        assert_eq!(names, vec!["beta".to_string(), "gamma".to_string()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_set_task_tags_keeps_project_tag_through_label_edit() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let project = storage
            .get_or_create_project_tag(std::path::Path::new("/Users/me/dev/about-me"))
            .await?;
        assert_eq!(project.label(), "about-me");

        let task = storage
            .create_task(Task::create().title("Directory task"))
            .await?;
        storage.set_task_tags(task.id, &[project.label()]).await?;

        // Committing the label unchanged must reuse the existing project tag,
        // not create a new plain tag named `about-me`.
        storage
            .set_task_tags(task.id, &["about-me".to_string(), "notes".to_string()])
            .await?;
        let direct = storage.get_direct_task_tags(task.id).await?;
        assert_eq!(direct.len(), 2);
        assert!(direct.iter().any(|tag| tag.is_project()));
        assert!(
            direct
                .iter()
                .any(|tag| tag.name == project.name && tag.label() == "about-me")
        );

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
    async fn test_inherited_tags_walk_ancestor_chain() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag_a = storage.create_tag("TagA").await?;
        let tag_b = storage.create_tag("TagB").await?;

        let grandparent = storage.create_task(Task::create().title("Grandparent")).await?;
        storage.assign_tag_to_task(grandparent.id, &tag_b.name).await?;
        let parent = storage
            .create_task(
                Task::create()
                    .title("Parent")
                    .parent_id(Some(grandparent.id)),
            )
            .await?;
        storage.assign_tag_to_task(parent.id, &tag_a.name).await?;
        let child = storage
            .create_task(
                Task::create()
                    .title("Child")
                    .parent_id(Some(parent.id)),
            )
            .await?;

        // Inherited tags are the ancestors' direct tags, nearest first,
        // transitive through the whole chain.
        let meta = storage.get_task_with_meta(child.id).await?;
        assert_eq!(meta.inherited_tags, vec![tag_a.label(), tag_b.label()]);
        assert!(meta.direct_tags.is_empty());

        // The parent itself inherits only from its own parent.
        let meta = storage.get_task_with_meta(parent.id).await?;
        assert_eq!(meta.inherited_tags, vec![tag_b.label()]);
        assert_eq!(meta.direct_tags, vec![tag_a.label()]);
        Ok(())
    }

    #[tokio::test]
    async fn test_inherited_tags_deduplicated() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag = storage.create_tag("Shared").await?;

        let grandparent = storage.create_task(Task::create().title("Grandparent")).await?;
        storage.assign_tag_to_task(grandparent.id, &tag.name).await?;
        let parent = storage
            .create_task(
                Task::create()
                    .title("Parent")
                    .parent_id(Some(grandparent.id)),
            )
            .await?;
        storage.assign_tag_to_task(parent.id, &tag.name).await?;
        let child = storage
            .create_task(
                Task::create()
                    .title("Child")
                    .parent_id(Some(parent.id)),
            )
            .await?;

        let meta = storage.get_task_with_meta(child.id).await?;
        assert_eq!(meta.inherited_tags, vec![tag.label()]);
        Ok(())
    }

    #[tokio::test]
    async fn test_inherited_tags_feed_inference_and_leaf() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let cs = storage.create_tag("CS").await?;
        let programming = storage.create_tag("Programming").await?;
        let python = storage.create_tag("Python").await?;
        storage
            .add_tag_implication(python.id, programming.id)
            .await?;
        storage
            .add_tag_implication(programming.id, cs.id)
            .await?;

        let parent = storage.create_task(Task::create().title("Parent")).await?;
        storage.assign_tag_to_task(parent.id, &python.name).await?;
        let child = storage
            .create_task(
                Task::create()
                    .title("Child")
                    .parent_id(Some(parent.id)),
            )
            .await?;

        let meta = storage.get_task_with_meta(child.id).await?;
        assert_eq!(meta.inherited_tags, vec![python.label()]);
        // Tag-DAG inference applies over inherited tags too.
        for expected in [&python, &programming, &cs] {
            assert!(
                meta.inferred_tags.contains(&expected.label()),
                "inferred tags should contain {}",
                expected.name
            );
        }
        // Leaf drops implied tags: only the most specific remains.
        assert_eq!(meta.leaf_tags, vec![python.label()]);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks_by_tag_includes_subtasks() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag = storage.create_tag("Team").await?;

        let parent = storage.create_task(Task::create().title("Parent")).await?;
        storage.assign_tag_to_task(parent.id, &tag.name).await?;
        let child = storage
            .create_task(
                Task::create()
                    .title("Child")
                    .parent_id(Some(parent.id)),
            )
            .await?;
        let grandchild = storage
            .create_task(
                Task::create()
                    .title("Grandchild")
                    .parent_id(Some(child.id)),
            )
            .await?;

        // An unrelated task and its subtask stay out of the tag's list.
        let other = storage.create_task(Task::create().title("Other")).await?;
        storage
            .create_task(
                Task::create()
                    .title("Other child")
                    .parent_id(Some(other.id)),
            )
            .await?;

        let tasks = storage.list_tasks_by_tag(tag.id).await?;
        let ids: Vec<u64> = tasks.iter().map(|t| t.id).collect();
        assert!(ids.contains(&parent.id));
        assert!(ids.contains(&child.id));
        assert!(ids.contains(&grandchild.id));
        assert!(!ids.contains(&other.id));

        // Listed subtasks carry their inherited tags.
        let child_meta = tasks.iter().find(|t| t.id == child.id).unwrap();
        assert_eq!(child_meta.inherited_tags, vec![tag.label()]);
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

    /// Deleting a parent must take its edges with it in both directions.
    /// Otherwise the placed child is neither top-level (it still appears as an
    /// implier) nor rendered (its parent is gone), so it vanishes from the nav
    /// entirely.
    #[tokio::test]
    async fn test_delete_tag_removes_placement_edges_both_ways() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let area = storage.create_tag("Area").await?;
        let work = storage.create_tag("Work").await?;
        let project = storage
            .get_or_create_project_tag(std::path::Path::new("/tmp/nested/api"))
            .await?;
        // Work hangs under Area; the project's folder is placed under Work.
        storage.add_tag_implication(work.id, area.id).await?;
        storage.add_tag_implication(project.id, work.id).await?;

        storage.delete_tag(work.id).await?;

        assert!(storage.get_parents(project.id).await?.is_empty());
        assert!(storage.get_parents(area.id).await?.is_empty());
        let top = storage.get_top_level_tags().await?;
        assert!(top.iter().any(|tag| tag.id == project.id), "{top:?}");
        assert!(top.iter().any(|tag| tag.id == area.id));
        // …and the project is reachable in the rendered tree, not orphaned.
        let rows = storage.tag_tree_rows().await?;
        assert!(rows.iter().any(|row| row.tag.id == project.id));

        Ok(())
    }

    #[tokio::test]
    async fn test_tag_tree_nests_projects_under_a_tag_in_kind_order() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let work = storage.create_tag("Work").await?;
        let zebra = storage
            .get_or_create_project_tag(std::path::Path::new("/tmp/tree/zebra"))
            .await?;
        let alpha = storage
            .get_or_create_project_tag(std::path::Path::new("/tmp/tree/alpha"))
            .await?;
        let note = storage.create_tag("Backburner").await?;
        for child in [zebra.id, alpha.id, note.id] {
            storage.add_tag_implication(child, work.id).await?;
        }

        let rows = storage.tag_tree_rows().await?;
        let at = |id: u64| {
            rows.iter()
                .position(|row| row.tag.id == id)
                .expect("row for tag")
        };
        let work_at = at(work.id);
        assert_eq!(rows[work_at].depth, 0);
        assert!(!rows[work_at].is_directory_backed);
        // Projects first (alphabetically), then the plain child tag.
        assert_eq!(rows[work_at + 1].tag.id, alpha.id);
        assert_eq!(rows[work_at + 2].tag.id, zebra.id);
        assert_eq!(rows[work_at + 3].tag.id, note.id);
        assert_eq!(rows[work_at + 1].depth, 1);
        assert!(rows[work_at + 1].is_directory_backed);
        assert!(!rows[work_at + 3].is_directory_backed);
        // A nested row knows the path it was reached through.
        assert_eq!(
            rows[work_at + 1].path,
            vec!["Work".to_string(), alpha.name.clone()]
        );
        // The order is total, so repeated loads agree.
        let again = storage.tag_tree_rows().await?;
        let ids = |rows: &[TagTreeRow]| rows.iter().map(|row| row.tag.id).collect::<Vec<_>>();
        assert_eq!(ids(&rows), ids(&again));

        Ok(())
    }

    #[tokio::test]
    async fn test_tag_tree_orders_sections_last_and_marks_them() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let project = storage
            .get_or_create_project_tag(std::path::Path::new("/tmp/sectioned/api"))
            .await?;
        let plan = storage.create_tag("Plan").await?;
        let backlog = storage.create_tag("Backlog").await?;
        storage.add_tag_implication(plan.id, project.id).await?;
        storage.add_tag_implication(backlog.id, project.id).await?;
        // "Backlog" is a section of the project, so it sorts after plain tags.
        storage
            .add_tag_section(project.id, "Backlog".to_string())
            .await?;

        let rows = storage.tag_tree_rows().await?;
        let project_at = rows
            .iter()
            .position(|row| row.tag.id == project.id)
            .expect("project row");
        assert_eq!(rows[project_at + 1].tag.id, plan.id);
        assert!(!rows[project_at + 1].is_section);
        assert_eq!(rows[project_at + 2].tag.id, backlog.id);
        assert!(rows[project_at + 2].is_section);

        Ok(())
    }

    #[tokio::test]
    async fn test_tag_tree_repeats_a_multi_parent_project_per_parent() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let work = storage.create_tag("Work").await?;
        let home = storage.create_tag("Home").await?;
        let project = storage
            .get_or_create_project_tag(std::path::Path::new("/tmp/dup/api"))
            .await?;
        storage.add_tag_implication(project.id, work.id).await?;
        storage.add_tag_implication(project.id, home.id).await?;

        let rows = storage.tag_tree_rows().await?;
        let copies = rows
            .iter()
            .filter(|row| row.tag.id == project.id)
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|row| row.depth == 1));
        assert!(copies.iter().any(|row| row.path[0] == "Work"));
        assert!(copies.iter().any(|row| row.path[0] == "Home"));

        Ok(())
    }

    /// A cycle can only reach the table behind the API's back, but the render
    /// walk still has to terminate when it does.
    #[tokio::test]
    async fn test_tag_tree_terminates_on_a_cycle_written_behind_the_api() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let root = storage.create_tag("Root").await?;
        let a = storage.create_tag("A").await?;
        let b = storage.create_tag("B").await?;
        storage.add_tag_implication(a.id, root.id).await?;
        storage.add_tag_implication(b.id, a.id).await?;
        // The API refuses this direction, so write it directly.
        assert!(storage.add_tag_implication(a.id, b.id).await.is_err());
        toasty::sql::statement(
            r#"INSERT INTO tag_implications (implier_id, implied_id) VALUES (?1, ?2)"#,
        )
        .bind(a.id as i64)
        .bind(b.id as i64)
        .exec(&mut storage.db)
        .await?;

        let rows = storage.tag_tree_rows().await?;
        // Root -> A -> B, and the back edge to A is cut rather than followed.
        let ids = rows.iter().map(|row| row.tag.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![root.id, a.id, b.id]);

        Ok(())
    }

    #[tokio::test]
    async fn test_directory_backed_covers_dirs_alone() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let plain = storage.create_tag("Plain").await?;
        let with_dirs = storage.create_tag("WithDirs").await?;
        let project = storage
            .get_or_create_project_tag(std::path::Path::new("/tmp/dirs/api"))
            .await?;

        assert!(!storage.tag_is_directory_backed(plain.id).await?);
        assert!(!storage.tag_is_directory_backed(with_dirs.id).await?);
        assert!(storage.tag_is_directory_backed(project.id).await?);

        storage
            .add_tag_dir(with_dirs.id, "/tmp/dirs/extra".to_string())
            .await?;
        assert!(storage.tag_is_directory_backed(with_dirs.id).await?);

        let ids = storage.directory_backed_tag_ids().await?;
        assert!(ids.contains(&project.id));
        assert!(ids.contains(&with_dirs.id));
        assert!(!ids.contains(&plain.id));

        let rows = storage.tag_tree_rows().await?;
        let row = rows
            .iter()
            .find(|row| row.tag.id == with_dirs.id)
            .expect("row for the dirs-backed tag");
        assert!(row.is_directory_backed);

        Ok(())
    }
}

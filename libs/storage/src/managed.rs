//! Managed content: apps, their bindings to tags, and per-row ownership.
//!
//! An *app* is anything that owns content on the user's behalf: a workflow
//! recipe (`kind='recipe'`), an integration such as Todoist
//! (`kind='integration'`), or shipped content (`kind='builtin'`).
//!
//! Ownership is a nullable `managed_by` column on tags, sections and tasks.
//! For tasks the column carries a *mode*:
//! - `managed` — the app owns the task: content edits are blocked (completion
//!   is always allowed) and regeneration may replace it.
//! - `captured` — the app only propagates the task (e.g. pushes it to
//!   Todoist). The task stays fully editable and the app never regenerates or
//!   removes it.
//!
//! Two further flags refine `managed` tasks:
//! - `managed_editable` — set by the app (dynamic) or by a remote edit, and
//!   the only way a managed task becomes locally editable.
//! - `user_modified` — set by any local content edit; regeneration and
//!   removal skip the task so a deliberate edit is never discarded.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;

/// Slug of the builtin app that owns shipped/demo content.
pub const DEMO_APP_SLUG: &str = "demo";
/// Slug of the travel automation's app.
pub const TRAVEL_APP_SLUG: &str = "travel";

/// Boxed future used by [`TodoStore::with_transaction`] so the closure can
/// borrow the store for the duration of the transaction.
pub type BoxQueryFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = QueryResult<T>> + Send + 'a>>;

/// How an app owns a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedMode {
    /// Owned: content edits blocked, regeneration may replace.
    Managed,
    /// Propagated only: fully editable, never regenerated or removed.
    Captured,
}

impl ManagedMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ManagedMode::Managed => "managed",
            ManagedMode::Captured => "captured",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "managed" => Some(ManagedMode::Managed),
            "captured" => Some(ManagedMode::Captured),
            _ => None,
        }
    }
}

/// One registered owner of content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub id: u64,
    /// `'recipe' | 'integration' | 'builtin'`.
    pub kind: String,
    pub slug: String,
    pub label: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub created_at: jiff::Timestamp,
}

/// App role on a tag: whole-tag ownership or partial management.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRole {
    FullTag,
    Partial,
}

impl BindingRole {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingRole::FullTag => "full_tag",
            BindingRole::Partial => "partial",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full_tag" => Some(BindingRole::FullTag),
            "partial" => Some(BindingRole::Partial),
            _ => None,
        }
    }
}

/// One app attached to one tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTagBinding {
    pub id: u64,
    pub app_id: u64,
    pub tag_id: u64,
    pub role: BindingRole,
    /// New tasks landing in this tag are captured (propagated) by the app.
    pub capture_new_tasks: bool,
    pub created_at: jiff::Timestamp,
}

/// Ownership state of one task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskOwnership {
    pub managed_by: Option<u64>,
    pub managed_mode: Option<ManagedMode>,
    pub managed_editable: bool,
    pub user_modified: bool,
}

fn parse_app(row: &toasty::stmt::Value) -> Option<App> {
    let toasty::stmt::Value::Record(record) = row else {
        return None;
    };
    Some(App {
        id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
        kind: record.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        slug: record.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        label: record.get(3).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        description: record
            .get(4)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        enabled: record.get(5).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
        created_at: record
            .get(6)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(jiff::Timestamp::now),
    })
}

fn parse_binding(row: &toasty::stmt::Value) -> Option<AppTagBinding> {
    let toasty::stmt::Value::Record(record) = row else {
        return None;
    };
    Some(AppTagBinding {
        id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
        app_id: record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
        tag_id: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
        role: record
            .get(3)
            .and_then(|v| v.as_str())
            .and_then(BindingRole::parse)
            .unwrap_or(BindingRole::Partial),
        capture_new_tasks: record.get(4).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
        created_at: record
            .get(5)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(jiff::Timestamp::now),
    })
}

const APP_COLUMNS: &str = "id, kind, slug, label, description, enabled, created_at";
const BINDING_COLUMNS: &str = "id, app_id, tag_id, role, capture_new_tasks, created_at";

impl TodoStore {
    // -- raw helpers ------------------------------------------------------

    async fn run_sql(&mut self, sql: &str) -> QueryResult<()> {
        toasty::sql::statement(sql)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: format!("transaction statement: {sql}"),
            })?;
        Ok(())
    }

    async fn app_last_insert_id(&mut self) -> QueryResult<u64> {
        let rows = toasty::sql::query("SELECT last_insert_rowid()")
            .column_types([toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "last app insert id",
            })?;
        match rows.first() {
            Some(toasty::stmt::Value::Record(record)) => Ok(record
                .first()
                .and_then(|v| v.to_i64())
                .unwrap_or(0) as u64),
            _ => Ok(0),
        }
    }

    /// Run `f` inside a `BEGIN`/`COMMIT` transaction, rolling back on error.
    ///
    /// Not re-entrant: nested calls would issue a second `BEGIN`, so callers
    /// must keep transactions flat.
    pub async fn with_transaction<T, F>(&mut self, f: F) -> QueryResult<T>
    where
        F: for<'a> FnOnce(&'a mut TodoStore) -> BoxQueryFuture<'a, T>,
    {
        self.run_sql("BEGIN").await?;
        match f(self).await {
            Ok(value) => {
                self.run_sql("COMMIT").await?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback) = self.run_sql("ROLLBACK").await {
                    tracing::error!(%rollback, "failed to roll back transaction");
                }
                Err(error)
            }
        }
    }

    // -- apps -------------------------------------------------------------

    /// Register an app, or refresh the label/kind of an existing slug.
    pub async fn upsert_app(
        &mut self,
        kind: &str,
        slug: &str,
        label: &str,
        description: Option<String>,
    ) -> QueryResult<App> {
        let created_at = jiff::Timestamp::now().to_string();
        toasty::sql::statement(
            r#"INSERT INTO apps (kind, slug, label, description, enabled, created_at)
               VALUES (?1, ?2, ?3, ?4, 0, ?5)
               ON CONFLICT (slug) DO UPDATE
               SET kind = excluded.kind,
                   label = excluded.label,
                   description = COALESCE(excluded.description, apps.description)"#,
        )
        .bind(kind)
        .bind(slug)
        .bind(label)
        .bind(description.clone().unwrap_or_default())
        .bind(created_at)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "upsert app",
        })?;
        self.app_by_slug(slug)
            .await?
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: format!("app '{slug}' missing after upsert"),
            })
    }

    pub async fn app_by_id(&mut self, id: u64) -> QueryResult<Option<App>> {
        let rows = toasty::sql::query(format!("SELECT {APP_COLUMNS} FROM apps WHERE id = ?1"))
            .column_types([
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
            ])
            .bind(id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "get app by id",
            })?;
        Ok(rows.first().and_then(parse_app))
    }

    pub async fn app_by_slug(&mut self, slug: &str) -> QueryResult<Option<App>> {
        let rows = toasty::sql::query(format!("SELECT {APP_COLUMNS} FROM apps WHERE slug = ?1"))
            .column_types([
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
            ])
            .bind(slug)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "get app by slug",
            })?;
        Ok(rows.first().and_then(parse_app))
    }

    pub async fn list_apps(&mut self) -> QueryResult<Vec<App>> {
        let rows = toasty::sql::query(format!("SELECT {APP_COLUMNS} FROM apps ORDER BY id"))
            .column_types([
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
            ])
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list apps",
            })?;
        Ok(rows.iter().filter_map(parse_app).collect())
    }

    pub async fn set_app_enabled(&mut self, app_id: u64, enabled: bool) -> QueryResult<()> {
        toasty::sql::statement(r#"UPDATE apps SET enabled = ?1 WHERE id = ?2"#)
            .bind(i64::from(enabled))
            .bind(app_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "set app enabled",
            })?;
        Ok(())
    }

    /// The builtin app that owns shipped/demo content, created on demand.
    pub async fn demo_app(&mut self) -> QueryResult<App> {
        let app = self
            .upsert_app(
                "builtin",
                DEMO_APP_SLUG,
                "Demo data",
                Some("Shipped example content; never syncs to integrations.".to_string()),
            )
            .await?;
        self.set_app_enabled(app.id, true).await?;
        self.app_by_id(app.id)
            .await?
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: "demo app missing after creation".to_string(),
            })
    }

    /// The app backing a workflow recipe, if one is registered.
    pub async fn app_for_recipe(&mut self, recipe_id: u64) -> QueryResult<Option<App>> {
        let rows = toasty::sql::query(r#"SELECT app_id FROM workflow_recipes WHERE id = ?1"#)
            .column_types([toasty::stmt::Type::I64])
            .bind(recipe_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "recipe app id",
            })?;
        let app_id = rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        });
        match app_id {
            Some(id) => self.app_by_id(id).await,
            None => Ok(None),
        }
    }

    /// Point a recipe at its app.
    pub async fn set_recipe_app(&mut self, recipe_id: u64, app_id: u64) -> QueryResult<()> {
        toasty::sql::statement(r#"UPDATE workflow_recipes SET app_id = ?1 WHERE id = ?2"#)
            .bind(app_id as i64)
            .bind(recipe_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "set recipe app",
            })?;
        Ok(())
    }

    /// The app backing an integration, if one is registered.
    pub async fn app_for_integration(&mut self, integration_id: u64) -> QueryResult<Option<App>> {
        let rows = toasty::sql::query(r#"SELECT app_id FROM integrations WHERE id = ?1"#)
            .column_types([toasty::stmt::Type::I64])
            .bind(integration_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "integration app id",
            })?;
        let app_id = rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        });
        match app_id {
            Some(id) => self.app_by_id(id).await,
            None => Ok(None),
        }
    }

    /// The recipe backed by an app, if the app is a recipe app.
    pub async fn recipe_for_app(&mut self, app_id: u64) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(r#"SELECT id FROM workflow_recipes WHERE app_id = ?1"#)
            .column_types([toasty::stmt::Type::I64])
            .bind(app_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "recipe for app",
            })?;
        Ok(rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        }))
    }

    pub async fn set_integration_app(
        &mut self,
        integration_id: u64,
        app_id: u64,
    ) -> QueryResult<()> {
        toasty::sql::statement(r#"UPDATE integrations SET app_id = ?1 WHERE id = ?2"#)
            .bind(app_id as i64)
            .bind(integration_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "set integration app",
            })?;
        Ok(())
    }

    // -- bindings ---------------------------------------------------------

    /// Attach an app to a tag, enforcing the ownership rules:
    /// directory-backed project tags are ineligible, a tag held with a
    /// `full_tag` binding is closed to other apps, and only one app may hold
    /// a `full_tag` binding.
    pub async fn attach_app_to_tag(
        &mut self,
        app_id: u64,
        tag_id: u64,
        role: BindingRole,
        capture_new_tasks: bool,
    ) -> QueryResult<AppTagBinding> {
        let tag = self.get_tag(tag_id).await?;
        if tag.is_project() {
            return Err(crate::QueryErr::UnexpectedValue {
                message: format!(
                    "tag '{}' backs a local directory and cannot be managed by an app",
                    tag.label()
                ),
            });
        }
        for existing in self.bindings_for_tag(tag_id).await? {
            if existing.app_id == app_id {
                continue;
            }
            let conflict = existing.role == BindingRole::FullTag || role == BindingRole::FullTag;
            if conflict {
                return Err(crate::QueryErr::UnexpectedValue {
                    message: format!(
                        "tag '{}' is already owned by app {}; release it first",
                        tag.label(),
                        existing.app_id
                    ),
                });
            }
        }
        let created_at = jiff::Timestamp::now().to_string();
        toasty::sql::statement(
            r#"INSERT INTO app_tag_bindings
               (app_id, tag_id, role, capture_new_tasks, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT (app_id, tag_id) DO UPDATE
               SET role = excluded.role,
                   capture_new_tasks = excluded.capture_new_tasks"#,
        )
        .bind(app_id as i64)
        .bind(tag_id as i64)
        .bind(role.as_str())
        .bind(i64::from(capture_new_tasks))
        .bind(created_at)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "attach app to tag",
        })?;
        if role == BindingRole::FullTag {
            self.set_tag_managed(tag_id, Some(app_id)).await?;
        }
        self.bindings_for_tag(tag_id)
            .await?
            .into_iter()
            .find(|binding| binding.app_id == app_id)
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: "binding missing after attach".to_string(),
            })
    }

    pub async fn detach_app_from_tag(&mut self, app_id: u64, tag_id: u64) -> QueryResult<()> {
        toasty::sql::statement(
            r#"DELETE FROM app_tag_bindings WHERE app_id = ?1 AND tag_id = ?2"#,
        )
        .bind(app_id as i64)
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "detach app from tag",
        })?;
        Ok(())
    }

    async fn delete_bindings_for_app(&mut self, app_id: u64) -> QueryResult<()> {
        toasty::sql::statement(r#"DELETE FROM app_tag_bindings WHERE app_id = ?1"#)
            .bind(app_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "delete app bindings",
            })?;
        Ok(())
    }

    pub async fn bindings_for_tag(&mut self, tag_id: u64) -> QueryResult<Vec<AppTagBinding>> {
        let rows = toasty::sql::query(format!(
            "SELECT {BINDING_COLUMNS} FROM app_tag_bindings WHERE tag_id = ?1 ORDER BY id"
        ))
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .bind(tag_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "bindings for tag",
        })?;
        Ok(rows.iter().filter_map(parse_binding).collect())
    }

    pub async fn bindings_for_app(&mut self, app_id: u64) -> QueryResult<Vec<AppTagBinding>> {
        let rows = toasty::sql::query(format!(
            "SELECT {BINDING_COLUMNS} FROM app_tag_bindings WHERE app_id = ?1 ORDER BY id"
        ))
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .bind(app_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "bindings for app",
        })?;
        Ok(rows.iter().filter_map(parse_binding).collect())
    }

    // -- ownership --------------------------------------------------------

    pub async fn tag_owner(&mut self, tag_id: u64) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(r#"SELECT managed_by FROM tags WHERE id = ?1"#)
            .column_types([toasty::stmt::Type::I64])
            .bind(tag_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "tag owner",
            })?;
        Ok(rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        }))
    }

    pub async fn set_tag_managed(&mut self, tag_id: u64, app_id: Option<u64>) -> QueryResult<()> {
        // `NULLIF(..., -1)` keeps "no owner" a real NULL rather than a
        // sentinel id.
        toasty::sql::statement(
            r#"UPDATE tags SET managed_by = NULLIF(?1, -1) WHERE id = ?2"#,
        )
        .bind(app_id.map(|id| id as i64).unwrap_or(-1))
            .bind(tag_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "set tag owner",
            })?;
        Ok(())
    }

    /// Ownership state of one task. Absent rows read as unowned.
    pub async fn task_ownership(&mut self, task_id: u64) -> QueryResult<TaskOwnership> {
        let rows = toasty::sql::query(
            r#"SELECT managed_by, managed_mode, managed_editable, user_modified
               FROM tasks WHERE id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
        ])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "task ownership",
        })?;
        let Some(toasty::stmt::Value::Record(record)) = rows.first() else {
            return Ok(TaskOwnership::default());
        };
        Ok(TaskOwnership {
            managed_by: record.first().and_then(|v| v.to_i64()).map(|id| id as u64),
            managed_mode: record
                .get(1)
                .and_then(|v| v.as_str())
                .and_then(ManagedMode::parse),
            managed_editable: record.get(2).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
            user_modified: record.get(3).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
        })
    }

    /// Record that an app owns a task. `editable` is the app's own setting,
    /// not a user preference.
    pub async fn set_task_managed(
        &mut self,
        task_id: u64,
        app_id: u64,
        mode: ManagedMode,
        editable: bool,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"UPDATE tasks
               SET managed_by = ?1, managed_mode = ?2, managed_editable = ?3
               WHERE id = ?4"#,
        )
        .bind(app_id as i64)
        .bind(mode.as_str())
        .bind(i64::from(editable))
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "set task owner",
        })?;
        Ok(())
    }

    /// Drop an app's ownership of a task, leaving the row untouched.
    pub async fn clear_task_managed(&mut self, task_id: u64) -> QueryResult<()> {
        toasty::sql::statement(
            r#"UPDATE tasks
               SET managed_by = NULL, managed_mode = NULL, managed_editable = NULL
               WHERE id = ?1"#,
        )
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "clear task owner",
        })?;
        Ok(())
    }

    /// Flip the app-controlled editability flag.
    pub async fn set_task_editable(&mut self, task_id: u64, editable: bool) -> QueryResult<()> {
        toasty::sql::statement(r#"UPDATE tasks SET managed_editable = ?1 WHERE id = ?2"#)
            .bind(i64::from(editable))
            .bind(task_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "set task editable",
            })?;
        Ok(())
    }

    /// Mark a managed task as user-modified so regeneration spares it.
    pub async fn mark_task_user_modified(&mut self, task_id: u64) -> QueryResult<()> {
        toasty::sql::statement(r#"UPDATE tasks SET user_modified = 1 WHERE id = ?1"#)
            .bind(task_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "mark task user modified",
            })?;
        Ok(())
    }

    /// A remote (integration-side) edit wins: unlock the whole task and spare
    /// it from regeneration.
    pub async fn unlock_task_from_remote(&mut self, task_id: u64) -> QueryResult<()> {
        toasty::sql::statement(
            r#"UPDATE tasks
               SET managed_editable = 1, user_modified = 1
               WHERE id = ?1 AND managed_by IS NOT NULL"#,
        )
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "unlock task from remote edit",
        })?;
        Ok(())
    }

    /// Hard-block guard for local content edits: only `managed` tasks that
    /// are not editable are locked. Captured tasks and completion are exempt.
    pub async fn assert_task_editable(&mut self, task_id: u64) -> QueryResult<()> {
        let ownership = self.task_ownership(task_id).await?;
        if let Some(app_id) = ownership.managed_by
            && ownership.managed_mode == Some(ManagedMode::Managed)
            && !ownership.managed_editable
        {
            return Err(crate::QueryErr::TaskLocked { task_id, app_id });
        }
        Ok(())
    }

    /// Content edits through the guard also mark the task user-modified, so
    /// regeneration will spare it.
    pub(crate) async fn guard_and_mark_modified(&mut self, task_id: u64) -> QueryResult<()> {
        self.assert_task_editable(task_id).await?;
        if self.task_ownership(task_id).await?.managed_by.is_some() {
            self.mark_task_user_modified(task_id).await?;
        }
        Ok(())
    }

    /// Task ids in `tag_id`'s subtree owned by `app_id` in `managed` mode,
    /// still open and not user-modified: the regeneration scope.
    pub async fn managed_tasks_in_tag(
        &mut self,
        tag_id: u64,
        app_id: u64,
    ) -> QueryResult<Vec<u64>> {
        let mut tag_ids = vec![tag_id];
        tag_ids.extend(self.get_all_descendants(tag_id).await?.into_iter().map(|t| t.id));
        let id_list: Vec<String> = tag_ids.iter().map(|id| id.to_string()).collect();
        let query = format!(
            r#"SELECT DISTINCT t.id FROM tasks t
               JOIN direct_task_tags dtt ON dtt.task_id = t.id
               WHERE dtt.tag_id IN ({})
                 AND t.managed_by = ?1
                 AND COALESCE(t.managed_mode, 'managed') = 'managed'
                 AND COALESCE(t.user_modified, 0) = 0
                 AND t.done = 0
                 AND t.deleted_at IS NULL"#,
            id_list.join(",")
        );
        let rows = toasty::sql::query(&query)
            .column_types([toasty::stmt::Type::I64])
            .bind(app_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "managed tasks in tag",
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

    /// Sections of `tag_id` owned by `app_id`.
    pub async fn managed_sections_in_tag(
        &mut self,
        tag_id: u64,
        app_id: u64,
    ) -> QueryResult<Vec<crate::tag_settings::TagSection>> {
        Ok(self
            .tag_sections(tag_id)
            .await?
            .into_iter()
            .filter(|section| section.managed_by == Some(app_id))
            .collect())
    }

    /// The owning app of a section, if any.
    pub async fn section_owner(&mut self, section_id: u64) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(r#"SELECT managed_by FROM tag_sections WHERE id = ?1"#)
            .column_types([toasty::stmt::Type::I64])
            .bind(section_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "section owner",
            })?;
        Ok(rows.first().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => {
                record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
            }
            _ => None,
        }))
    }

    /// Take ownership of a section: the `tag_sections` row and its child tag
    /// are marked together.
    pub async fn set_section_managed(
        &mut self,
        section_id: u64,
        app_id: Option<u64>,
        capture: bool,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"UPDATE tag_sections
               SET managed_by = NULLIF(?1, -1), managed_capture = ?2
               WHERE id = ?3"#,
        )
        .bind(app_id.map(|id| id as i64).unwrap_or(-1))
        .bind(i64::from(capture))
        .bind(section_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "set section owner",
        })?;
        if let Some(section) = self.section_row(section_id).await?
            && let Some(child) = self.section_child_tag(section.tag_id, &section.name).await?
        {
            self.set_tag_managed(child.id, app_id).await?;
        }
        Ok(())
    }

    /// Clear ownership on a section, keeping it as an ordinary user section.
    pub async fn downgrade_section(&mut self, section_id: u64) -> QueryResult<()> {
        let Some(section) = self.section_row(section_id).await? else {
            return Ok(());
        };
        toasty::sql::statement(
            r#"UPDATE tag_sections SET managed_by = NULL, managed_capture = NULL WHERE id = ?1"#,
        )
        .bind(section_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "downgrade section",
        })?;
        if let Some(child) = self.section_child_tag(section.tag_id, &section.name).await? {
            self.set_tag_managed(child.id, None).await?;
        }
        Ok(())
    }

    async fn section_row(
        &mut self,
        section_id: u64,
    ) -> QueryResult<Option<crate::tag_settings::TagSection>> {
        let rows = toasty::sql::query(
            r#"SELECT id, tag_id, name, position FROM tag_sections WHERE id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
        ])
        .bind(section_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load section",
        })?;
        Ok(rows.first().and_then(|row| {
            let toasty::stmt::Value::Record(record) = row else {
                return None;
            };
            Some(crate::tag_settings::TagSection {
                id: record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                tag_id: record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64,
                name: record.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                position: record.get(3).and_then(|v| v.to_i64()).unwrap_or(0),
                managed_by: record.get(4).and_then(|v| v.to_i64()).map(|id| id as u64),
                managed_capture: record.get(5).and_then(|v| v.to_i64()).unwrap_or(0) != 0,
            })
        }))
    }

    /// The child tag backing a named section of `tag_id` (sections are stored
    /// as an implication child of their tag).
    pub async fn section_child_tag(
        &mut self,
        tag_id: u64,
        name: &str,
    ) -> QueryResult<Option<crate::Tag>> {
        Ok(self
            .get_children(tag_id)
            .await?
            .into_iter()
            .find(|child| child.label() == name))
    }

    /// Whether a section still holds any tasks (user's, completed, or the
    /// app's own): if so it survives app removal as a plain section.
    pub async fn section_has_tasks(&mut self, tag_id: u64, name: &str) -> QueryResult<bool> {
        let Some(child) = self.section_child_tag(tag_id, name).await? else {
            return Ok(false);
        };
        let rows = toasty::sql::query(
            r#"SELECT 1 FROM direct_task_tags dtt
               JOIN tasks t ON t.id = dtt.task_id
               WHERE dtt.tag_id = ?1 AND t.deleted_at IS NULL LIMIT 1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(child.id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "section has tasks",
        })?;
        Ok(!rows.is_empty())
    }

    // -- capture ----------------------------------------------------------

    /// Mark a task as captured by every app whose binding/section covers it.
    /// Returns the app ids that captured it (the caller runs provider hooks).
    pub async fn capture_task(&mut self, task_id: u64) -> QueryResult<Vec<u64>> {
        let direct = self.get_direct_task_tags(task_id).await?;
        let mut capturing: Vec<u64> = Vec::new();
        for tag in direct {
            for binding in self.bindings_for_tag(tag.id).await? {
                if binding.capture_new_tasks && !capturing.contains(&binding.app_id) {
                    capturing.push(binding.app_id);
                }
            }
            for parent in self.get_parents(tag.id).await? {
                let section_capture = toasty::sql::query(
                    r#"SELECT managed_capture FROM tag_sections
                       WHERE tag_id = ?1 AND name = ?2"#,
                )
                .column_types([toasty::stmt::Type::I64])
                .bind(parent.id as i64)
                .bind(tag.label())
                .exec(&mut self.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "section capture policy",
                })?;
                let section_capture = section_capture
                    .first()
                    .and_then(|row| match row {
                        toasty::stmt::Value::Record(record) => {
                            record.first().and_then(|v| v.to_i64())
                        }
                        _ => None,
                    })
                    .unwrap_or(0)
                    != 0;
                for binding in self.bindings_for_tag(parent.id).await? {
                    if (binding.capture_new_tasks || section_capture)
                        && !capturing.contains(&binding.app_id)
                    {
                        capturing.push(binding.app_id);
                    }
                }
            }
        }
        for app_id in &capturing {
            self.set_task_managed(task_id, *app_id, ManagedMode::Captured, true)
                .await?;
        }
        Ok(capturing)
    }

    // -- lifecycle --------------------------------------------------------

    /// Detach one app from one tag: downgrade or remove its sections, and
    /// remove the binding. Tasks keep their ownership (a full app removal
    /// clears them).
    pub async fn release_app_from_tag(&mut self, app_id: u64, tag_id: u64) -> QueryResult<()> {
        let sections = self
            .tag_sections(tag_id)
            .await?
            .into_iter()
            .filter(|section| section.managed_by == Some(app_id))
            .collect::<Vec<_>>();
        for section in sections {
            if self.section_has_tasks(tag_id, &section.name).await? {
                self.downgrade_section(section.id).await?;
            } else {
                if let Some(child) = self.section_child_tag(tag_id, &section.name).await? {
                    self.remove_tag_implication(child.id, tag_id).await?;
                    self.delete_tag(child.id).await?;
                }
                self.remove_tag_section(section.id).await?;
            }
        }
        self.detach_app_from_tag(app_id, tag_id).await?;
        Ok(())
    }

    /// Remove an app. When `remove_owned_items`, its open, unmodified managed
    /// tasks are tombstoned; completed and user-modified tasks survive as
    /// ordinary tasks. Runs in a single transaction.
    pub async fn disable_app(&mut self, app_id: u64, remove_owned_items: bool) -> QueryResult<()> {
        let bindings = self.bindings_for_app(app_id).await?;
        self.with_transaction(|store| {
            Box::pin(async move {
                for binding in &bindings {
                    store.release_app_from_tag(app_id, binding.tag_id).await?;
                }
                if remove_owned_items {
                    let rows = toasty::sql::query(
                        r#"SELECT id FROM tasks
                           WHERE managed_by = ?1
                             AND COALESCE(managed_mode, 'managed') = 'managed'
                             AND COALESCE(user_modified, 0) = 0
                             AND done = 0
                             AND deleted_at IS NULL"#,
                    )
                    .column_types([toasty::stmt::Type::I64])
                    .bind(app_id as i64)
                    .exec(&mut store.db)
                    .await
                    .context(crate::error::QueryTagsSnafu {
                        context: "list removable app tasks",
                    })?;
                    for row in rows {
                        if let toasty::stmt::Value::Record(record) = row
                            && let Some(id) = record.first().and_then(|v| v.to_i64())
                        {
                            store.tombstone_task(id as u64).await?;
                        }
                    }
                }
                // Surviving rows (completed, user-modified, captured) lose
                // their ownership but stay in the database.
                toasty::sql::statement(
                    r#"UPDATE tasks
                       SET managed_by = NULL, managed_mode = NULL, managed_editable = NULL
                       WHERE managed_by = ?1"#,
                )
                .bind(app_id as i64)
                .exec(&mut store.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "clear app task ownership",
                })?;
                for binding in &bindings {
                    if binding.role == BindingRole::FullTag {
                        store.delete_tag(binding.tag_id).await?;
                    }
                }
                store.delete_bindings_for_app(app_id).await?;
                store.set_app_enabled(app_id, false).await?;
                Ok(())
            })
        })
        .await
    }

    /// Idempotent startup backfill: register the builtin demo app, give every
    /// integration and managed-tag recipe an app row, and convert legacy
    /// `tags.managed_by_recipe_id` ownership into partial bindings.
    pub async fn ensure_builtin_apps(&mut self) -> QueryResult<()> {
        self.demo_app().await?;

        let integrations = toasty::sql::query(
            r#"SELECT id, provider, COALESCE(account_label, '') FROM integrations
               WHERE app_id IS NULL"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "integrations missing apps",
        })?;
        for row in integrations {
            let toasty::stmt::Value::Record(record) = row else {
                continue;
            };
            let id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
            let provider = record.get(1).and_then(|v| v.as_str()).unwrap_or("integration");
            let account = record.get(2).and_then(|v| v.as_str()).unwrap_or("");
            let label = if account.is_empty() { provider } else { account };
            let slug = format!("{provider}-{id}");
            let app = self
                .upsert_app("integration", &slug, label, None)
                .await?;
            self.set_app_enabled(app.id, true).await?;
            self.set_integration_app(id, app.id).await?;
        }

        let recipes = toasty::sql::query(
            r#"SELECT id, slug, recipe_json FROM workflow_recipes WHERE app_id IS NULL"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "recipes missing apps",
        })?;
        for row in recipes {
            let toasty::stmt::Value::Record(record) = row else {
                continue;
            };
            let recipe_id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
            let slug = record.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let raw = record.get(2).and_then(|v| v.as_str()).unwrap_or("");
            let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
                continue;
            };
            let Ok(recipe) = crate::workflow::parse_recipe(&value) else {
                continue;
            };
            let app = self
                .upsert_app("recipe", &slug, &recipe.name, recipe.description.clone())
                .await?;
            self.set_recipe_app(recipe_id, app.id).await?;
            if let Some(legacy_tag) = recipe.managed_tag {
                self.migrate_legacy_managed_tag(app.id, recipe_id, &legacy_tag)
                    .await?;
            }
        }
        Ok(())
    }

    /// Convert a `managed_by_recipe_id` tag into a `partial` binding: the tag
    /// stays an ordinary user tag, its sections and their tasks become the
    /// app's, and the legacy marker is cleared.
    async fn migrate_legacy_managed_tag(
        &mut self,
        app_id: u64,
        recipe_id: u64,
        tag_name: &str,
    ) -> QueryResult<()> {
        if self.get_tag_by_name(tag_name).await?.is_none() {
            return Ok(());
        }
        let owned = self.managed_recipe_for_tag_by_name(tag_name, recipe_id).await?;
        if !owned {
            return Ok(());
        }
        let tag = self
            .get_tag_by_name(tag_name)
            .await?
            .ok_or_else(|| crate::QueryErr::UnexpectedValue {
                message: format!("legacy managed tag '{tag_name}' vanished during migration"),
            })?;
        self.attach_app_to_tag(app_id, tag.id, BindingRole::Partial, false)
            .await?;
        for section in self.tag_sections(tag.id).await? {
            self.set_section_managed(section.id, Some(app_id), false)
                .await?;
            if let Some(child) = self.section_child_tag(tag.id, &section.name).await? {
                toasty::sql::statement(
                    r#"UPDATE tasks
                       SET managed_by = ?1, managed_mode = 'managed', managed_editable = 0
                       WHERE id IN (SELECT task_id FROM direct_task_tags WHERE tag_id = ?2)
                         AND deleted_at IS NULL"#,
                )
                .bind(app_id as i64)
                .bind(child.id as i64)
                .exec(&mut self.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "own legacy managed section tasks",
                })?;
            }
        }
        toasty::sql::statement(
            r#"UPDATE tags SET managed_by_recipe_id = NULL, managed_by = NULL WHERE id = ?1"#,
        )
        .bind(tag.id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "clear legacy managed tag",
        })?;
        Ok(())
    }

    /// Whether `tag_name` is still marked as owned by `recipe_id`.
    async fn managed_recipe_for_tag_by_name(
        &mut self,
        tag_name: &str,
        recipe_id: u64,
    ) -> QueryResult<bool> {
        let rows = toasty::sql::query(
            r#"SELECT 1 FROM tags
               WHERE LOWER(name) = LOWER(?1) AND managed_by_recipe_id = ?2"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(tag_name)
        .bind(recipe_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "check legacy managed tag",
        })?;
        Ok(!rows.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    #[tokio::test]
    async fn test_app_registry_and_bindings() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let travel = store
            .upsert_app("recipe", "travel", "Travel checklists", None)
            .await?;
        assert_eq!(travel.kind, "recipe");
        assert!(!travel.enabled);
        // Upsert refreshes the label without duplicating.
        let again = store
            .upsert_app("recipe", "travel", "Travel", None)
            .await?;
        assert_eq!(again.id, travel.id);
        // The builtin demo app also exists; the travel upsert did not
        // duplicate.
        let apps = store.list_apps().await?;
        assert_eq!(apps.iter().filter(|app| app.kind != "builtin").count(), 1);

        let tag = store.create_tag("Travel").await?;
        let binding = store
            .attach_app_to_tag(travel.id, tag.id, BindingRole::Partial, true)
            .await?;
        assert_eq!(binding.role, BindingRole::Partial);
        assert!(binding.capture_new_tasks);
        assert_eq!(store.bindings_for_tag(tag.id).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_project_tags_and_full_tag_conflicts_rejected() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let first = store.upsert_app("recipe", "a", "A", None).await?;
        let second = store.upsert_app("recipe", "b", "B", None).await?;

        let project = store
            .get_or_create_project_tag(std::path::Path::new("/tmp/somewhere/api"))
            .await?;
        assert!(
            store
                .attach_app_to_tag(first.id, project.id, BindingRole::Partial, false)
                .await
                .is_err()
        );

        let tag = store.create_tag("Shared").await?;
        store
            .attach_app_to_tag(first.id, tag.id, BindingRole::FullTag, false)
            .await?;
        assert_eq!(store.tag_owner(tag.id).await?, Some(first.id));
        // A second app cannot take partial ownership of a fully owned tag.
        assert!(
            store
                .attach_app_to_tag(second.id, tag.id, BindingRole::Partial, false)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_managed_guard_and_captured_exemption() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let app = store.upsert_app("recipe", "travel", "Travel", None).await?;

        let managed = store.create_task(Task::create().title("Managed")).await?;
        store
            .set_task_managed(managed.id, app.id, ManagedMode::Managed, false)
            .await?;
        assert!(store.update_task_title(managed.id, "Nope").await.is_err());
        // Completion is always allowed.
        store.update_task_done(managed.id, true).await?;

        // The app can make its own item editable, then the user may edit it
        // (which marks it user-modified).
        store.set_task_editable(managed.id, true).await?;
        store.update_task_title(managed.id, "Renamed").await?;
        let ownership = store.task_ownership(managed.id).await?;
        assert!(ownership.user_modified);

        // Captured tasks stay fully editable.
        let captured = store.create_task(Task::create().title("Captured")).await?;
        store
            .set_task_managed(captured.id, app.id, ManagedMode::Captured, true)
            .await?;
        store.update_task_title(captured.id, "Still mine").await?;
        assert_eq!(store.get_task(captured.id).await?.title, "Still mine");
        Ok(())
    }

    #[tokio::test]
    async fn test_remote_edit_unlocks_and_spares() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let app = store.upsert_app("integration", "todoist", "Todoist", None).await?;
        let task = store.create_task(Task::create().title("Synced")).await?;
        store
            .set_task_managed(task.id, app.id, ManagedMode::Managed, false)
            .await?;
        assert!(store.update_task_title(task.id, "blocked").await.is_err());

        store.unlock_task_from_remote(task.id).await?;
        let ownership = store.task_ownership(task.id).await?;
        assert!(ownership.managed_editable);
        assert!(ownership.user_modified);
        store.update_task_title(task.id, "remote rename").await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_disable_app_spares_completed_and_sections() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let app = store.upsert_app("recipe", "travel", "Travel", None).await?;
        let tag = store.create_tag("Travel").await?;
        store
            .attach_app_to_tag(app.id, tag.id, BindingRole::Partial, false)
            .await?;

        let section = store.add_tag_section(tag.id, "Pack".to_string()).await?;
        let section_tag = store
            .create_tag_with_display_name(
                format!("{}:pack", tag.name),
                Some("Pack".to_string()),
            )
            .await?;
        store.add_tag_implication(section_tag.id, tag.id).await?;
        store
            .set_section_managed(section.id, Some(app.id), false)
            .await?;

        let open = store.create_task(Task::create().title("Open item")).await?;
        store.assign_tag_to_task(open.id, &section_tag.name).await?;
        store
            .set_task_managed(open.id, app.id, ManagedMode::Managed, false)
            .await?;
        let done = store.create_task(Task::create().title("Done item")).await?;
        store.assign_tag_to_task(done.id, &section_tag.name).await?;
        store
            .set_task_managed(done.id, app.id, ManagedMode::Managed, false)
            .await?;
        store.update_task_done(done.id, true).await?;

        store.disable_app(app.id, true).await?;

        // Open item tombstoned; completed item survives and is unowned.
        assert!(store.get_task(open.id).await?.deleted_at.is_some());
        let done_task = store.get_task(done.id).await?;
        assert!(done_task.deleted_at.is_none());
        assert_eq!(store.task_ownership(done.id).await?.managed_by, None);
        // The section still holds the completed task, so it survives as a
        // plain section.
        let sections = store.tag_sections(tag.id).await?;
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "Pack");
        assert_eq!(store.section_owner(sections[0].id).await?, None);
        // Binding and app are gone/disabled.
        assert!(store.bindings_for_tag(tag.id).await?.is_empty());
        assert!(!store.app_by_id(app.id).await?.unwrap().enabled);
        Ok(())
    }
}

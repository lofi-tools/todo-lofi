//! Persistence boundary and its first durable implementation.
//!
//! The trait keeps the core free of a database-specific API. `TursoStore`
//! applies the embedded schema, then stores projects and tasks as typed values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_harness::{AttachmentMetadata, Entry, EntryKind};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toasty::stmt::{Type, Value};
use toasty_driver_turso::Turso;
use tokio::sync::{Mutex, RwLock};

use crate::projects::{Project, ProjectId, parse_preview_url};
use crate::search::{RESULT_LIMIT, SearchHit, SearchQuery, entry_text};
use crate::session::SessionRecord;
use crate::snapshots::WorkspaceLayout;
use crate::tasks::{Agent, AgentTier, Task, TaskId, TaskState};
use crate::terminal::{ProcessRecord, TerminalId, TerminalLayout, TerminalRecord, TerminalScope};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store backend error: {0}")]
    Backend(String),
    #[error("database error: {0}")]
    Database(#[from] toasty::Error),
    #[error("migration checksum mismatch for {name}: expected {expected}, got {actual}")]
    MigrationChecksum {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("invalid database row: {0}")]
    InvalidRow(String),
    #[error("task {task_id} is no longer awaiting review in the database")]
    ReviewStateChanged { task_id: TaskId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectApproval {
    pub id: String,
    pub agent: Agent,
    pub title: String,
    pub arguments: serde_json::Value,
    pub paths: Vec<PathBuf>,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn save_process(&self, process: &ProcessRecord) -> Result<(), StoreError>;
    async fn remove_process(&self, id: &str) -> Result<(), StoreError>;
    async fn load_processes(&self) -> Result<Vec<ProcessRecord>, StoreError>;
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError>;
    async fn save_project(&self, project: &Project) -> Result<(), StoreError>;
    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError>;
    async fn save_task(&self, task: &Task) -> Result<(), StoreError>;
    async fn archive_tasks(&self, tasks: &[Task]) -> Result<(), StoreError>;
    async fn delete_task(&self, task_id: &TaskId) -> Result<(), StoreError>;
    async fn load_workspace_layouts(&self) -> Result<Vec<(TaskId, WorkspaceLayout)>, StoreError>;
    async fn save_workspace_layout(
        &self,
        task_id: &TaskId,
        layout: WorkspaceLayout,
    ) -> Result<(), StoreError>;
    async fn load_terminals(&self) -> Result<Vec<TerminalRecord>, StoreError>;
    async fn save_terminal(&self, terminal: &TerminalRecord) -> Result<(), StoreError>;
    async fn load_terminal_layouts(&self) -> Result<Vec<(ProjectId, TerminalLayout)>, StoreError>;
    async fn save_terminal_layout(
        &self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
    ) -> Result<(), StoreError>;
    async fn load_sessions(&self) -> Result<Vec<SessionRecord>, StoreError>;
    async fn save_session(&self, session: &SessionRecord) -> Result<(), StoreError>;
    async fn load_transcript(&self, task_id: &TaskId) -> Result<Vec<Entry>, StoreError>;
    async fn search_entries(&self, _query: &SearchQuery) -> Result<Vec<SearchHit>, StoreError> {
        Err(StoreError::Backend(
            "Task search needs a durable Turso database.".into(),
        ))
    }
    async fn save_transcript_entries(
        &self,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError>;
    async fn project_approvals(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<ProjectApproval>, StoreError>;
    async fn has_project_approval(&self, project: &ProjectId, id: &str)
    -> Result<bool, StoreError>;
    async fn save_project_approval(
        &self,
        project: &ProjectId,
        grant: &ProjectApproval,
    ) -> Result<(), StoreError>;
    async fn reset_project_approval(&self, project: &ProjectId, id: &str)
    -> Result<(), StoreError>;
}

struct Migration {
    name: &'static str,
    sql: &'static str,
}

const SEARCH_SQL: &str = "SELECT (SELECT project_id FROM tasks WHERE tasks.id = entries.task_id),
        task_id, id, project_name, task_title, entry_text,
        fts_score(entry_text, task_title, project_name, ?1) AS score
     FROM entries WHERE fts_match(entry_text, task_title, project_name, ?1)
     ORDER BY score DESC LIMIT ?2";

const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_init.sql",
        sql: include_str!("../toasty/migrations/0001_init.sql"),
    },
    Migration {
        name: "0002_workspace.sql",
        sql: include_str!("../toasty/migrations/0002_workspace.sql"),
    },
    Migration {
        name: "0003_sessions.sql",
        sql: include_str!("../toasty/migrations/0003_sessions.sql"),
    },
    Migration {
        name: "0004_branch_tip.sql",
        sql: include_str!("../toasty/migrations/0004_branch_tip.sql"),
    },
    Migration {
        name: "0005_task_agent.sql",
        sql: include_str!("../toasty/migrations/0005_task_agent.sql"),
    },
    Migration {
        name: "0006_project_approvals.sql",
        sql: include_str!("../toasty/migrations/0006_project_approvals.sql"),
    },
    Migration {
        name: "0007_session_capabilities.sql",
        sql: include_str!("../toasty/migrations/0007_session_capabilities.sql"),
    },
    Migration {
        name: "0008_project_preview_url.sql",
        sql: include_str!("../toasty/migrations/0008_project_preview_url.sql"),
    },
    Migration {
        name: "0009_transcript_entries.sql",
        sql: include_str!("../toasty/migrations/0009_transcript_entries.sql"),
    },
    Migration {
        name: "0010_entries_search.sql",
        sql: include_str!("../toasty/migrations/0010_entries_search.sql"),
    },
    Migration {
        name: "0011_attachments.sql",
        sql: include_str!("../toasty/migrations/0011_attachments.sql"),
    },
    Migration {
        name: "0012_processes.sql",
        sql: include_str!("../toasty/migrations/0012_processes.sql"),
    },
    Migration {
        name: "0013_worktree_last_used.sql",
        sql: include_str!("../toasty/migrations/0013_worktree_last_used.sql"),
    },
];

/// The durable store used by the application. The database handle is guarded
/// because Toasty's raw SQL API needs exclusive access for each statement.
pub struct TursoStore {
    db: Mutex<toasty::db::Db>,
}

impl TursoStore {
    pub async fn open(db_uri: &str) -> Result<Self, StoreError> {
        Self::open_driver(Turso::new(db_uri)?).await
    }

    /// Open (or create) the database at a filesystem path.
    ///
    /// This bypasses URI parsing on purpose: the driver parses `turso:<value>`
    /// as a URL and reads back the encoded path, so a real data directory such
    /// as the macOS `Application Support` one resolves to a literal
    /// `Application%20Support` file that does not exist.
    pub async fn open_at(path: &Path) -> Result<Self, StoreError> {
        Self::open_driver(Turso::file(path)).await
    }

    async fn open_driver(driver: Turso) -> Result<Self, StoreError> {
        let mut db = toasty::Db::builder()
            .build(driver.experimental_index_method(true))
            .await?;
        apply_migrations(&mut db).await?;
        Ok(Self { db: Mutex::new(db) })
    }
}

#[async_trait]
impl Store for TursoStore {
    async fn save_process(&self, process: &ProcessRecord) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let started_at = i64::try_from(process.started_at)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        toasty::sql::statement("INSERT INTO processes (id, kind, owner_id, pid, started_at, process_group, session_id, boot_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")
            .bind(&process.id).bind(&process.kind).bind(&process.owner_id).bind(i64::from(process.pid))
            .bind(started_at).bind(i64::from(process.process_group)).bind(i64::from(process.session_id)).bind(&process.boot_id)
            .exec(&mut *db).await?;
        Ok(())
    }

    async fn remove_process(&self, id: &str) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        toasty::sql::statement("DELETE FROM processes WHERE id = ?1")
            .bind(id)
            .exec(&mut *db)
            .await?;
        Ok(())
    }

    async fn load_processes(&self) -> Result<Vec<ProcessRecord>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query("SELECT id, kind, owner_id, pid, started_at, process_group, session_id, boot_id FROM processes ORDER BY id")
            .column_types([Type::String, Type::String, Type::String, Type::I64, Type::I64, Type::I64, Type::I64, Type::String])
            .exec(&mut *db).await?;
        rows.into_iter()
            .map(|row| {
                let Value::Record(fields) = row else {
                    return Err(StoreError::InvalidRow("expected a process record".into()));
                };
                Ok(ProcessRecord {
                    id: required_string(fields.first())?,
                    kind: required_string(fields.get(1))?,
                    owner_id: required_string(fields.get(2))?,
                    pid: u32::try_from(required_u64(fields.get(3))?)
                        .map_err(|error| StoreError::InvalidRow(error.to_string()))?,
                    started_at: required_u64(fields.get(4))?,
                    process_group: u32::try_from(required_u64(fields.get(5))?)
                        .map_err(|error| StoreError::InvalidRow(error.to_string()))?,
                    session_id: u32::try_from(required_u64(fields.get(6))?)
                        .map_err(|error| StoreError::InvalidRow(error.to_string()))?,
                    boot_id: required_string(fields.get(7))?,
                })
            })
            .collect()
    }
    async fn search_entries(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(SEARCH_SQL)
            .column_types([
                Type::String,
                Type::String,
                Type::String,
                Type::String,
                Type::String,
                Type::String,
                Type::F64,
            ])
            .bind(query.fts())
            .bind(RESULT_LIMIT)
            .exec(&mut *db)
            .await?;
        drop(db);
        rows.into_iter()
            .map(|row| {
                let Value::Record(fields) = row else {
                    return Err(StoreError::InvalidRow(
                        "expected a search result record".into(),
                    ));
                };
                let project_id = optional_string(fields.first())?
                    .ok_or_else(|| StoreError::InvalidRow("search result has no project".into()))?;
                let task_id = required_string(fields.get(1))?;
                let entry_id = required_string(fields.get(2))?;
                let project_name = required_string(fields.get(3))?;
                let task_title = required_string(fields.get(4))?;
                let entry = required_string(fields.get(5))?;
                let Some(Value::F64(score)) = fields.get(6) else {
                    return Err(StoreError::InvalidRow(
                        "expected a full-text search score".into(),
                    ));
                };
                Ok(SearchHit {
                    project_id,
                    task_id,
                    entry_id,
                    snippet: query.snippet(&entry, &task_title, &project_name),
                    project_name,
                    task_title,
                    score: *score,
                })
            })
            .collect()
    }

    async fn load_transcript(&self, task_id: &TaskId) -> Result<Vec<Entry>, StoreError> {
        let mut db = self.db.lock().await;
        let rows =
            toasty::sql::query("SELECT payload FROM entries WHERE task_id = ?1 ORDER BY sequence")
                .column_types([Type::String])
                .bind(task_id.as_str())
                .exec(&mut *db)
                .await?;
        let mut entries = rows
            .into_iter()
            .map(|row| {
                let Value::Record(fields) = row else {
                    return Err(StoreError::InvalidRow(
                        "expected a transcript entry record".into(),
                    ));
                };
                let json = required_string(fields.first())?;
                serde_json::from_str::<Entry>(&json).map_err(|error| {
                    StoreError::InvalidRow(format!("invalid transcript entry: {error}"))
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        if entries.is_empty() {
            return Ok(entries);
        }
        let rows = toasty::sql::query(
            "SELECT entry_id, id, relative_path, mime, bytes FROM attachments
             WHERE task_id = ?1 ORDER BY entry_id, position",
        )
        .column_types([
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::I64,
        ])
        .bind(task_id.as_str())
        .exec(&mut *db)
        .await?;
        let mut by_entry = BTreeMap::<String, Vec<AttachmentMetadata>>::new();
        for row in rows {
            let Value::Record(fields) = row else {
                return Err(StoreError::InvalidRow(
                    "expected an attachment record".into(),
                ));
            };
            by_entry
                .entry(required_string(fields.first())?)
                .or_default()
                .push(AttachmentMetadata {
                    id: required_string(fields.get(1))?,
                    relative_path: PathBuf::from(required_string(fields.get(2))?),
                    mime: required_string(fields.get(3))?,
                    bytes: required_u64(fields.get(4))?,
                });
        }
        for entry in &mut entries {
            entry.attachments = by_entry.remove(&entry.id).unwrap_or_default();
        }
        Ok(entries)
    }

    async fn save_transcript_entries(
        &self,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        let created_at = crate::projects::now_millis() as i64;
        for entry in entries {
            let mut payload = serde_json::to_value(entry).map_err(|error| {
                StoreError::InvalidRow(format!("could not encode transcript entry: {error}"))
            })?;
            if let Some(fields) = payload.as_object_mut() {
                fields.remove("attachments");
            }
            let kind = payload
                .pointer("/kind/type")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| StoreError::InvalidRow("missing transcript entry kind".into()))?;
            toasty::sql::statement(
                "INSERT INTO entries (session_id, task_id, id, sequence, kind, payload, created_at,
                                      entry_text, task_title, project_name)
                 SELECT ?1, tasks.id, ?3, ?4, ?5, ?6, ?7, ?8, tasks.title, projects.name
                 FROM tasks JOIN projects ON projects.id = tasks.project_id WHERE tasks.id = ?2
                 ON CONFLICT(task_id, id) DO UPDATE SET kind = excluded.kind, payload = excluded.payload,
                     entry_text = excluded.entry_text, task_title = excluded.task_title, project_name = excluded.project_name",
            )
            .bind(&session.id)
            .bind(session.task_id.as_str())
            .bind(&entry.id)
            .bind(entry.sequence as i64)
            .bind(kind)
            .bind(payload.to_string())
            .bind(created_at)
            .bind(entry_text(&entry.kind))
            .exec(&mut transaction)
            .await?;
            if matches!(entry.kind, EntryKind::UserMessage { .. }) {
                for (position, attachment) in entry.attachments.iter().enumerate() {
                    let path = attachment
                        .relative_path
                        .to_str()
                        .filter(|path| {
                            !path.is_empty()
                                && attachment
                                    .relative_path
                                    .components()
                                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                        })
                        .ok_or_else(|| {
                            StoreError::InvalidRow("expected a relative attachment path".into())
                        })?;
                    let bytes = i64::try_from(attachment.bytes).map_err(|_| {
                        StoreError::InvalidRow("attachment size exceeds database range".into())
                    })?;
                    toasty::sql::statement(
                        "INSERT INTO attachments (task_id, entry_id, position, id, relative_path, mime, bytes, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                         ON CONFLICT(task_id, entry_id, position) DO UPDATE SET id = excluded.id,
                             relative_path = excluded.relative_path, mime = excluded.mime, bytes = excluded.bytes",
                    )
                    .bind(session.task_id.as_str())
                    .bind(&entry.id)
                    .bind(position as i64)
                    .bind(&attachment.id)
                    .bind(path)
                    .bind(&attachment.mime)
                    .bind(bytes)
                    .bind(created_at)
                    .exec(&mut transaction)
                    .await?;
                }
                toasty::sql::statement(
                    "DELETE FROM attachments WHERE task_id = ?1 AND entry_id = ?2 AND position >= ?3",
                )
                .bind(session.task_id.as_str())
                .bind(&entry.id)
                .bind(entry.attachments.len() as i64)
                .exec(&mut transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn project_approvals(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<ProjectApproval>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT grant_json FROM project_approvals WHERE project_id = ?1 ORDER BY id",
        )
        .column_types([Type::String])
        .bind(project.as_str())
        .exec(&mut *db)
        .await?;
        rows.into_iter()
            .map(|row| {
                let Value::Record(record) = row else {
                    return Err(StoreError::InvalidRow(
                        "expected a project approval record".into(),
                    ));
                };
                serde_json::from_str(&required_string(record.first())?).map_err(|error| {
                    StoreError::InvalidRow(format!("invalid project approval: {error}"))
                })
            })
            .collect()
    }

    async fn has_project_approval(
        &self,
        project: &ProjectId,
        id: &str,
    ) -> Result<bool, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id FROM project_approvals WHERE project_id = ?1 AND id = ?2",
        )
        .column_types([Type::String])
        .bind(project.as_str())
        .bind(id)
        .exec(&mut *db)
        .await?;
        Ok(!rows.is_empty())
    }

    async fn save_project_approval(
        &self,
        project: &ProjectId,
        grant: &ProjectApproval,
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(grant).map_err(|error| {
            StoreError::InvalidRow(format!("could not encode project approval: {error}"))
        })?;
        let mut db = self.db.lock().await;
        toasty::sql::statement(
            "INSERT INTO project_approvals (project_id, id, grant_json) \
             VALUES (?1, ?2, ?3) ON CONFLICT (project_id, id) DO NOTHING",
        )
        .bind(project.as_str())
        .bind(&grant.id)
        .bind(&json)
        .exec(&mut *db)
        .await?;
        Ok(())
    }

    async fn reset_project_approval(
        &self,
        project: &ProjectId,
        id: &str,
    ) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        toasty::sql::statement("DELETE FROM project_approvals WHERE project_id = ?1 AND id = ?2")
            .bind(project.as_str())
            .bind(id)
            .exec(&mut *db)
            .await?;
        Ok(())
    }

    async fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, path, name, missing, last_task_id, created_at, preview_url \
             FROM projects ORDER BY created_at, id",
        )
        .column_types([
            Type::String,
            Type::String,
            Type::String,
            Type::I64,
            Type::String,
            Type::I64,
            Type::String,
        ])
        .exec(&mut *db)
        .await?;
        rows.into_iter().map(parse_project).collect()
    }

    async fn save_project(&self, project: &Project) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let last_task_id = project
            .last_task_id
            .as_ref()
            .map(|task_id| task_id.as_str())
            .unwrap_or_default();
        toasty::sql::statement(
            "INSERT INTO projects (id, path, name, missing, last_task_id, created_at, preview_url) \
             VALUES (?1, ?2, ?3, ?4, NULLIF(?5, ''), ?6, NULLIF(?7, '')) \
             ON CONFLICT (id) DO UPDATE SET \
                path = excluded.path, \
                name = excluded.name, \
                missing = excluded.missing, \
                last_task_id = excluded.last_task_id, \
                created_at = excluded.created_at, \
                preview_url = excluded.preview_url",
        )
        .bind(project.id.as_str())
        .bind(project.path.to_string_lossy().as_ref())
        .bind(&project.name)
        .bind(i64::from(project.missing))
        .bind(last_task_id)
        .bind(project.created_at as i64)
        .bind(
            project
                .preview_url
                .as_ref()
                .map(url::Url::as_str)
                .unwrap_or_default(),
        )
        .exec(&mut *db)
        .await?;
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, project_id, title, state, tier, proposed_worktree_path, \
                    proposed_branch, worktree_path, branch, base_branch, error, \
                    created_at, updated_at, branch_tip_sha, agent, worktree_last_used_at \
             FROM tasks ORDER BY created_at, id",
        )
        .column_types([
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::I64,
            Type::I64,
            Type::String,
            Type::String,
            Type::I64,
        ])
        .exec(&mut *db)
        .await?;
        rows.into_iter().map(parse_task).collect()
    }

    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let worktree_path = optional_path(task.worktree_path.as_ref());
        let branch = task.branch.as_deref().unwrap_or_default();
        let base_branch = task.base_branch.as_deref().unwrap_or_default();
        let error = task.error.as_deref().unwrap_or_default();
        toasty::sql::statement(
            "INSERT INTO tasks (\
                id, project_id, title, state, tier, proposed_worktree_path, \
                proposed_branch, worktree_path, branch, base_branch, error, \
                created_at, updated_at, branch_tip_sha, agent, worktree_last_used_at\
             ) VALUES (\
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, \
                NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), ?12, ?13, NULLIF(?14, ''), ?15, ?16\
             ) ON CONFLICT (id) DO UPDATE SET \
                project_id = excluded.project_id, \
                title = excluded.title, \
                state = excluded.state, \
                tier = excluded.tier, \
                proposed_worktree_path = excluded.proposed_worktree_path, \
                proposed_branch = excluded.proposed_branch, \
                worktree_path = excluded.worktree_path, \
                branch = excluded.branch, \
                base_branch = excluded.base_branch, \
                error = excluded.error, \
                created_at = excluded.created_at, \
                updated_at = excluded.updated_at, \
                branch_tip_sha = excluded.branch_tip_sha, \
                agent = excluded.agent, \
                worktree_last_used_at = excluded.worktree_last_used_at",
        )
        .bind(task.id.as_str())
        .bind(task.project_id.as_str())
        .bind(&task.title)
        .bind(task.state.as_str())
        .bind(task.tier.as_str())
        .bind(task.proposed_worktree_path.to_string_lossy().as_ref())
        .bind(&task.proposed_branch)
        .bind(worktree_path)
        .bind(branch)
        .bind(base_branch)
        .bind(error)
        .bind(task.created_at as i64)
        .bind(task.updated_at as i64)
        .bind(task.branch_tip_sha.as_deref().unwrap_or_default())
        .bind(task.agent.as_str())
        .bind(task.worktree_last_used_at as i64)
        .exec(&mut *db)
        .await?;
        Ok(())
    }

    async fn archive_tasks(&self, tasks: &[Task]) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        for task in tasks {
            let updated = toasty::sql::statement(
                "UPDATE tasks SET state = 'done', updated_at = ?2 WHERE id = ?1 AND state = 'review'",
            )
            .bind(task.id.as_str())
            .bind(task.updated_at as i64)
            .exec(&mut transaction)
            .await?;
            if updated != 1 {
                transaction.rollback().await?;
                return Err(StoreError::ReviewStateChanged {
                    task_id: task.id.clone(),
                });
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn delete_task(&self, task_id: &TaskId) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        for statement in [
            "DELETE FROM attachments WHERE task_id = ?1",
            "DELETE FROM entries WHERE task_id = ?1",
            "DELETE FROM sessions WHERE task_id = ?1",
            "DELETE FROM terminals WHERE task_id = ?1",
            "DELETE FROM layouts WHERE task_id = ?1",
            "DELETE FROM tasks WHERE id = ?1",
        ] {
            toasty::sql::statement(statement)
                .bind(task_id.as_str())
                .exec(&mut transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn load_terminals(&self) -> Result<Vec<TerminalRecord>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, project_id, task_id, scope, title, cwd, command, \
                    created_at, ended_at, exit_code \
             FROM terminals ORDER BY created_at, id",
        )
        .column_types([
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::I64,
            Type::I64,
            Type::I64,
        ])
        .exec(&mut *db)
        .await?;
        rows.into_iter().map(parse_terminal).collect()
    }

    async fn save_terminal(&self, terminal: &TerminalRecord) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let task_id = terminal
            .task_id
            .as_ref()
            .map(|task_id| task_id.as_str())
            .unwrap_or_default();
        // `-1` is the absent marker: timestamps are positive and an exit code of
        // `-1` cannot come from a process this app spawned.
        let ended_at = terminal.ended_at.map(|value| value as i64).unwrap_or(-1);
        let exit_code = terminal.exit_code.map(i64::from).unwrap_or(-1);
        toasty::sql::statement(
            "INSERT INTO terminals (\
                id, project_id, task_id, scope, title, cwd, command, \
                created_at, ended_at, exit_code\
             ) VALUES (\
                ?1, ?2, NULLIF(?3, ''), ?4, ?5, ?6, ?7, ?8, NULLIF(?9, -1), NULLIF(?10, -1)\
             ) ON CONFLICT (id) DO UPDATE SET \
                project_id = excluded.project_id, \
                task_id = excluded.task_id, \
                scope = excluded.scope, \
                title = excluded.title, \
                cwd = excluded.cwd, \
                command = excluded.command, \
                created_at = excluded.created_at, \
                ended_at = excluded.ended_at, \
                exit_code = excluded.exit_code",
        )
        .bind(terminal.id.as_str())
        .bind(terminal.project_id.as_str())
        .bind(task_id)
        .bind(terminal.scope.as_str())
        .bind(&terminal.title)
        .bind(terminal.cwd.to_string_lossy().as_ref())
        .bind(&terminal.command)
        .bind(terminal.created_at as i64)
        .bind(ended_at)
        .bind(exit_code)
        .exec(&mut *db)
        .await?;
        Ok(())
    }

    async fn load_workspace_layouts(&self) -> Result<Vec<(TaskId, WorkspaceLayout)>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query("SELECT task_id, workspace_json FROM layouts")
            .column_types([Type::String, Type::String])
            .exec(&mut *db)
            .await?;
        let mut layouts = Vec::new();
        for row in rows {
            let Value::Record(fields) = row else {
                return Err(StoreError::InvalidRow(
                    "expected a workspace layout record".into(),
                ));
            };
            let task = TaskId::new(required_string(fields.first())?);
            let json = required_string(fields.get(1))?;
            match serde_json::from_str(&json) {
                Ok(layout) => layouts.push((task, layout)),
                Err(error) => tracing::warn!(%task, %error, "ignored unreadable workspace layout"),
            }
        }
        Ok(layouts)
    }

    async fn save_workspace_layout(
        &self,
        task_id: &TaskId,
        layout: WorkspaceLayout,
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(&layout)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let mut db = self.db.lock().await;
        toasty::sql::statement(
            "INSERT INTO layouts (task_id, workspace_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (task_id) DO UPDATE SET workspace_json = excluded.workspace_json, updated_at = excluded.updated_at",
        )
        .bind(task_id.as_str())
        .bind(json)
        .bind(crate::projects::now_millis() as i64)
        .exec(&mut *db)
        .await?;
        Ok(())
    }

    async fn load_terminal_layouts(&self) -> Result<Vec<(ProjectId, TerminalLayout)>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query("SELECT project_id, layout_json FROM terminal_layouts")
            .column_types([Type::String, Type::String])
            .exec(&mut *db)
            .await?;
        let mut layouts = Vec::new();
        for row in rows {
            let Value::Record(fields) = row else {
                return Err(StoreError::InvalidRow(
                    "expected a terminal layout record".into(),
                ));
            };
            let project = ProjectId::new(required_string(fields.first())?);
            let json = required_string(fields.get(1))?;
            match serde_json::from_str(&json) {
                Ok(layout) => layouts.push((project, layout)),
                Err(error) => {
                    tracing::warn!(%project, %error, "ignored unreadable terminal layout")
                }
            }
        }
        Ok(layouts)
    }

    async fn save_terminal_layout(
        &self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(layout)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let mut db = self.db.lock().await;
        toasty::sql::statement(
            "INSERT INTO terminal_layouts (project_id, layout_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (project_id) DO UPDATE SET layout_json = excluded.layout_json, updated_at = excluded.updated_at",
        ).bind(project_id.as_str()).bind(json).bind(crate::projects::now_millis() as i64).exec(&mut *db).await?;
        Ok(())
    }

    async fn load_sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        self.select_sessions().await
    }

    async fn save_session(&self, session: &SessionRecord) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let adapter_session_id = session.adapter_session_id.as_deref().unwrap_or_default();
        // The column predates resumable sessions; the value only has to be
        // stable, and an end time is what actually distinguishes the two states.
        let state = if session.is_ended() { "ended" } else { "live" };
        let ended_at = session.ended_at.map(|value| value as i64).unwrap_or(-1);
        let exit_code = session.exit_code.map(i64::from).unwrap_or(-1);
        let stderr = session.stderr.as_deref().unwrap_or_default();
        let capabilities = session
            .capabilities
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        toasty::sql::statement(
            "INSERT INTO sessions (\
                id, task_id, adapter, state, adapter_session_id, started_at, \
                ended_at, exit_code, stderr, capabilities_json\
             ) VALUES (\
                ?1, ?2, ?3, ?4, NULLIF(?5, ''), ?6, \
                NULLIF(?7, -1), NULLIF(?8, -1), NULLIF(?9, ''), NULLIF(?10, '')\
             ) ON CONFLICT (id) DO UPDATE SET \
                task_id = excluded.task_id, \
                adapter = excluded.adapter, \
                state = excluded.state, \
                adapter_session_id = excluded.adapter_session_id, \
                started_at = excluded.started_at, \
                ended_at = excluded.ended_at, \
                exit_code = excluded.exit_code, \
                stderr = excluded.stderr, \
                capabilities_json = excluded.capabilities_json",
        )
        .bind(session.id.as_str())
        .bind(session.task_id.as_str())
        .bind(&session.adapter)
        .bind(state)
        .bind(adapter_session_id)
        .bind(session.started_at as i64)
        .bind(ended_at)
        .bind(exit_code)
        .bind(stderr)
        .bind(capabilities.as_deref().unwrap_or_default())
        .exec(&mut *db)
        .await?;
        Ok(())
    }
}

impl TursoStore {
    async fn select_sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, task_id, adapter, adapter_session_id, started_at, ended_at, \
                    exit_code, stderr, capabilities_json \
             FROM sessions ORDER BY started_at, rowid",
        )
        .column_types([
            Type::String,
            Type::String,
            Type::String,
            Type::String,
            Type::I64,
            Type::I64,
            Type::I64,
            Type::String,
            Type::String,
        ])
        .exec(&mut *db)
        .await?;
        rows.into_iter().map(parse_session).collect()
    }
}

async fn apply_migrations(db: &mut toasty::db::Db) -> Result<(), StoreError> {
    toasty::sql::statement(
        "CREATE TABLE IF NOT EXISTS _migrations_history (\
            name TEXT NOT NULL PRIMARY KEY, \
            checksum TEXT NOT NULL\
        )",
    )
    .exec(&mut *db)
    .await?;

    for migration in MIGRATIONS {
        let expected = checksum(migration.sql);
        let rows = toasty::sql::query("SELECT checksum FROM _migrations_history WHERE name = ?1")
            .column_types([Type::String])
            .bind(migration.name)
            .exec(&mut *db)
            .await?;

        if let Some(row) = rows.into_iter().next() {
            let Value::Record(record) = row else {
                return Err(StoreError::InvalidRow(
                    "expected a migration history record".to_string(),
                ));
            };
            let actual = required_string(record.first())?;
            if actual != expected {
                return Err(StoreError::MigrationChecksum {
                    name: migration.name.to_string(),
                    expected,
                    actual,
                });
            }
            continue;
        }

        let mut transaction = db.transaction().await?;
        for statement in split_sql(migration.sql) {
            toasty::sql::statement(statement)
                .exec(&mut transaction)
                .await?;
        }
        toasty::sql::statement("INSERT INTO _migrations_history (name, checksum) VALUES (?1, ?2)")
            .bind(migration.name)
            .bind(&expected)
            .exec(&mut transaction)
            .await?;
        transaction.commit().await?;
    }

    Ok(())
}

pub(crate) fn checksum(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let bytes = hasher.finalize();
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn split_sql(sql: &str) -> impl Iterator<Item = &str> {
    sql.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
}

fn parse_project(value: Value) -> Result<Project, StoreError> {
    let Value::Record(record) = value else {
        return Err(StoreError::InvalidRow(
            "expected a project record".to_string(),
        ));
    };
    Ok(Project {
        id: ProjectId::new(required_string(record.first())?),
        path: PathBuf::from(required_string(record.get(1))?),
        name: required_string(record.get(2))?,
        missing: required_missing(record.get(3))?,
        last_task_id: optional_string(record.get(4))?.map(TaskId::new),
        created_at: required_u64(record.get(5))?,
        preview_url: optional_string(record.get(6))?
            .as_deref()
            .and_then(parse_preview_url),
    })
}

fn parse_task(value: Value) -> Result<Task, StoreError> {
    let Value::Record(record) = value else {
        return Err(StoreError::InvalidRow("expected a task record".to_string()));
    };
    let state_name = required_string(record.get(3))?;
    let state = TaskState::parse(&state_name)
        .ok_or_else(|| StoreError::InvalidRow(format!("unknown task state: {state_name}")))?;
    let tier_name = required_string(record.get(4))?;
    let tier = AgentTier::parse(&tier_name)
        .ok_or_else(|| StoreError::InvalidRow(format!("unknown agent tier: {tier_name}")))?;
    let agent_name = required_string(record.get(14))?;
    let agent = Agent::parse(&agent_name)
        .ok_or_else(|| StoreError::InvalidRow(format!("unknown task agent: {agent_name}")))?;
    Ok(Task {
        id: TaskId::new(required_string(record.first())?),
        project_id: ProjectId::new(required_string(record.get(1))?),
        title: required_string(record.get(2))?,
        state,
        agent,
        tier,
        proposed_worktree_path: PathBuf::from(required_string(record.get(5))?),
        proposed_branch: required_string(record.get(6))?,
        worktree_path: optional_string(record.get(7))?.map(PathBuf::from),
        branch: optional_string(record.get(8))?,
        base_branch: optional_string(record.get(9))?,
        error: optional_string(record.get(10))?,
        created_at: required_u64(record.get(11))?,
        updated_at: required_u64(record.get(12))?,
        worktree_last_used_at: required_u64(record.get(15))?,
        branch_tip_sha: optional_string(record.get(13))?,
    })
}

fn parse_session(value: Value) -> Result<SessionRecord, StoreError> {
    let Value::Record(record) = value else {
        return Err(StoreError::InvalidRow(
            "expected a session record".to_string(),
        ));
    };
    let capabilities =
        optional_string(record.get(8))?.and_then(|json| match serde_json::from_str(&json) {
            Ok(capabilities) => Some(capabilities),
            Err(error) => {
                tracing::warn!(%error, "ignored unreadable session capabilities");
                None
            }
        });
    Ok(SessionRecord {
        id: required_string(record.first())?,
        task_id: TaskId::new(required_string(record.get(1))?),
        adapter: required_string(record.get(2))?,
        adapter_session_id: optional_string(record.get(3))?,
        started_at: required_u64(record.get(4))?,
        ended_at: optional_i64(record.get(5))?.map(|value| value as u64),
        exit_code: optional_i64(record.get(6))?.map(|value| value as i32),
        stderr: optional_string(record.get(7))?,
        capabilities,
    })
}

fn parse_terminal(value: Value) -> Result<TerminalRecord, StoreError> {
    let Value::Record(record) = value else {
        return Err(StoreError::InvalidRow(
            "expected a terminal record".to_string(),
        ));
    };
    let scope_name = required_string(record.get(3))?;
    let scope = TerminalScope::parse(&scope_name)
        .ok_or_else(|| StoreError::InvalidRow(format!("unknown terminal scope: {scope_name}")))?;
    Ok(TerminalRecord {
        id: TerminalId::new(required_string(record.first())?),
        project_id: ProjectId::new(required_string(record.get(1))?),
        task_id: optional_string(record.get(2))?.map(TaskId::new),
        scope,
        title: required_string(record.get(4))?,
        cwd: PathBuf::from(required_string(record.get(5))?),
        command: required_string(record.get(6))?,
        created_at: required_u64(record.get(7))?,
        ended_at: optional_i64(record.get(8))?.map(|value| value as u64),
        exit_code: optional_i64(record.get(9))?.map(|value| value as i32),
    })
}

fn optional_i64(value: Option<&Value>) -> Result<Option<i64>, StoreError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::I64(value)) if *value == -1 => Ok(None),
        Some(Value::I64(value)) => Ok(Some(*value)),
        _ => Err(StoreError::InvalidRow(
            "expected a nullable integer column".to_string(),
        )),
    }
}

fn required_string(value: Option<&Value>) -> Result<String, StoreError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(StoreError::InvalidRow(
            "expected a non-null string column".to_string(),
        )),
    }
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, StoreError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(StoreError::InvalidRow(
            "expected a nullable string column".to_string(),
        )),
    }
}

fn required_u64(value: Option<&Value>) -> Result<u64, StoreError> {
    match value {
        Some(Value::I64(value)) => u64::try_from(*value)
            .map_err(|_| StoreError::InvalidRow("negative integer column".to_string())),
        _ => Err(StoreError::InvalidRow(
            "expected an integer column".to_string(),
        )),
    }
}

fn required_missing(value: Option<&Value>) -> Result<bool, StoreError> {
    match value {
        Some(Value::I64(value)) => Ok(*value != 0),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(StoreError::InvalidRow(
            "expected a missing-path integer column".to_string(),
        )),
    }
}

fn optional_path(path: Option<&PathBuf>) -> String {
    path.map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A non-durable store used to test the boundary before a database is opened.
#[derive(Default)]
pub struct MemoryStore {
    processes: RwLock<BTreeMap<String, ProcessRecord>>,
    projects: RwLock<BTreeMap<String, Project>>,
    tasks: RwLock<BTreeMap<String, Task>>,
    workspace_layouts: RwLock<BTreeMap<TaskId, WorkspaceLayout>>,
    terminals: RwLock<BTreeMap<String, TerminalRecord>>,
    terminal_layouts: RwLock<BTreeMap<ProjectId, TerminalLayout>>,
    sessions: RwLock<Vec<SessionRecord>>,
    transcripts: RwLock<BTreeMap<TaskId, BTreeMap<u64, Entry>>>,
    project_approvals: RwLock<BTreeMap<ProjectId, BTreeMap<String, ProjectApproval>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn save_process(&self, process: &ProcessRecord) -> Result<(), StoreError> {
        self.processes
            .write()
            .await
            .insert(process.id.clone(), process.clone());
        Ok(())
    }

    async fn remove_process(&self, id: &str) -> Result<(), StoreError> {
        self.processes.write().await.remove(id);
        Ok(())
    }

    async fn load_processes(&self) -> Result<Vec<ProcessRecord>, StoreError> {
        Ok(self.processes.read().await.values().cloned().collect())
    }
    async fn load_transcript(&self, task_id: &TaskId) -> Result<Vec<Entry>, StoreError> {
        Ok(self
            .transcripts
            .read()
            .await
            .get(task_id)
            .into_iter()
            .flat_map(|entries| entries.values().cloned())
            .collect())
    }

    async fn save_transcript_entries(
        &self,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError> {
        let mut transcripts = self.transcripts.write().await;
        let transcript = transcripts.entry(session.task_id.clone()).or_default();
        for entry in entries {
            transcript.insert(entry.sequence, (*entry).clone());
        }
        Ok(())
    }

    async fn project_approvals(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<ProjectApproval>, StoreError> {
        let approvals = self.project_approvals.read().await;
        Ok(approvals
            .get(project)
            .into_iter()
            .flat_map(|grants| grants.values().cloned())
            .collect())
    }

    async fn has_project_approval(
        &self,
        project: &ProjectId,
        id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .project_approvals
            .read()
            .await
            .get(project)
            .is_some_and(|grants| grants.contains_key(id)))
    }

    async fn save_project_approval(
        &self,
        project: &ProjectId,
        grant: &ProjectApproval,
    ) -> Result<(), StoreError> {
        self.project_approvals
            .write()
            .await
            .entry(project.clone())
            .or_default()
            .entry(grant.id.clone())
            .or_insert_with(|| grant.clone());
        Ok(())
    }

    async fn reset_project_approval(
        &self,
        project: &ProjectId,
        id: &str,
    ) -> Result<(), StoreError> {
        if let Some(grants) = self.project_approvals.write().await.get_mut(project) {
            grants.remove(id);
        }
        Ok(())
    }

    async fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        let projects = self.projects.read().await;
        Ok(projects.values().cloned().collect())
    }

    async fn save_project(&self, project: &Project) -> Result<(), StoreError> {
        let mut projects = self.projects.write().await;
        projects.insert(project.id.as_str().to_string(), project.clone());
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let tasks = self.tasks.read().await;
        Ok(tasks.values().cloned().collect())
    }

    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        tasks.insert(task.id.as_str().to_string(), task.clone());
        Ok(())
    }

    async fn archive_tasks(&self, archived: &[Task]) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        if let Some(task) = archived.iter().find(|task| {
            tasks
                .get(task.id.as_str())
                .is_none_or(|stored| stored.state != TaskState::Review)
        }) {
            return Err(StoreError::ReviewStateChanged {
                task_id: task.id.clone(),
            });
        }
        for task in archived {
            tasks.insert(task.id.as_str().to_string(), task.clone());
        }
        Ok(())
    }

    async fn delete_task(&self, task_id: &TaskId) -> Result<(), StoreError> {
        self.tasks.write().await.remove(task_id.as_str());
        self.workspace_layouts.write().await.remove(task_id);
        self.transcripts.write().await.remove(task_id);
        self.sessions
            .write()
            .await
            .retain(|session| &session.task_id != task_id);
        self.terminals
            .write()
            .await
            .retain(|_, terminal| terminal.task_id.as_ref() != Some(task_id));
        Ok(())
    }

    async fn load_workspace_layouts(&self) -> Result<Vec<(TaskId, WorkspaceLayout)>, StoreError> {
        Ok(self
            .workspace_layouts
            .read()
            .await
            .iter()
            .map(|(task, layout)| (task.clone(), *layout))
            .collect())
    }

    async fn save_workspace_layout(
        &self,
        task_id: &TaskId,
        layout: WorkspaceLayout,
    ) -> Result<(), StoreError> {
        self.workspace_layouts
            .write()
            .await
            .insert(task_id.clone(), layout);
        Ok(())
    }

    async fn load_terminals(&self) -> Result<Vec<TerminalRecord>, StoreError> {
        let terminals = self.terminals.read().await;
        Ok(terminals.values().cloned().collect())
    }

    async fn save_terminal(&self, terminal: &TerminalRecord) -> Result<(), StoreError> {
        let mut terminals = self.terminals.write().await;
        terminals.insert(terminal.id.as_str().to_string(), terminal.clone());
        Ok(())
    }

    async fn load_terminal_layouts(&self) -> Result<Vec<(ProjectId, TerminalLayout)>, StoreError> {
        Ok(self
            .terminal_layouts
            .read()
            .await
            .iter()
            .map(|(project, layout)| (project.clone(), layout.clone()))
            .collect())
    }

    async fn save_terminal_layout(
        &self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
    ) -> Result<(), StoreError> {
        self.terminal_layouts
            .write()
            .await
            .insert(project_id.clone(), layout.clone());
        Ok(())
    }

    async fn load_sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let sessions = self.sessions.read().await;
        let mut sessions = sessions.clone();
        // Stable, so sessions started in the same millisecond keep insertion order
        // and the last record stays the newest.
        sessions.sort_by_key(|session| session.started_at);
        Ok(sessions)
    }

    async fn save_session(&self, session: &SessionRecord) -> Result<(), StoreError> {
        let mut sessions = self.sessions.write().await;
        sessions.retain(|seen| seen.id != session.id);
        sessions.push(session.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::search::{SearchField, TaskSearch};
    use crate::test_support::{TestDatabase, TestDirectory};

    async fn table_names(store: &TursoStore) -> Result<Vec<String>, StoreError> {
        let mut db = store.db.lock().await;
        let rows =
            toasty::sql::query("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .column_types([Type::String])
                .exec(&mut *db)
                .await?;
        rows.into_iter()
            .map(|row| {
                let Value::Record(record) = row else {
                    return Err(StoreError::InvalidRow(
                        "expected a table record".to_string(),
                    ));
                };
                required_string(record.first())
            })
            .collect()
    }

    #[tokio::test]
    async fn process_identity_survives_store_reopen_until_removed()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let process = ProcessRecord {
            id: "launch-1".into(),
            kind: "terminal".into(),
            owner_id: "terminal-1".into(),
            pid: 123,
            started_at: 456,
            process_group: 123,
            session_id: 123,
            boot_id: "boot-a".into(),
        };
        for store in [
            Box::new(MemoryStore::new()) as Box<dyn Store>,
            Box::new(TursoStore::open(&uri).await?),
        ] {
            store.save_process(&process).await?;
            assert_eq!(store.load_processes().await?, vec![process.clone()]);
        }
        let reopened = TursoStore::open(&uri).await?;
        assert_eq!(reopened.load_processes().await?, vec![process.clone()]);
        reopened.remove_process(&process.id).await?;
        assert!(reopened.load_processes().await?.is_empty());
        Ok(())
    }

    async fn verify_task_deletion(
        store: &dyn Store,
        root: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let project = Project::new(root.to_path_buf());
        let removed = Task::draft(project.id.clone(), "Removed", root);
        let kept = Task::draft(project.id.clone(), "Kept", root);
        store.save_project(&project).await?;
        store.save_task(&removed).await?;
        store.save_task(&kept).await?;
        let layout = WorkspaceLayout {
            inspector_width: Some(320),
        };
        store.save_workspace_layout(&removed.id, layout).await?;
        store.save_workspace_layout(&kept.id, layout).await?;
        store
            .save_session(&SessionRecord::started(removed.id.clone(), "codex", None))
            .await?;
        store
            .save_session(&SessionRecord::started(kept.id.clone(), "codex", None))
            .await?;
        let shared = TerminalRecord::project_shared(
            TerminalId::new("shared"),
            project.id.clone(),
            "Shared",
            root.to_path_buf(),
            "shell",
            1,
        );
        let removed_terminal = TerminalRecord::project_shared(
            TerminalId::new("removed"),
            project.id.clone(),
            "Removed",
            root.to_path_buf(),
            "shell",
            2,
        )
        .for_task(removed.id.clone());
        let kept_terminal = TerminalRecord::project_shared(
            TerminalId::new("kept"),
            project.id.clone(),
            "Kept",
            root.to_path_buf(),
            "shell",
            3,
        )
        .for_task(kept.id.clone());
        for terminal in [&shared, &removed_terminal, &kept_terminal] {
            store.save_terminal(terminal).await?;
        }
        store.delete_task(&removed.id).await?;
        assert_eq!(
            store
                .load_tasks()
                .await?
                .iter()
                .map(|task| task.id.clone())
                .collect::<Vec<_>>(),
            [kept.id.clone()]
        );
        assert_eq!(
            store.load_workspace_layouts().await?,
            vec![(kept.id.clone(), layout)]
        );
        assert_eq!(
            store
                .load_sessions()
                .await?
                .iter()
                .map(|session| session.task_id.clone())
                .collect::<Vec<_>>(),
            [kept.id]
        );
        let ids: Vec<_> = store
            .load_terminals()
            .await?
            .into_iter()
            .map(|terminal| terminal.id.as_str().to_owned())
            .collect();
        assert!(ids.contains(&"shared".to_owned()));
        assert!(ids.contains(&"kept".to_owned()));
        assert!(!ids.contains(&"removed".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn task_deletion_removes_only_its_durable_state() -> Result<(), Box<dyn std::error::Error>>
    {
        let database = TestDatabase::new()?;
        verify_task_deletion(&MemoryStore::new(), database.path()).await?;
        let uri = database.uri();
        let store = TursoStore::open(&uri).await?;
        verify_task_deletion(&store, database.path()).await?;
        drop(store);
        let reopened = TursoStore::open(&uri).await?;
        assert_eq!(reopened.load_tasks().await?.len(), 1);
        assert_eq!(reopened.load_workspace_layouts().await?.len(), 1);
        assert_eq!(reopened.load_sessions().await?.len(), 1);
        assert_eq!(reopened.load_terminals().await?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn turso_search_ranks_exact_and_prefix_matches_and_tracks_entry_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let store = Arc::new(TursoStore::open(&uri).await?);
        let search = TaskSearch(store.clone());
        let first_project = Project::new(database.path().join("Starlight"));
        let other_project = Project::new(database.path().join("North"));
        store.save_project(&first_project).await?;
        store.save_project(&other_project).await?;
        let exact = Task::draft(first_project.id.clone(), "Renderer", database.path());
        let completion = Task::draft(other_project.id.clone(), "Editor", database.path());
        store.save_task(&exact).await?;
        store.save_task(&completion).await?;
        let exact_session = SessionRecord::started(exact.id.clone(), "codex", None);
        let completion_session = SessionRecord::started(completion.id.clone(), "codex", None);
        store.save_session(&exact_session).await?;
        store.save_session(&completion_session).await?;
        let mut transcript = agent_harness::Transcript::default();
        transcript.push_user_message("Open the preview browser and show the Éclair.");
        let entry = transcript.entry(0).ok_or("missing prompt")?;
        store
            .save_transcript_entries(&exact_session, &[entry])
            .await?;
        let mut completion_transcript = agent_harness::Transcript::default();
        completion_transcript.push_user_message("We are previewing a file.");
        let completion_entry = completion_transcript.entry(0).ok_or("missing completion")?;
        store
            .save_transcript_entries(&completion_session, &[completion_entry])
            .await?;

        let hits = search.search("preview").await?;
        {
            let mut db = store.db.lock().await;
            let plan = toasty::sql::query(format!("EXPLAIN QUERY PLAN {SEARCH_SQL}"))
                .column_types([Type::I64, Type::I64, Type::I64, Type::String])
                .bind("preview")
                .bind(RESULT_LIMIT)
                .exec(&mut *db)
                .await?;
            assert!(plan.iter().any(|row| matches!(row, Value::Record(fields)
                if matches!(fields.get(3), Some(Value::String(detail)) if detail == "QUERY INDEX METHOD fts"))), "{plan:?}");
        }
        assert_eq!(
            hits.iter()
                .map(|hit| hit.task_id.as_str())
                .collect::<Vec<_>>(),
            [exact.id.as_str(), completion.id.as_str()]
        );
        let [first, second] = hits.as_slice() else {
            return Err("expected two ranked hits".into());
        };
        assert!(first.score > second.score);
        assert_eq!(first.entry_id, entry.id);
        assert_eq!(first.project_id, first_project.id.as_str());
        assert_eq!(first.snippet.matched, "preview");
        assert_eq!(second.snippet.matched, "previewing");
        assert_eq!(search.search("preview ").await?.len(), 1);
        assert_eq!(search.search("preview bro").await?.len(), 1);
        let unicode = search.search("ÉCL").await?;
        assert_eq!(
            unicode.first().map(|hit| hit.snippet.matched.as_str()),
            Some("Éclair")
        );
        assert_eq!(
            search
                .search("Render")
                .await?
                .first()
                .map(|hit| hit.snippet.field),
            Some(SearchField::Task)
        );
        assert_eq!(
            search
                .search("STARL")
                .await?
                .first()
                .map(|hit| hit.snippet.field),
            Some(SearchField::Project)
        );
        assert!(search.search("\"*").await?.is_empty());
        assert!(search.search(&"x".repeat(40)).await?.is_empty());

        let updated = Entry {
            kind: agent_harness::EntryKind::UserMessage {
                text: "Index the replacement.".into(),
            },
            ..entry.clone()
        };
        store
            .save_transcript_entries(&exact_session, &[&updated])
            .await?;
        assert!(search.search("ÉCL").await?.is_empty());
        drop(search);
        drop(store);
        let reopened = Arc::new(TursoStore::open(&uri).await?);
        let search = TaskSearch(reopened.clone());
        assert_eq!(
            search
                .search("replac")
                .await?
                .first()
                .map(|hit| hit.entry_id.as_str()),
            Some(entry.id.as_str())
        );
        reopened.delete_task(&exact.id).await?;
        assert!(search.search("replac").await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "measures the local Turso FTS search budget with 50,000 entries"]
    async fn turso_search_meets_the_50k_entry_latency_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::time::{Duration, Instant};

        let database = TestDatabase::new()?;
        let store = Arc::new(TursoStore::open(&database.uri()).await?);
        for project_number in 0..4 {
            let project = Project::new(database.path().join(format!("Workspace {project_number}")));
            store.save_project(&project).await?;
            for task_number in 0..5 {
                let task = Task::draft(
                    project.id.clone(),
                    &format!("Task {task_number}"),
                    database.path(),
                );
                store.save_task(&task).await?;
                let session = SessionRecord::started(task.id.clone(), "codex", None);
                store.save_session(&session).await?;
                let mut db = store.db.lock().await;
                toasty::sql::statement(
                    "WITH digits(digit) AS (VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9)),
                     numbers(sequence) AS (
                         SELECT ones.digit + 10 * tens.digit + 100 * hundreds.digit + 1000 * thousands.digit
                         FROM digits AS ones CROSS JOIN digits AS tens CROSS JOIN digits AS hundreds
                             CROSS JOIN digits AS thousands WHERE thousands.digit < 3
                     ), documents(sequence, content) AS (
                         SELECT sequence, CASE sequence % 5
                                 WHEN 0 THEN 'Preview rendering diagnostics '
                                 WHEN 1 THEN 'Terminal completion and reconnect '
                                 WHEN 2 THEN 'Browser preview annotation fixture '
                                 WHEN 3 THEN 'Review outstanding workspace files '
                                 ELSE 'Éclair build artifact and configuration '
                             END || sequence FROM numbers WHERE sequence < 2500
                     )
                     INSERT INTO entries (session_id, task_id, id, sequence, kind, payload, created_at,
                                          entry_text, task_title, project_name)
                     SELECT ?1, ?2, 'entry-' || sequence, sequence, 'userMessage',
                         json_object('id', 'entry-' || sequence, 'sequence', sequence, 'kind',
                             json_object('type', 'userMessage', 'data', json_object('text', content))),
                         0, content, ?3, ?4 FROM documents",
                )
                .bind(&session.id)
                .bind(task.id.as_str())
                .bind(&task.title)
                .bind(&project.name)
                .exec(&mut *db)
                .await?;
            }
        }
        {
            let mut db = store.db.lock().await;
            let rows = toasty::sql::query("SELECT COUNT(*) FROM entries")
                .column_types([Type::I64])
                .exec(&mut *db)
                .await?;
            assert!(matches!(rows.first(), Some(Value::Record(fields))
                if matches!(fields.first(), Some(Value::I64(50_000)))));
        }
        let search = TaskSearch(store);
        let mut elapsed = Vec::new();
        for query in ["preview", "pre", "browser ann", "workspace", "écl"]
            .into_iter()
            .cycle()
            .take(50)
        {
            let start = Instant::now();
            let hits = search.search(query).await?;
            elapsed.push(start.elapsed());
            assert_eq!(hits.len(), RESULT_LIMIT as usize);
        }
        elapsed.sort_unstable();
        let p95 = elapsed
            .get((elapsed.len() * 95).div_ceil(100) - 1)
            .ok_or("missing timings")?;
        eprintln!("50,000 entries, 50 queries, p95 {p95:?}");
        assert!(*p95 < Duration::from_millis(250), "FTS p95 was {p95:?}");
        Ok(())
    }

    #[tokio::test]
    async fn turso_restores_attachment_order_and_removes_metadata_with_its_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let project = Project::new(database.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Attachment test", database.path());
        let session = SessionRecord::started(task.id.clone(), "codex", None);
        let text = AttachmentMetadata {
            id: "text".into(),
            relative_path: PathBuf::from("file-text/notes.txt"),
            mime: "text/plain".into(),
            bytes: 50,
        };
        let image = AttachmentMetadata {
            id: "image".into(),
            relative_path: PathBuf::from("file-image/image.png"),
            mime: "image/png".into(),
            bytes: 300,
        };
        let mut transcript = agent_harness::Transcript::default();
        transcript.push_user_message_with_attachments(
            "Review the files.",
            vec![text.clone(), image, text.clone()],
        );
        transcript.push_user_message_with_attachments("Review the text again.", vec![text]);
        let first = transcript.entry(0).ok_or("missing prompt")?;
        let second = transcript.entry(1).ok_or("missing second prompt")?;

        let store = TursoStore::open(&uri).await?;
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        store.save_session(&session).await?;
        store
            .save_transcript_entries(&session, &[first, second])
            .await?;
        store.save_transcript_entries(&session, &[first]).await?;
        let mut invalid = second.clone();
        invalid
            .attachments
            .first_mut()
            .ok_or("missing attachment")?
            .relative_path = "../outside".into();
        if let EntryKind::UserMessage { text } = &mut invalid.kind {
            *text = "This failed update must roll back.".into();
        }
        assert!(
            store
                .save_transcript_entries(&session, &[&invalid])
                .await
                .is_err()
        );
        drop(store);

        let reopened = TursoStore::open(&uri).await?;
        assert_eq!(
            reopened.load_transcript(&task.id).await?,
            transcript.entries()
        );
        reopened.delete_task(&task.id).await?;
        let mut db = reopened.db.lock().await;
        let rows = toasty::sql::query("SELECT id FROM attachments WHERE task_id = ?1")
            .column_types([Type::String])
            .bind(task.id.as_str())
            .exec(&mut *db)
            .await?;
        assert!(rows.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn turso_schema_migrates_and_persists_across_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut project = Project::new(database.path().to_path_buf());
        let mut task = Task::draft(
            project.id.clone(),
            "Persist across restart",
            database.path(),
        );
        task.agent = Agent::OpenCode;
        project.last_task_id = Some(task.id.clone());

        {
            let store = TursoStore::open(&uri).await?;
            let tables = table_names(&store).await?;
            for expected in ["projects", "tasks", "sessions", "entries", "attachments"] {
                assert!(tables.iter().any(|table| table == expected));
            }
            assert!(tables.iter().any(|table| table == "_migrations_history"));
            store.save_project(&project).await?;
            store.save_task(&task).await?;
        }

        let reopened = TursoStore::open(&uri).await?;
        let projects = reopened.load_projects().await?;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);
        assert_eq!(projects[0].path, project.path);
        assert_eq!(projects[0].last_task_id, Some(task.id.clone()));

        let tasks = reopened.load_tasks().await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
        assert_eq!(tasks[0].project_id, task.project_id);
        assert_eq!(tasks[0].state, TaskState::Draft);
        assert_eq!(tasks[0].agent, Agent::OpenCode);
        assert_eq!(tasks[0].tier, AgentTier::Native);
        assert_eq!(tasks[0].proposed_branch, task.proposed_branch);
        assert!(tasks[0].worktree_path.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unreadable_terminal_layouts_do_not_block_valid_projects()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::harness::UnavailableHarnessFactory;
        use crate::terminal::InspectorTab;

        let directory = TestDirectory::new()?;
        let database = directory.join("shepherd.db");
        let unreadable = Project::new(directory.join("unreadable"));
        let valid = Project::new(directory.join("valid"));
        let saved = TerminalLayout {
            inspector_tab: InspectorTab::Review,
            ..TerminalLayout::default()
        };
        {
            let store = TursoStore::open_at(&database).await?;
            store.save_project(&unreadable).await?;
            store.save_project(&valid).await?;
            store.save_terminal_layout(&valid.id, &saved).await?;
            let mut db = store.db.lock().await;
            toasty::sql::statement(
                "INSERT INTO terminal_layouts (project_id, layout_json, updated_at) VALUES (?1, ?2, 0)",
            )
            .bind(unreadable.id.as_str())
            .bind("{")
            .exec(&mut *db)
            .await?;
        }

        let store = TursoStore::open_at(&database).await?;
        let core = crate::Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(core.project_list().projects.len(), 2);
        assert_eq!(
            core.terminal_layout(&unreadable.id)?,
            TerminalLayout::default()
        );
        assert_eq!(core.terminal_layout(&valid.id)?, saved);
        Ok(())
    }

    #[tokio::test]
    async fn a_database_path_containing_a_space_opens() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let spaced = directory.join("Application Support");
        std::fs::create_dir_all(&spaced)?;

        let store = TursoStore::open_at(&spaced.join("shepherd.db")).await?;
        let project = Project::new(spaced.clone());
        store.save_project(&project).await?;

        assert_eq!(store.load_projects().await?.len(), 1);
        assert!(spaced.join("shepherd.db").is_file());
        Ok(())
    }

    #[tokio::test]
    async fn an_existing_database_gains_the_workspace_migration()
    -> Result<(), Box<dyn std::error::Error>> {
        let driver = Turso::new("turso::memory:")?.experimental_index_method(true);
        let mut db = toasty::Db::builder().build(driver).await?;
        let first = &MIGRATIONS[0];
        let expected = checksum(first.sql);

        toasty::sql::statement(
            "CREATE TABLE _migrations_history (\
                name TEXT NOT NULL PRIMARY KEY, \
                checksum TEXT NOT NULL\
            )",
        )
        .exec(&mut db)
        .await?;
        for statement in split_sql(first.sql) {
            toasty::sql::statement(statement).exec(&mut db).await?;
        }
        toasty::sql::statement("INSERT INTO projects (id, path, name, created_at) VALUES ('project', '/legacy', 'Legacy', 1)")
            .exec(&mut db)
            .await?;
        toasty::sql::statement("INSERT INTO tasks (id, project_id, title, state, tier, proposed_worktree_path, proposed_branch, created_at, updated_at) VALUES ('task', 'project', 'Legacy task', 'draft', 'native', '/legacy/worktree', 'shepherd/legacy', 1, 1)")
            .exec(&mut db)
            .await?;
        let mut transcript = agent_harness::Transcript::default();
        transcript.push_user_message("Legacy prompt");
        let entry = transcript.entry(0).ok_or("missing legacy entry")?;
        toasty::sql::statement(
            "INSERT INTO sessions (id, task_id, adapter, state, started_at) \
             VALUES ('session', 'task', 'codex', 'ended', 1)",
        )
        .exec(&mut db)
        .await?;
        toasty::sql::statement(
            "INSERT INTO entries (session_id, id, sequence, kind, payload, created_at) \
             VALUES ('session', ?1, 0, 'userMessage', ?2, 1)",
        )
        .bind(&entry.id)
        .bind(serde_json::to_string(entry)?)
        .exec(&mut db)
        .await?;
        toasty::sql::statement("INSERT INTO _migrations_history (name, checksum) VALUES (?1, ?2)")
            .bind(first.name)
            .bind(&expected)
            .exec(&mut db)
            .await?;

        apply_migrations(&mut db).await?;

        let history = toasty::sql::query("SELECT name FROM _migrations_history ORDER BY name")
            .column_types([Type::String])
            .exec(&mut db)
            .await?;
        assert_eq!(
            history.len(),
            MIGRATIONS.len(),
            "every pending migration should be recorded"
        );

        toasty::sql::query("SELECT id FROM terminals")
            .column_types([Type::String])
            .exec(&mut db)
            .await?;
        let store = TursoStore { db: Mutex::new(db) };
        let projects = store.load_projects().await?;
        assert!(
            projects
                .first()
                .is_some_and(|project| project.preview_url.is_none())
        );
        let tasks = store.load_tasks().await?;
        assert_eq!(tasks.first().map(|task| task.agent), Some(Agent::Codex));
        assert_eq!(
            tasks.first().map(|task| task.worktree_last_used_at),
            Some(1)
        );
        assert_eq!(
            store.load_transcript(&TaskId::new("task")).await?,
            transcript.entries()
        );
        let hits = TaskSearch(Arc::new(store)).search("legacy pro").await?;
        assert_eq!(
            hits.first().map(|hit| hit.entry_id.as_str()),
            Some(entry.id.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_workspace_migration_creates_its_tables() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = TursoStore::open("turso::memory:").await?;
        let tables = table_names(&store).await?;

        for expected in ["terminals", "terminal_layouts", "layouts"] {
            assert!(
                tables.iter().any(|name| name == expected),
                "the workspace migration should create {expected}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn the_memory_store_round_trips_projects_and_tasks()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let store = MemoryStore::new();
        let project = Project::new(directory.path().to_path_buf());
        store.save_project(&project).await?;

        let projects = store.load_projects().await?;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);

        let task = Task::draft(
            ProjectId::new("project-1"),
            "Boundary",
            Path::new("/tmp/project"),
        );
        store.save_task(&task).await?;
        let tasks = store.load_tasks().await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
        assert_eq!(tasks[0].state, TaskState::Draft);
        assert_eq!(TaskId::new(tasks[0].id.as_str()), task.id);
        Ok(())
    }
}

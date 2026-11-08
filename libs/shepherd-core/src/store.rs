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
    #[error(
        "unknown database migration {name}; this database may require a newer Shepherd version"
    )]
    UnknownMigration { name: String },
    #[error("database migration history is incomplete: {missing} is missing before {applied}")]
    MigrationHistoryGap { missing: String, applied: String },
    #[error("invalid migration {name}: {reason}")]
    InvalidMigration { name: String, reason: String },
    #[error("migration {name} failed at statement {statement}: {reason}")]
    MigrationFailed {
        name: String,
        statement: usize,
        reason: String,
    },
    #[error("could not back up the database to {path}: {reason}")]
    Backup { path: String, reason: String },
    #[error("invalid database row: {0}")]
    InvalidRow(String),
    #[error("task {task_id} is no longer awaiting review in the database")]
    ReviewStateChanged { task_id: TaskId },
    #[error("session {session_id} does not belong to task {task_id} in the store")]
    TranscriptOwnerMissing { task_id: TaskId, session_id: String },
    #[error("session {session_id} cannot be saved for task {task_id}")]
    SessionOwnerMissing { task_id: TaskId, session_id: String },
    #[error("terminal {terminal_id} cannot be saved for its project and task")]
    TerminalOwnerMissing { terminal_id: String },
    #[error("terminal layout cannot be saved for missing project {project_id}")]
    TerminalLayoutOwnerMissing { project_id: ProjectId },
    #[error("approval cannot be saved for missing project {project_id}")]
    ApprovalOwnerMissing { project_id: ProjectId },
    #[error("workspace layout cannot be saved for missing task {task_id}")]
    WorkspaceOwnerMissing { task_id: TaskId },
    #[error("task {task_id} cannot be saved for project {project_id}")]
    TaskOwnerMissing {
        task_id: TaskId,
        project_id: ProjectId,
    },
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeDeletion {
    pub task_id: TaskId,
    pub repository_root: PathBuf,
    pub original_path: PathBuf,
    pub staged_path: PathBuf,
    pub worktree_git_dir: PathBuf,
    pub identity: String,
}

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must-use futures"
)]
#[async_trait]
pub trait Store: Send + Sync {
    async fn save_worktree_deletion(&self, deletion: &WorktreeDeletion) -> Result<(), StoreError>;
    async fn load_worktree_deletions(&self) -> Result<Vec<WorktreeDeletion>, StoreError>;
    async fn remove_worktree_deletion(&self, task_id: &TaskId) -> Result<(), StoreError>;
    async fn save_process(&self, process: &ProcessRecord) -> Result<(), StoreError>;
    async fn remove_process(&self, id: &str) -> Result<(), StoreError>;
    async fn load_processes(&self) -> Result<Vec<ProcessRecord>, StoreError>;
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError>;
    async fn save_project(&self, project: &Project) -> Result<(), StoreError>;
    async fn save_draft(&self, project: &Project, task: &Task) -> Result<(), StoreError>;
    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError>;
    async fn save_task(&self, task: &Task) -> Result<(), StoreError>;
    async fn save_task_with_transcript(
        &self,
        task: &Task,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError>;
    async fn archive_tasks(&self, tasks: &[Task]) -> Result<(), StoreError>;
    /// Remove the task and its durable state with the last-task reference and any changed terminal layout.
    async fn delete_task(
        &self,
        task_id: &TaskId,
        layout: Option<(&ProjectId, &TerminalLayout)>,
    ) -> Result<(), StoreError>;
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
    Migration {
        name: "0014_worktree_collection.sql",
        sql: include_str!("../toasty/migrations/0014_worktree_collection.sql"),
    },
    Migration {
        name: "0015_worktree_deletions.sql",
        sql: include_str!("../toasty/migrations/0015_worktree_deletions.sql"),
    },
    Migration {
        name: "0016_worktree_deletion_identity.sql",
        sql: include_str!("../toasty/migrations/0016_worktree_deletion_identity.sql"),
    },
];

/// The durable store used by the application. The database handle is guarded
/// because Toasty's raw SQL API needs exclusive access for each statement.
pub struct TursoStore {
    db: Mutex<toasty::db::Db>,
}

impl TursoStore {
    pub async fn open(db_uri: &str) -> Result<Self, StoreError> {
        let url =
            url::Url::parse(db_uri).map_err(|error| StoreError::Backend(error.to_string()))?;
        let path = (url.path() != ":memory:").then(|| PathBuf::from(url.path()));
        Self::open_driver(Turso::new(db_uri)?, path.as_deref()).await
    }

    /// Open (or create) the database at a filesystem path.
    ///
    /// This bypasses URI parsing on purpose: the driver parses `turso:<value>`
    /// as a URL and reads back the encoded path, so a real data directory such
    /// as the macOS `Application Support` one resolves to a literal
    /// `Application%20Support` file that does not exist.
    pub async fn open_at(path: &Path) -> Result<Self, StoreError> {
        Self::open_driver(Turso::file(path), Some(path)).await
    }

    async fn open_driver(driver: Turso, path: Option<&Path>) -> Result<Self, StoreError> {
        let _migration_lock = if let Some(path) = path {
            Some(acquire_database_migration_lock(path).await?)
        } else {
            None
        };
        let existed = match path {
            Some(path) => path.try_exists().map_err(|error| {
                StoreError::Backend(format!(
                    "could not inspect database path {}: {error}",
                    path.display()
                ))
            })?,
            None => false,
        };
        let mut db = toasty::Db::builder()
            .build(driver.experimental_index_method(true))
            .await?;
        if let Some(path) = path.filter(|_| existed)
            && migrations_pending(&mut db).await?
        {
            backup_database(&mut db, path).await?;
        }
        apply_migrations(&mut db).await?;
        Ok(Self { db: Mutex::new(db) })
    }
}

async fn acquire_database_migration_lock(path: &Path) -> Result<std::fs::File, StoreError> {
    let path = path.to_path_buf();
    let display = path.display().to_string();
    let (file, lock_path) =
        tokio::task::spawn_blocking(move || open_database_migration_lock(&path))
            .await
            .map_err(|error| {
                StoreError::Backend(format!("migration lock worker failed: {error}"))
            })??;
    wait_for_database_migration_lock(file, lock_path, &display).await
}

async fn wait_for_database_migration_lock(
    file: std::fs::File,
    lock_path: PathBuf,
    display: &str,
) -> Result<std::fs::File, StoreError> {
    loop {
        match file.try_lock() {
            Ok(()) => {
                verify_database_migration_lock(&file, &lock_path)?;
                return Ok(file);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(StoreError::Backend(format!(
                    "could not acquire database migration lock for {display}: {error}"
                )));
            }
        }
    }
}

#[cfg(test)]
fn lock_database_migrations(path: &Path) -> Result<std::fs::File, StoreError> {
    let (file, lock_path) = open_database_migration_lock(path)?;
    file.lock().map_err(|error| {
        StoreError::Backend(format!(
            "could not acquire database migration lock for {}: {error}",
            path.display()
        ))
    })?;
    verify_database_migration_lock(&file, &lock_path)?;
    Ok(file)
}

#[cfg(unix)]
fn verify_database_migration_lock(file: &std::fs::File, path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata().map_err(|error| {
        StoreError::Backend(format!(
            "could not inspect acquired migration lock {}: {error}",
            path.display()
        ))
    })?;
    let current = std::fs::symlink_metadata(path).map_err(|error| {
        StoreError::Backend(format!(
            "could not inspect migration lock path {}: {error}",
            path.display()
        ))
    })?;
    if !current.is_file() || current.dev() != opened.dev() || current.ino() != opened.ino() {
        return Err(StoreError::Backend(format!(
            "database migration lock changed while acquiring it: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_database_migration_lock(_file: &std::fs::File, _path: &Path) -> Result<(), StoreError> {
    Ok(())
}

fn open_database_migration_lock(path: &Path) -> Result<(std::fs::File, PathBuf), StoreError> {
    let canonical = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(StoreError::Backend(format!(
                        "database path is a dangling symbolic link: {}",
                        path.display()
                    )));
                }
                Ok(_) => {
                    return Err(StoreError::Backend(format!(
                        "database path changed while acquiring its migration lock: {}",
                        path.display()
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(StoreError::Backend(format!(
                        "could not inspect database path {}: {error}",
                        path.display()
                    )));
                }
            }
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .ok_or_else(|| StoreError::Backend("database path has no filename".into()))?;
            std::fs::canonicalize(parent)
                .map_err(|error| {
                    StoreError::Backend(format!(
                        "could not inspect database directory {}: {error}",
                        parent.display()
                    ))
                })?
                .join(name)
        }
        Err(error) => {
            return Err(StoreError::Backend(format!(
                "could not inspect database path {}: {error}",
                path.display()
            )));
        }
    };
    let mut lock_path = canonical.into_os_string();
    lock_path.push(".migration.lock");
    let lock_path = PathBuf::from(lock_path);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        );
    }
    let file = options.open(&lock_path).map_err(|error| {
        StoreError::Backend(format!(
            "could not open database migration lock {}: {error}",
            lock_path.display()
        ))
    })?;
    if !file
        .metadata()
        .map_err(|error| {
            StoreError::Backend(format!(
                "could not inspect database migration lock {}: {error}",
                lock_path.display()
            ))
        })?
        .is_file()
    {
        return Err(StoreError::Backend(format!(
            "database migration lock is not a regular file: {}",
            lock_path.display()
        )));
    }
    Ok((file, lock_path))
}

async fn migrations_pending(db: &mut toasty::db::Db) -> Result<bool, StoreError> {
    let exists = toasty::sql::query(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = '_migrations_history'",
    )
    .column_types([Type::String])
    .exec(&mut *db)
    .await?;
    if exists.is_empty() {
        return Ok(true);
    }
    let rows = toasty::sql::query("SELECT name, checksum FROM _migrations_history")
        .column_types([Type::String, Type::String])
        .exec(&mut *db)
        .await?;
    let mut applied = std::collections::BTreeSet::new();
    for row in rows {
        let Value::Record(record) = row else {
            return Err(StoreError::InvalidRow(
                "expected a migration history record".into(),
            ));
        };
        let name = required_string(record.first())?;
        let actual = required_string(record.get(1))?;
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.name == name)
            .ok_or_else(|| StoreError::UnknownMigration { name: name.clone() })?;
        let expected = checksum(migration.sql);
        if actual != expected {
            return Err(StoreError::MigrationChecksum {
                name,
                expected,
                actual,
            });
        }
        applied.insert(migration.name);
    }
    let mut first_missing: Option<&str> = None;
    for migration in MIGRATIONS {
        if applied.contains(migration.name) {
            if let Some(missing) = first_missing {
                return Err(StoreError::MigrationHistoryGap {
                    missing: missing.to_string(),
                    applied: migration.name.to_string(),
                });
            }
        } else if first_missing.is_none() {
            first_missing = Some(migration.name);
        }
    }
    Ok(first_missing.is_some())
}

async fn backup_database(db: &mut toasty::db::Db, path: &Path) -> Result<(), StoreError> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".pre-migration-{}", uuid::Uuid::new_v4()));
    let final_directory = PathBuf::from(&name);
    name.push(".partial");
    let directory = PathBuf::from(name);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&directory)
        .map_err(|error| StoreError::Backup {
            path: directory.display().to_string(),
            reason: error.to_string(),
        })?;
    let backup = directory.join("snapshot.db");
    let display = backup.display().to_string();
    let destination = backup.to_str().ok_or_else(|| StoreError::Backup {
        path: display.clone(),
        reason: "the backup path is not UTF-8".into(),
    })?;
    let statement = format!("VACUUM INTO \"{}\"", destination.replace('"', "\"\""));
    toasty::sql::statement(&statement)
        .exec(&mut *db)
        .await
        .map_err(|error| StoreError::Backup {
            path: display,
            reason: error.to_string(),
        })?;
    std::fs::rename(&directory, &final_directory).map_err(|error| StoreError::Backup {
        path: final_directory.display().to_string(),
        reason: error.to_string(),
    })?;
    Ok(())
}

async fn write_transcript_entries(
    executor: &mut dyn toasty::db::Executor,
    session: &SessionRecord,
    entries: &[&Entry],
) -> Result<(), StoreError> {
    let owners = toasty::sql::query(
            "SELECT sessions.id, tasks.title, projects.name FROM sessions JOIN tasks ON tasks.id = sessions.task_id \
             JOIN projects ON projects.id = tasks.project_id WHERE sessions.id = ?1 AND tasks.id = ?2",
        )
        .column_types([Type::String, Type::String, Type::String])
        .bind(&session.id)
        .bind(session.task_id.as_str())
        .exec(&mut *executor)
        .await?;
    let Some(Value::Record(owner)) = owners.first() else {
        return Err(StoreError::TranscriptOwnerMissing {
            task_id: session.task_id.clone(),
            session_id: session.id.clone(),
        });
    };
    let task_title = required_string(owner.get(1))?;
    let project_name = required_string(owner.get(2))?;
    let created_at = crate::projects::now_millis() as i64;
    let mut remaining = entries.iter();
    let mut pending = None;
    loop {
        let mut batch = Vec::new();
        let mut batch_bytes = 0_usize;
        while batch.len() < 32 {
            let entry = match pending.take().or_else(|| remaining.next().copied()) {
                Some(entry) => entry,
                None => break,
            };
            let mut payload = serde_json::to_value(entry).map_err(|error| {
                StoreError::InvalidRow(format!("could not encode transcript entry: {error}"))
            })?;
            if let Some(fields) = payload.as_object_mut() {
                fields.remove("attachments");
            }
            let kind = payload
                .pointer("/kind/type")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| StoreError::InvalidRow("missing transcript entry kind".into()))?
                .to_owned();
            let payload = payload.to_string();
            let text = entry_text(&entry.kind);
            let entry_bytes = payload
                .len()
                .saturating_add(text.len())
                .saturating_add(kind.len())
                .saturating_add(entry.id.len())
                .saturating_add(session.id.len())
                .saturating_add(session.task_id.as_str().len())
                .saturating_add(task_title.len())
                .saturating_add(project_name.len());
            if !batch.is_empty() && batch_bytes.saturating_add(entry_bytes) > 1024 * 1024 {
                pending = Some(entry);
                break;
            }
            batch_bytes = batch_bytes.saturating_add(entry_bytes);
            batch.push((entry, kind, payload, text));
        }
        if batch.is_empty() {
            break;
        }
        let rows = (0..batch.len())
            .map(|index| {
                let first = index * 10 + 1;
                format!(
                    "({})",
                    (first..first + 10)
                        .map(|parameter| format!("?{parameter}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO entries (session_id, task_id, id, sequence, kind, payload, created_at, entry_text, task_title, project_name) VALUES {rows} \
             ON CONFLICT(task_id, id) DO UPDATE SET kind = excluded.kind, payload = excluded.payload, \
             entry_text = excluded.entry_text, task_title = excluded.task_title, project_name = excluded.project_name"
        );
        let mut statement = toasty::sql::statement(sql);
        for (entry, kind, payload, text) in batch {
            statement = statement
                .bind(&session.id)
                .bind(session.task_id.as_str())
                .bind(&entry.id)
                .bind(entry.sequence as i64)
                .bind(kind)
                .bind(payload)
                .bind(created_at)
                .bind(text)
                .bind(&task_title)
                .bind(&project_name);
        }
        statement.exec(&mut *executor).await?;
    }
    for entry in entries {
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
                    .exec(&mut *executor)
                    .await?;
            }
            toasty::sql::statement(
                "DELETE FROM attachments WHERE task_id = ?1 AND entry_id = ?2 AND position >= ?3",
            )
            .bind(session.task_id.as_str())
            .bind(&entry.id)
            .bind(entry.attachments.len() as i64)
            .exec(&mut *executor)
            .await?;
        }
    }
    Ok(())
}

async fn write_task(
    executor: &mut dyn toasty::db::Executor,
    task: &Task,
) -> Result<(), StoreError> {
    let worktree_path = optional_path(task.worktree_path.as_ref());
    let branch = task.branch.as_deref().unwrap_or_default();
    let base_branch = task.base_branch.as_deref().unwrap_or_default();
    let error = task.error.as_deref().unwrap_or_default();
    let updated = toasty::sql::statement(
            "INSERT INTO tasks (\
                id, project_id, title, state, tier, proposed_worktree_path, \
                proposed_branch, worktree_path, branch, base_branch, error, \
                created_at, updated_at, branch_tip_sha, agent, worktree_last_used_at, collection_pending\
             ) SELECT \
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, \
                NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), ?12, ?13, NULLIF(?14, ''), ?15, ?16, ?17\
             FROM projects WHERE id = ?2 \
             ON CONFLICT (id) DO UPDATE SET \
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
                worktree_last_used_at = excluded.worktree_last_used_at, \
                collection_pending = excluded.collection_pending \
             WHERE tasks.project_id = excluded.project_id",
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
        .bind(i64::from(task.collection_pending))
        .exec(&mut *executor)
        .await?;
    if updated != 1 {
        return Err(StoreError::TaskOwnerMissing {
            task_id: task.id.clone(),
            project_id: task.project_id.clone(),
        });
    }
    Ok(())
}

async fn write_project(
    executor: &mut dyn toasty::db::Executor,
    project: &Project,
) -> Result<(), StoreError> {
    let last_task_id = project
        .last_task_id
        .as_ref()
        .map(|task_id| task_id.as_str())
        .unwrap_or_default();
    toasty::sql::statement(
        "INSERT INTO projects (id, path, name, missing, last_task_id, created_at, preview_url) \
         VALUES (?1, ?2, ?3, ?4, NULLIF(?5, ''), ?6, NULLIF(?7, '')) \
         ON CONFLICT (id) DO UPDATE SET \
            path = excluded.path, name = excluded.name, missing = excluded.missing, \
            last_task_id = excluded.last_task_id, created_at = excluded.created_at, preview_url = excluded.preview_url",
    ).bind(project.id.as_str()).bind(project.path.to_string_lossy().as_ref())
        .bind(&project.name).bind(i64::from(project.missing)).bind(last_task_id)
        .bind(project.created_at as i64).bind(project.preview_url.as_ref().map(url::Url::as_str).unwrap_or_default())
        .exec(executor).await?;
    Ok(())
}

fn validate_draft_owner(project: &Project, task: &Task) -> Result<(), StoreError> {
    if task.project_id != project.id
        || project.last_task_id.as_ref() != Some(&task.id)
        || task.state != TaskState::Draft
    {
        return Err(StoreError::InvalidRow(
            "draft and selected project do not match".into(),
        ));
    }
    Ok(())
}

async fn write_terminal_layout(
    executor: &mut dyn toasty::db::Executor,
    project_id: &ProjectId,
    layout: &TerminalLayout,
) -> Result<(), StoreError> {
    let json =
        serde_json::to_string(layout).map_err(|error| StoreError::Backend(error.to_string()))?;
    let updated = toasty::sql::statement(
        "INSERT INTO terminal_layouts (project_id, layout_json, updated_at) \
         SELECT id, ?2, ?3 FROM projects WHERE id = ?1 \
         ON CONFLICT (project_id) DO UPDATE SET layout_json = excluded.layout_json, updated_at = excluded.updated_at",
    ).bind(project_id.as_str()).bind(json).bind(crate::projects::now_millis() as i64).exec(executor).await?;
    if updated != 1 {
        return Err(StoreError::TerminalLayoutOwnerMissing {
            project_id: project_id.clone(),
        });
    }
    Ok(())
}

#[async_trait]
impl Store for TursoStore {
    async fn save_worktree_deletion(&self, deletion: &WorktreeDeletion) -> Result<(), StoreError> {
        let path = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| StoreError::Backend("worktree deletion paths must be UTF-8".into()))
        };
        let repository = path(&deletion.repository_root)?;
        let original = path(&deletion.original_path)?;
        let staged = path(&deletion.staged_path)?;
        let git_dir = path(&deletion.worktree_git_dir)?;
        let mut db = self.db.lock().await;
        toasty::sql::statement("INSERT INTO worktree_deletions (task_id, repository_root, original_path, staged_path, worktree_git_dir, identity) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
            .bind(deletion.task_id.as_str()).bind(repository).bind(original).bind(staged).bind(git_dir).bind(&deletion.identity).exec(&mut *db).await?;
        Ok(())
    }

    async fn load_worktree_deletions(&self) -> Result<Vec<WorktreeDeletion>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query("SELECT task_id, repository_root, original_path, staged_path, worktree_git_dir, identity FROM worktree_deletions ORDER BY task_id")
            .column_types([Type::String, Type::String, Type::String, Type::String, Type::String, Type::String]).exec(&mut *db).await?;
        rows.into_iter()
            .map(|row| {
                let Value::Record(fields) = row else {
                    return Err(StoreError::InvalidRow(
                        "expected a worktree deletion record".into(),
                    ));
                };
                Ok(WorktreeDeletion {
                    task_id: TaskId::new(required_string(fields.first())?),
                    repository_root: PathBuf::from(required_string(fields.get(1))?),
                    original_path: PathBuf::from(required_string(fields.get(2))?),
                    staged_path: PathBuf::from(required_string(fields.get(3))?),
                    worktree_git_dir: PathBuf::from(required_string(fields.get(4))?),
                    identity: required_string(fields.get(5))?,
                })
            })
            .collect()
    }

    async fn remove_worktree_deletion(&self, task_id: &TaskId) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        toasty::sql::statement("DELETE FROM worktree_deletions WHERE task_id = ?1")
            .bind(task_id.as_str())
            .exec(&mut *db)
            .await?;
        Ok(())
    }

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
        if let Err(error) = write_transcript_entries(&mut transaction, session, entries).await {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(StoreError::Backend(format!(
                    "transcript save failed: {error}; rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn save_task_with_transcript(
        &self,
        task: &Task,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError> {
        if task.id != session.task_id {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: task.id.clone(),
                session_id: session.id.clone(),
            });
        }
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        let result = async {
            if entries.is_empty() {
                write_transcript_entries(&mut transaction, session, entries).await?;
            }
            write_task(&mut transaction, task).await?;
            if !entries.is_empty() {
                write_transcript_entries(&mut transaction, session, entries).await?;
            }
            Ok::<(), StoreError>(())
        }
        .await;
        if let Err(error) = result {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(StoreError::Backend(format!(
                    "task and transcript save failed: {error}; rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
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
        let updated = toasty::sql::statement(
            "INSERT INTO project_approvals (project_id, id, grant_json) \
             SELECT id, ?2, ?3 FROM projects WHERE id = ?1 \
             ON CONFLICT (project_id, id) DO NOTHING",
        )
        .bind(project.as_str())
        .bind(&grant.id)
        .bind(&json)
        .exec(&mut *db)
        .await?;
        if updated == 0 {
            let existing = toasty::sql::query(
                "SELECT project_approvals.id FROM project_approvals JOIN projects \
                 ON projects.id = project_approvals.project_id \
                 WHERE project_approvals.project_id = ?1 AND project_approvals.id = ?2",
            )
            .column_types([Type::String])
            .bind(project.as_str())
            .bind(&grant.id)
            .exec(&mut *db)
            .await?;
            if existing.is_empty() {
                return Err(StoreError::ApprovalOwnerMissing {
                    project_id: project.clone(),
                });
            }
        }
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
        write_project(&mut *db, project).await
    }

    async fn save_draft(&self, project: &Project, task: &Task) -> Result<(), StoreError> {
        validate_draft_owner(project, task)?;
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        let result = async {
            write_project(&mut transaction, project).await?;
            write_task(&mut transaction, task).await
        }
        .await;
        if let Err(error) = result {
            if let Err(rollback) = transaction.rollback().await {
                return Err(StoreError::Backend(format!(
                    "draft creation failed: {error}; rollback failed: {rollback}"
                )));
            }
            return Err(error);
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, project_id, title, state, tier, proposed_worktree_path, \
                    proposed_branch, worktree_path, branch, base_branch, error, \
                    created_at, updated_at, branch_tip_sha, agent, worktree_last_used_at, collection_pending \
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
            Type::I64,
        ])
        .exec(&mut *db)
        .await?;
        rows.into_iter().map(parse_task).collect()
    }

    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        write_task(&mut *db, task).await
    }

    async fn archive_tasks(&self, tasks: &[Task]) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        for task in tasks {
            let result = toasty::sql::statement(
                "UPDATE tasks SET state = 'done', updated_at = ?2 WHERE id = ?1 AND state = 'review'",
            )
            .bind(task.id.as_str())
            .bind(task.updated_at as i64)
            .exec(&mut transaction)
            .await;
            let updated = match result {
                Ok(updated) => updated,
                Err(error) => {
                    if let Err(rollback_error) = transaction.rollback().await {
                        return Err(StoreError::Backend(format!(
                            "archive failed: {error}; rollback failed: {rollback_error}"
                        )));
                    }
                    return Err(error.into());
                }
            };
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

    async fn delete_task(
        &self,
        task_id: &TaskId,
        layout: Option<(&ProjectId, &TerminalLayout)>,
    ) -> Result<(), StoreError> {
        let mut db = self.db.lock().await;
        let mut transaction = db.transaction().await?;
        for statement in [
            "UPDATE projects SET last_task_id = NULL WHERE last_task_id = ?1",
            "DELETE FROM attachments WHERE task_id = ?1",
            "DELETE FROM entries WHERE task_id = ?1",
            "DELETE FROM sessions WHERE task_id = ?1",
            "DELETE FROM terminals WHERE task_id = ?1",
            "DELETE FROM layouts WHERE task_id = ?1",
            "DELETE FROM tasks WHERE id = ?1",
        ] {
            if let Err(error) = toasty::sql::statement(statement)
                .bind(task_id.as_str())
                .exec(&mut transaction)
                .await
            {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(StoreError::Backend(format!(
                        "task deletion failed: {error}; rollback failed: {rollback_error}"
                    )));
                }
                return Err(error.into());
            }
        }
        if let Some((project_id, layout)) = layout
            && let Err(error) = write_terminal_layout(&mut transaction, project_id, layout).await
        {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(StoreError::Backend(format!(
                    "task deletion failed: {error}; rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
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
        let updated = toasty::sql::statement(
            "INSERT INTO terminals (\
                id, project_id, task_id, scope, title, cwd, command, \
                created_at, ended_at, exit_code\
             ) SELECT \
                ?1, ?2, NULLIF(?3, ''), ?4, ?5, ?6, ?7, ?8, NULLIF(?9, -1), NULLIF(?10, -1)\
             FROM projects WHERE id = ?2 AND (\
                (?4 = 'project' AND ?3 = '') OR \
                (?4 = 'task' AND EXISTS (SELECT 1 FROM tasks WHERE id = ?3 AND project_id = ?2))\
             ) ON CONFLICT (id) DO UPDATE SET \
                title = excluded.title, \
                cwd = excluded.cwd, \
                command = excluded.command, \
                created_at = excluded.created_at, \
                ended_at = excluded.ended_at, \
                exit_code = excluded.exit_code \
             WHERE terminals.project_id = excluded.project_id \
                AND COALESCE(terminals.task_id, '') = COALESCE(excluded.task_id, '') \
                AND terminals.scope = excluded.scope",
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
        if updated != 1 {
            return Err(StoreError::TerminalOwnerMissing {
                terminal_id: terminal.id.as_str().to_string(),
            });
        }
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
        let updated = toasty::sql::statement(
            "INSERT INTO layouts (task_id, workspace_json, updated_at)
             SELECT ?1, ?2, ?3 FROM tasks WHERE id = ?1
             ON CONFLICT (task_id) DO UPDATE SET workspace_json = excluded.workspace_json, updated_at = excluded.updated_at",
        )
        .bind(task_id.as_str())
        .bind(json)
        .bind(crate::projects::now_millis() as i64)
        .exec(&mut *db)
        .await?;
        if updated != 1 {
            return Err(StoreError::WorkspaceOwnerMissing {
                task_id: task_id.clone(),
            });
        }
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
        let mut db = self.db.lock().await;
        write_terminal_layout(&mut *db, project_id, layout).await
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
        let updated = toasty::sql::statement(
            "INSERT INTO sessions (\
                id, task_id, adapter, state, adapter_session_id, started_at, \
                ended_at, exit_code, stderr, capabilities_json\
             ) SELECT \
                ?1, ?2, ?3, ?4, NULLIF(?5, ''), ?6, \
                NULLIF(?7, -1), NULLIF(?8, -1), NULLIF(?9, ''), NULLIF(?10, '')\
             FROM tasks WHERE id = ?2 \
             ON CONFLICT (id) DO UPDATE SET \
                adapter = excluded.adapter, \
                state = excluded.state, \
                adapter_session_id = excluded.adapter_session_id, \
                started_at = excluded.started_at, \
                ended_at = excluded.ended_at, \
                exit_code = excluded.exit_code, \
                stderr = excluded.stderr, \
                capabilities_json = excluded.capabilities_json \
             WHERE sessions.task_id = excluded.task_id",
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
        if updated != 1 {
            return Err(StoreError::SessionOwnerMissing {
                task_id: session.task_id.clone(),
                session_id: session.id.clone(),
            });
        }
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
    apply_migration_set(db, MIGRATIONS).await
}

async fn apply_migration_set(
    db: &mut toasty::db::Db,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let mut names = std::collections::HashSet::new();
    for migration in migrations {
        if migration.name.trim().is_empty() {
            return Err(StoreError::InvalidMigration {
                name: migration.name.to_owned(),
                reason: "migration name is empty".into(),
            });
        }
        if !names.insert(migration.name) {
            return Err(StoreError::InvalidMigration {
                name: migration.name.to_owned(),
                reason: "migration name is duplicated".into(),
            });
        }
    }
    let statements = migrations
        .iter()
        .map(parse_migration)
        .collect::<Result<Vec<_>, _>>()?;
    toasty::sql::statement(
        "CREATE TABLE IF NOT EXISTS _migrations_history (\
            name TEXT NOT NULL PRIMARY KEY, \
            checksum TEXT NOT NULL\
        )",
    )
    .exec(&mut *db)
    .await?;

    for (migration, statements) in migrations.iter().zip(statements) {
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
        for (index, statement) in statements.iter().enumerate() {
            if let Err(error) = toasty::sql::statement(*statement)
                .exec(&mut transaction)
                .await
            {
                let mut reason = error.to_string();
                if let Err(rollback_error) = transaction.rollback().await {
                    reason.push_str(&format!("; rollback failed: {rollback_error}"));
                }
                return Err(StoreError::MigrationFailed {
                    name: migration.name.to_owned(),
                    statement: index + 1,
                    reason,
                });
            }
        }
        if let Err(error) = toasty::sql::statement(
            "INSERT INTO _migrations_history (name, checksum) VALUES (?1, ?2)",
        )
        .bind(migration.name)
        .bind(&expected)
        .exec(&mut transaction)
        .await
        {
            let mut reason = error.to_string();
            if let Err(rollback_error) = transaction.rollback().await {
                reason.push_str(&format!("; rollback failed: {rollback_error}"));
            }
            return Err(StoreError::MigrationFailed {
                name: migration.name.to_owned(),
                statement: statements.len() + 1,
                reason,
            });
        }
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

fn parse_migration(migration: &Migration) -> Result<Vec<&str>, StoreError> {
    let mut parser = turso_parser::parser::Parser::new(migration.sql.as_bytes());
    let mut statements = Vec::new();
    let mut start = 0;
    loop {
        let command = parser
            .next_cmd()
            .map_err(|error| StoreError::InvalidMigration {
                name: migration.name.to_owned(),
                reason: error.to_string(),
            })?;
        let Some(command) = command else {
            break;
        };
        if matches!(
            command,
            turso_parser::ast::Cmd::Stmt(
                turso_parser::ast::Stmt::Begin { .. }
                    | turso_parser::ast::Stmt::Commit { .. }
                    | turso_parser::ast::Stmt::Rollback { .. }
                    | turso_parser::ast::Stmt::Savepoint { .. }
                    | turso_parser::ast::Stmt::Release { .. }
            )
        ) {
            return Err(StoreError::InvalidMigration {
                name: migration.name.to_owned(),
                reason: "transaction control is managed by the migration runner".into(),
            });
        }
        let end = parser.offset();
        let statement = migration
            .sql
            .get(start..end)
            .ok_or_else(|| StoreError::InvalidMigration {
                name: migration.name.to_owned(),
                reason: "parser returned an invalid statement boundary".into(),
            })?
            .trim();
        statements.push(statement);
        start = end;
    }
    if statements.is_empty() {
        return Err(StoreError::InvalidMigration {
            name: migration.name.to_owned(),
            reason: "migration has no SQL statements".into(),
        });
    }
    Ok(statements)
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
        collection_pending: required_u64(record.get(16))? != 0,
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

fn validate_attachments(entries: &[&Entry]) -> Result<(), StoreError> {
    for entry in entries {
        if matches!(entry.kind, EntryKind::UserMessage { .. }) {
            for attachment in &entry.attachments {
                if attachment.relative_path.to_str().is_none_or(str::is_empty)
                    || !attachment
                        .relative_path
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                {
                    return Err(StoreError::InvalidRow(
                        "expected a relative attachment path".into(),
                    ));
                }
                i64::try_from(attachment.bytes).map_err(|_| {
                    StoreError::InvalidRow("attachment size exceeds database range".into())
                })?;
            }
        }
    }
    Ok(())
}

fn validate_memory_task_owner(
    tasks: &BTreeMap<String, Task>,
    task: &Task,
) -> Result<(), StoreError> {
    if tasks
        .get(task.id.as_str())
        .is_some_and(|stored| stored.project_id != task.project_id)
    {
        return Err(StoreError::TaskOwnerMissing {
            task_id: task.id.clone(),
            project_id: task.project_id.clone(),
        });
    }
    Ok(())
}

/// A non-durable store used to test the boundary before a database is opened.
#[derive(Default)]
pub struct MemoryStore {
    worktree_deletions: RwLock<BTreeMap<TaskId, WorktreeDeletion>>,
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
    async fn save_worktree_deletion(&self, deletion: &WorktreeDeletion) -> Result<(), StoreError> {
        let mut deletions = self.worktree_deletions.write().await;
        if deletions.contains_key(&deletion.task_id) {
            return Err(StoreError::Backend(
                "worktree deletion is already recorded".into(),
            ));
        }
        deletions.insert(deletion.task_id.clone(), deletion.clone());
        Ok(())
    }

    async fn load_worktree_deletions(&self) -> Result<Vec<WorktreeDeletion>, StoreError> {
        Ok(self
            .worktree_deletions
            .read()
            .await
            .values()
            .cloned()
            .collect())
    }

    async fn remove_worktree_deletion(&self, task_id: &TaskId) -> Result<(), StoreError> {
        self.worktree_deletions.write().await.remove(task_id);
        Ok(())
    }

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
        if entries.is_empty() {
            return Ok(());
        }
        validate_attachments(entries)?;
        let tasks = self.tasks.read().await;
        let sessions = self.sessions.read().await;
        let projects = self.projects.read().await;
        if !tasks
            .get(session.task_id.as_str())
            .is_some_and(|task| projects.contains_key(task.project_id.as_str()))
            || !sessions
                .iter()
                .any(|stored| stored.id == session.id && stored.task_id == session.task_id)
        {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: session.task_id.clone(),
                session_id: session.id.clone(),
            });
        }
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
        let projects = self.projects.read().await;
        if !projects.contains_key(project.as_str()) {
            return Err(StoreError::ApprovalOwnerMissing {
                project_id: project.clone(),
            });
        }
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

    async fn save_draft(&self, project: &Project, task: &Task) -> Result<(), StoreError> {
        validate_draft_owner(project, task)?;
        let mut tasks = self.tasks.write().await;
        let mut projects = self.projects.write().await;
        validate_memory_task_owner(&tasks, task)?;
        projects.insert(project.id.as_str().to_string(), project.clone());
        tasks.insert(task.id.as_str().to_string(), task.clone());
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let tasks = self.tasks.read().await;
        Ok(tasks.values().cloned().collect())
    }

    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        let projects = self.projects.read().await;
        if !projects.contains_key(task.project_id.as_str()) {
            return Err(StoreError::TaskOwnerMissing {
                task_id: task.id.clone(),
                project_id: task.project_id.clone(),
            });
        }
        validate_memory_task_owner(&tasks, task)?;
        tasks.insert(task.id.as_str().to_string(), task.clone());
        Ok(())
    }

    async fn save_task_with_transcript(
        &self,
        task: &Task,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError> {
        if task.id != session.task_id {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: task.id.clone(),
                session_id: session.id.clone(),
            });
        }
        validate_attachments(entries)?;
        let mut tasks = self.tasks.write().await;
        let sessions = self.sessions.read().await;
        let projects = self.projects.read().await;
        validate_memory_task_owner(&tasks, task)?;
        if !tasks.contains_key(task.id.as_str())
            || !projects.contains_key(task.project_id.as_str())
            || !sessions
                .iter()
                .any(|stored| stored.id == session.id && stored.task_id == task.id)
        {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: task.id.clone(),
                session_id: session.id.clone(),
            });
        }
        let mut transcripts = self.transcripts.write().await;
        tasks.insert(task.id.as_str().to_string(), task.clone());
        if !entries.is_empty() {
            let transcript = transcripts.entry(task.id.clone()).or_default();
            for entry in entries {
                transcript.insert(entry.sequence, (*entry).clone());
            }
        }
        Ok(())
    }

    async fn archive_tasks(&self, archived: &[Task]) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        let mut seen = std::collections::BTreeSet::new();
        if let Some(task) = archived.iter().find(|task| {
            !seen.insert(task.id.clone())
                || tasks
                    .get(task.id.as_str())
                    .is_none_or(|stored| stored.state != TaskState::Review)
        }) {
            return Err(StoreError::ReviewStateChanged {
                task_id: task.id.clone(),
            });
        }
        for task in archived {
            if let Some(stored) = tasks.get_mut(task.id.as_str()) {
                stored.state = TaskState::Done;
                stored.updated_at = task.updated_at;
            }
        }
        Ok(())
    }

    async fn delete_task(
        &self,
        task_id: &TaskId,
        layout: Option<(&ProjectId, &TerminalLayout)>,
    ) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        let mut sessions = self.sessions.write().await;
        let mut projects = self.projects.write().await;
        if let Some((project_id, _)) = layout
            && !projects.contains_key(project_id.as_str())
        {
            return Err(StoreError::TerminalLayoutOwnerMissing {
                project_id: project_id.clone(),
            });
        }
        let mut transcripts = self.transcripts.write().await;
        let mut workspace_layouts = self.workspace_layouts.write().await;
        let mut terminal_layouts = self.terminal_layouts.write().await;
        let mut terminals = self.terminals.write().await;
        if let Some((project_id, layout)) = layout {
            terminal_layouts.insert(project_id.clone(), layout.clone());
        }
        for project in projects.values_mut() {
            if project.last_task_id.as_ref() == Some(task_id) {
                project.last_task_id = None;
            }
        }
        tasks.remove(task_id.as_str());
        workspace_layouts.remove(task_id);
        transcripts.remove(task_id);
        sessions.retain(|session| &session.task_id != task_id);
        terminals.retain(|_, terminal| terminal.task_id.as_ref() != Some(task_id));
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
        let tasks = self.tasks.read().await;
        if !tasks.contains_key(task_id.as_str()) {
            return Err(StoreError::WorkspaceOwnerMissing {
                task_id: task_id.clone(),
            });
        }
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
        let tasks = self.tasks.read().await;
        let projects = self.projects.read().await;
        let mut terminals = self.terminals.write().await;
        let valid_owner = projects.contains_key(terminal.project_id.as_str())
            && match (terminal.scope, &terminal.task_id) {
                (TerminalScope::Project, None) => true,
                (TerminalScope::Task, Some(id)) => tasks
                    .get(id.as_str())
                    .is_some_and(|task| task.project_id == terminal.project_id),
                _ => false,
            };
        let same_owner = terminals.get(terminal.id.as_str()).is_none_or(|existing| {
            existing.project_id == terminal.project_id
                && existing.task_id == terminal.task_id
                && existing.scope == terminal.scope
        });
        if !valid_owner || !same_owner {
            return Err(StoreError::TerminalOwnerMissing {
                terminal_id: terminal.id.as_str().to_string(),
            });
        }
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
        let projects = self.projects.read().await;
        if !projects.contains_key(project_id.as_str()) {
            return Err(StoreError::TerminalLayoutOwnerMissing {
                project_id: project_id.clone(),
            });
        }
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
        let tasks = self.tasks.read().await;
        let mut sessions = self.sessions.write().await;
        if !tasks.contains_key(session.task_id.as_str())
            || sessions
                .iter()
                .any(|seen| seen.id == session.id && seen.task_id != session.task_id)
        {
            return Err(StoreError::SessionOwnerMissing {
                task_id: session.task_id.clone(),
                session_id: session.id.clone(),
            });
        }
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

    #[tokio::test]
    async fn draft_creation_rolls_back_project_selection_on_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = Project::new(database.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Draft", database.path());
        store.save_project(&project).await?;
        let mut selected = project.clone();
        selected.last_task_id = Some(task.id.clone());
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement("ALTER TABLE tasks RENAME TO tasks_unavailable")
                .exec(&mut *db)
                .await?;
        }
        assert!(store.save_draft(&selected, &task).await.is_err());
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement("ALTER TABLE tasks_unavailable RENAME TO tasks")
                .exec(&mut *db)
                .await?;
        }
        assert_eq!(
            store
                .load_projects()
                .await?
                .first()
                .and_then(|project| project.last_task_id.as_ref()),
            None
        );
        assert!(store.load_tasks().await?.is_empty());
        store.save_draft(&selected, &task).await?;
        assert_eq!(
            store
                .load_projects()
                .await?
                .first()
                .and_then(|project| project.last_task_id.as_ref()),
            Some(&task.id)
        );
        assert_eq!(store.load_tasks().await?.len(), 1);
        let memory = MemoryStore::new();
        memory.save_project(&project).await?;
        assert!(memory.save_draft(&project, &task).await.is_err());
        assert!(memory.load_tasks().await?.is_empty());
        memory.save_draft(&selected, &task).await?;
        assert_eq!(
            memory
                .load_projects()
                .await?
                .first()
                .and_then(|project| project.last_task_id.as_ref()),
            Some(&task.id)
        );
        Ok(())
    }

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
    async fn task_and_transcript_commit_or_roll_back_together()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.to_path_buf());
            let task = Task::draft(project.id.clone(), "Original", root);
            let session = SessionRecord::started(task.id.clone(), "codex", None);
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            store.save_session(&session).await?;
            let mut transcript = agent_harness::Transcript::default();
            transcript.push_user_message("Original prompt");
            let entry = transcript.entry(0).ok_or("missing entry")?;
            store.save_transcript_entries(&session, &[entry]).await?;
            let mut changed_task = task.clone();
            changed_task.title = "Changed".into();
            let mut changed_entry = entry.clone();
            if let EntryKind::UserMessage { text } = &mut changed_entry.kind {
                *text = "Changed prompt".into();
            }
            changed_entry.attachments.push(AttachmentMetadata {
                id: "invalid".into(),
                relative_path: "../outside".into(),
                mime: "text/plain".into(),
                bytes: 1,
            });
            assert!(
                store
                    .save_task_with_transcript(&changed_task, &session, &[&changed_entry])
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .load_tasks()
                    .await?
                    .first()
                    .ok_or("missing task")?
                    .title,
                "Original"
            );
            assert_eq!(store.load_transcript(&task.id).await?, transcript.entries());
            changed_entry.attachments.clear();
            store
                .save_task_with_transcript(&changed_task, &session, &[&changed_entry])
                .await?;
            assert_eq!(
                store
                    .load_tasks()
                    .await?
                    .first()
                    .ok_or("missing task")?
                    .title,
                "Changed"
            );
            assert_eq!(store.load_transcript(&task.id).await?, vec![changed_entry]);
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("shepherd.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn large_transcript_entries_survive_size_limited_batches()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = Project::new(database.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Large entries", database.path());
        let session = SessionRecord::started(task.id.clone(), "codex", None);
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        store.save_session(&session).await?;
        let mut transcript = agent_harness::Transcript::default();
        transcript.push_user_message("a".repeat(300_000));
        transcript.push_user_message("b".repeat(300_000));
        transcript.push_user_message("c".repeat(1_100_000));
        transcript.push_user_message("after the large entry");
        let entries = transcript.entries();
        store
            .save_transcript_entries(&session, &entries.iter().collect::<Vec<_>>())
            .await?;
        assert_eq!(store.load_transcript(&task.id).await?, entries);
        Ok(())
    }

    #[tokio::test]
    async fn project_approval_requires_an_existing_project_and_preserves_grants()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.join("approval-project"));
            let grant = ProjectApproval {
                id: "grant".into(),
                agent: Agent::Codex,
                title: "original".into(),
                arguments: serde_json::json!({"command": "echo"}),
                paths: vec![],
            };
            assert!(matches!(
                store.save_project_approval(&project.id, &grant).await,
                Err(StoreError::ApprovalOwnerMissing { .. })
            ));
            assert!(!store.has_project_approval(&project.id, &grant.id).await?);
            store.save_project(&project).await?;
            store.save_project_approval(&project.id, &grant).await?;
            let mut replacement = grant.clone();
            replacement.title = "replacement".into();
            store
                .save_project_approval(&project.id, &replacement)
                .await?;
            assert_eq!(store.project_approvals(&project.id).await?, vec![grant]);
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("approval-owner.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_database_opens_share_one_pre_migration_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.join("shared.db");
        let mut legacy = toasty::Db::builder()
            .build(Turso::file(&path).experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TABLE legacy_data (value TEXT NOT NULL)")
            .exec(&mut legacy)
            .await?;
        toasty::sql::statement("INSERT INTO legacy_data VALUES ('preserved')")
            .exec(&mut legacy)
            .await?;
        drop(legacy);
        let (first, second) = tokio::join!(TursoStore::open_at(&path), TursoStore::open_at(&path));
        assert!(first?.load_projects().await?.is_empty());
        assert!(second?.load_projects().await?.is_empty());
        let backups = std::fs::read_dir(directory.path())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|path| path.to_string_lossy().contains(".pre-migration-"))
            .collect::<Vec<_>>();
        let [backup] = backups.as_slice() else {
            return Err("expected one pre-migration backup".into());
        };
        let mut old = toasty::Db::builder()
            .build(Turso::file(backup.join("snapshot.db")).experimental_index_method(true))
            .await?;
        let rows = toasty::sql::query("SELECT value FROM legacy_data")
            .column_types([Type::String])
            .exec(&mut old)
            .await?;
        assert!(
            matches!(rows.first(), Some(Value::Record(fields)) if matches!(fields.first(), Some(Value::String(value)) if value.as_str() == "preserved"))
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn migration_lock_refuses_dangling_database_and_lock_symlinks()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let dangling = directory.join("dangling.db");
        std::os::unix::fs::symlink(directory.join("missing.db"), &dangling)?;
        assert!(TursoStore::open_at(&dangling).await.is_err());
        let path = directory.join("database.db");
        let lock = directory.join("database.db.migration.lock");
        let target = directory.join("target");
        std::fs::write(&target, "unchanged")?;
        std::os::unix::fs::symlink(&target, &lock)?;
        assert!(TursoStore::open_at(&path).await.is_err());
        assert_eq!(std::fs::read_to_string(target)?, "unchanged");
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    #[ignore = "subprocess fixture for the migration lock crash test"]
    fn migration_lock_subprocess_fixture() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os("SHEPHERD_TEST_MIGRATION_DATABASE") else {
            return Ok(());
        };
        let path = PathBuf::from(path);
        let ready = PathBuf::from(
            std::env::var_os("SHEPHERD_TEST_MIGRATION_READY").ok_or("missing ready path")?,
        );
        let _lock = lock_database_migrations(&path)?;
        std::fs::write(ready, "ready")?;
        std::thread::sleep(std::time::Duration::from_secs(30));
        Ok(())
    }

    #[tokio::test]
    async fn database_migration_lock_is_released_when_another_process_dies()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.join("cross-process.db");
        let ready = directory.join("ready");
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "store::tests::migration_lock_subprocess_fixture",
            ])
            .env("SHEPHERD_TEST_MIGRATION_DATABASE", &path)
            .env("SHEPHERD_TEST_MIGRATION_READY", &ready)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if ready.exists() {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                if let Some(status) = child.try_wait()? {
                    return Err(format!("migration lock fixture exited early: {status}").into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await??;
        let open_path = path.clone();
        let mut open = tokio::spawn(async move { TursoStore::open_at(&open_path).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut open)
                .await
                .is_err()
        );
        assert!(!path.exists());
        child.kill().await?;
        let store = tokio::time::timeout(std::time::Duration::from_secs(10), open).await???;
        assert!(store.load_projects().await?.is_empty());
        Ok(())
    }

    #[test]
    fn cancelled_database_open_does_not_occupy_a_blocking_worker()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.join("cancelled.db");
        let lock = lock_database_migrations(&path)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?;
        runtime.block_on(async {
            let open_path = path.clone();
            let open = tokio::spawn(async move { TursoStore::open_at(&open_path).await });
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            assert!(!open.is_finished());
            open.abort();
            assert!(open.await.is_err());
            let worker = tokio::task::spawn_blocking(|| 42);
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(2), worker).await??,
                42
            );
            assert!(!path.exists());
            Ok::<(), Box<dyn std::error::Error>>(())
        })?;
        drop(lock);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replaced_migration_lock_is_rejected_before_database_open()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.join("replace.db");
        let held = lock_database_migrations(&path)?;
        let (waiting_file, lock_path) = open_database_migration_lock(&path)?;
        let wait_path = lock_path.clone();
        let mut waiting = tokio::spawn(async move {
            wait_for_database_migration_lock(waiting_file, wait_path, "fixture").await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut waiting)
                .await
                .is_err()
        );
        std::fs::rename(&lock_path, directory.join("old.lock"))?;
        std::fs::write(&lock_path, "replacement")?;
        drop(held);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), waiting).await??;
        assert!(
            matches!(result, Err(StoreError::Backend(message)) if message.contains("lock changed"))
        );
        assert!(!path.exists());
        assert!(
            TursoStore::open_at(&path)
                .await?
                .load_projects()
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn database_open_waits_for_the_shared_migration_lock()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.join("locked.db");
        let lock = lock_database_migrations(&path)?;
        let open_path = path.clone();
        let mut open = tokio::spawn(async move { TursoStore::open_at(&open_path).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut open)
                .await
                .is_err()
        );
        assert!(!path.exists());
        drop(lock);
        let store = tokio::time::timeout(std::time::Duration::from_secs(10), open).await???;
        assert!(store.load_projects().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn task_saves_require_a_project_and_preserve_task_ownership()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let first = Project::new(root.join("first"));
            let mut second = Project::new(root.join("second"));
            let mut task = Task::draft(first.id.clone(), "Task", root);
            assert!(matches!(
                store.save_task(&task).await,
                Err(StoreError::TaskOwnerMissing { .. })
            ));
            store.save_project(&first).await?;
            store.save_project(&second).await?;
            store.save_task(&task).await?;
            let session = SessionRecord::started(task.id.clone(), "codex", None);
            store.save_session(&session).await?;
            let mut moved = task.clone();
            moved.project_id = second.id.clone();
            moved.title = "Moved".into();
            assert!(matches!(
                store.save_task(&moved).await,
                Err(StoreError::TaskOwnerMissing { .. })
            ));
            assert!(matches!(
                store.save_task_with_transcript(&moved, &session, &[]).await,
                Err(StoreError::TaskOwnerMissing { .. })
            ));
            second.last_task_id = Some(task.id.clone());
            assert!(matches!(
                store.save_draft(&second, &moved).await,
                Err(StoreError::TaskOwnerMissing { .. })
            ));
            let projects = store.load_projects().await?;
            assert!(
                projects
                    .iter()
                    .find(|project| project.id == second.id)
                    .ok_or("missing second project")?
                    .last_task_id
                    .is_none()
            );
            let tasks = store.load_tasks().await?;
            let stored = tasks.first().ok_or("missing task")?;
            assert_eq!(stored.project_id, first.id);
            assert_eq!(stored.title, "Task");
            task.title = "Updated".into();
            store.save_task(&task).await?;
            assert_eq!(
                store
                    .load_tasks()
                    .await?
                    .first()
                    .ok_or("missing task")?
                    .title,
                "Updated"
            );
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("task-owner.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn archive_rejects_duplicates_and_preserves_current_task_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.to_path_buf());
            let mut task = Task::draft(project.id.clone(), "Current title", root);
            task.state = TaskState::Review;
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            let mut stale = task.clone();
            stale.title = "Stale title".into();
            stale.project_id = ProjectId::new("other-project");
            stale.updated_at = task.updated_at + 1;
            assert!(matches!(
                store.archive_tasks(&[stale.clone(), stale.clone()]).await,
                Err(StoreError::ReviewStateChanged { .. })
            ));
            let original = store
                .load_tasks()
                .await?
                .into_iter()
                .next()
                .ok_or("missing task")?;
            assert_eq!(original.state, TaskState::Review);
            assert_eq!(original.title, "Current title");
            store.archive_tasks(&[stale.clone()]).await?;
            let archived = store
                .load_tasks()
                .await?
                .into_iter()
                .next()
                .ok_or("missing archived task")?;
            assert_eq!(archived.state, TaskState::Done);
            assert_eq!(archived.title, "Current title");
            assert_eq!(archived.project_id, project.id);
            assert_eq!(archived.updated_at, stale.updated_at);
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("archive-state.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_layout_requires_a_project_and_failed_deletion_preserves_task()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.join("present"));
            let missing = Project::new(root.join("missing"));
            let task = Task::draft(project.id.clone(), "Task", root);
            let layout = TerminalLayout {
                revision: 3,
                ..TerminalLayout::default()
            };
            assert!(matches!(
                store.save_terminal_layout(&missing.id, &layout).await,
                Err(StoreError::TerminalLayoutOwnerMissing { .. })
            ));
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            assert!(matches!(
                store
                    .delete_task(&task.id, Some((&missing.id, &layout)))
                    .await,
                Err(StoreError::TerminalLayoutOwnerMissing { .. })
            ));
            assert!(
                store
                    .load_tasks()
                    .await?
                    .iter()
                    .any(|stored| stored.id == task.id)
            );
            assert!(store.load_terminal_layouts().await?.is_empty());
            store.save_terminal_layout(&project.id, &layout).await?;
            store
                .delete_task(&task.id, Some((&project.id, &layout)))
                .await?;
            assert!(store.load_tasks().await?.is_empty());
            assert_eq!(
                store.load_terminal_layouts().await?,
                vec![(project.id, layout)]
            );
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("terminal-layout-owner.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn workspace_layout_saves_require_an_existing_task()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.to_path_buf());
            let task = Task::draft(project.id.clone(), "Task", root);
            let layout = WorkspaceLayout {
                inspector_width: Some(400),
            };
            assert!(matches!(
                store.save_workspace_layout(&task.id, layout).await,
                Err(StoreError::WorkspaceOwnerMissing { .. })
            ));
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            store.save_workspace_layout(&task.id, layout).await?;
            let updated = WorkspaceLayout {
                inspector_width: Some(600),
            };
            store.save_workspace_layout(&task.id, updated).await?;
            assert_eq!(
                store.load_workspace_layouts().await?,
                vec![(task.id.clone(), updated)]
            );
            store.delete_task(&task.id, None).await?;
            assert!(matches!(
                store.save_workspace_layout(&task.id, layout).await,
                Err(StoreError::WorkspaceOwnerMissing { .. })
            ));
            assert!(store.load_workspace_layouts().await?.is_empty());
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("workspace-owner.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_save_requires_matching_owner_and_rejects_late_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.join("one"));
            let other = Project::new(root.join("two"));
            let task = Task::draft(project.id.clone(), "Task", root);
            let shared = TerminalRecord::project_shared(
                TerminalId::new("shared"),
                project.id.clone(),
                "Shared",
                root.to_path_buf(),
                "shell",
                1,
            );
            let mut terminal = TerminalRecord::project_shared(
                TerminalId::new("task"),
                project.id.clone(),
                "Task",
                root.to_path_buf(),
                "shell",
                1,
            )
            .for_task(task.id.clone());
            assert!(matches!(
                store.save_terminal(&shared).await,
                Err(StoreError::TerminalOwnerMissing { .. })
            ));
            store.save_project(&project).await?;
            store.save_project(&other).await?;
            store.save_task(&task).await?;
            store.save_terminal(&shared).await?;
            store.save_terminal(&terminal).await?;
            let mut wrong_project = terminal.clone();
            wrong_project.id = TerminalId::new("wrong-project");
            wrong_project.project_id = other.id.clone();
            assert!(matches!(
                store.save_terminal(&wrong_project).await,
                Err(StoreError::TerminalOwnerMissing { .. })
            ));
            let mut moved = shared.clone();
            moved.project_id = other.id.clone();
            assert!(matches!(
                store.save_terminal(&moved).await,
                Err(StoreError::TerminalOwnerMissing { .. })
            ));
            terminal.ended_at = Some(2);
            store.save_terminal(&terminal).await?;
            store.delete_task(&task.id, None).await?;
            assert!(matches!(
                store.save_terminal(&terminal).await,
                Err(StoreError::TerminalOwnerMissing { .. })
            ));
            let remaining = store.load_terminals().await?;
            assert_eq!(remaining.len(), 1);
            assert_eq!(
                remaining.first().ok_or("missing shared terminal")?.id,
                shared.id
            );
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("terminal-owner.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn session_save_requires_existing_task_and_preserves_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.to_path_buf());
            let first = Task::draft(project.id.clone(), "First", root);
            let second = Task::draft(project.id.clone(), "Second", root);
            let mut session = SessionRecord::started(first.id.clone(), "codex", None);
            assert!(matches!(
                store.save_session(&session).await,
                Err(StoreError::SessionOwnerMissing { .. })
            ));
            store.save_project(&project).await?;
            store.save_task(&first).await?;
            store.save_task(&second).await?;
            store.save_session(&session).await?;
            let mut reassigned = session.clone();
            reassigned.task_id = second.id.clone();
            assert!(matches!(
                store.save_session(&reassigned).await,
                Err(StoreError::SessionOwnerMissing { .. })
            ));
            assert_eq!(store.load_sessions().await?, vec![session.clone()]);
            session.end(Some(0), None);
            store.save_session(&session).await?;
            assert_eq!(store.load_sessions().await?, vec![session.clone()]);
            store.delete_task(&first.id, None).await?;
            assert!(matches!(
                store.save_session(&session).await,
                Err(StoreError::SessionOwnerMissing { .. })
            ));
            assert!(store.load_sessions().await?.is_empty());
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("session-owner.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn memory_delete_and_transcript_save_are_serialized()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let store = Arc::new(MemoryStore::new());
        let mut project = Project::new(directory.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Original", directory.path());
        project.last_task_id = Some(task.id.clone());
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        let session = SessionRecord::started(task.id.clone(), "codex", None);
        store.save_session(&session).await?;
        let transcripts = store.transcripts.write().await;
        let deletion = tokio::spawn({
            let store = store.clone();
            let id = task.id.clone();
            async move { store.delete_task(&id, None).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while store.tasks.try_read().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let save = tokio::spawn({
            let store = store.clone();
            let task = task.clone();
            async move { store.save_task_with_transcript(&task, &session, &[]).await }
        });
        drop(transcripts);
        tokio::time::timeout(std::time::Duration::from_secs(2), deletion).await???;
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), save).await??,
            Err(StoreError::TranscriptOwnerMissing { .. })
        ));
        assert!(store.load_tasks().await?.is_empty());
        assert!(store.load_sessions().await?.is_empty());
        assert!(store.load_transcript(&task.id).await?.is_empty());
        assert!(
            store
                .load_projects()
                .await?
                .first()
                .ok_or("missing project")?
                .last_task_id
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn memory_draft_and_transcript_saves_do_not_deadlock()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let store = Arc::new(MemoryStore::new());
        let mut project = Project::new(directory.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Current", directory.path());
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        let session = SessionRecord::started(task.id.clone(), "codex", None);
        store.save_session(&session).await?;
        let sessions = store.sessions.write().await;
        let transcript_save = tokio::spawn({
            let store = store.clone();
            async move { store.save_task_with_transcript(&task, &session, &[]).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while store.tasks.try_write().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let new_task = Task::draft(project.id.clone(), "New", directory.path());
        project.last_task_id = Some(new_task.id.clone());
        let (started, starting) = tokio::sync::oneshot::channel();
        let draft_save = tokio::spawn({
            let store = store.clone();
            async move {
                started
                    .send(())
                    .map_err(|_| StoreError::Backend("test listener closed".into()))?;
                store.save_draft(&project, &new_task).await
            }
        });
        starting.await?;
        tokio::task::yield_now().await;
        assert!(
            store.projects.try_write().is_ok(),
            "draft must not hold the project lock while waiting for tasks"
        );
        drop(sessions);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            transcript_save.await??;
            draft_save.await??;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await??;
        assert_eq!(store.load_tasks().await?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn empty_transcript_task_save_requires_a_persisted_session_and_task()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.to_path_buf());
            let task = Task::draft(project.id.clone(), "Original", root);
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            let session = SessionRecord::started(task.id.clone(), "codex", None);
            let mut changed = task.clone();
            changed.title = "Changed".into();
            assert!(matches!(
                store
                    .save_task_with_transcript(&changed, &session, &[])
                    .await,
                Err(StoreError::TranscriptOwnerMissing { .. })
            ));
            assert_eq!(
                store
                    .load_tasks()
                    .await?
                    .first()
                    .ok_or("missing task")?
                    .title,
                "Original"
            );
            store.save_session(&session).await?;
            store
                .save_task_with_transcript(&changed, &session, &[])
                .await?;
            assert_eq!(
                store
                    .load_tasks()
                    .await?
                    .first()
                    .ok_or("missing task")?
                    .title,
                "Changed"
            );
            store.delete_task(&task.id, None).await?;
            assert!(matches!(
                store
                    .save_task_with_transcript(&changed, &session, &[])
                    .await,
                Err(StoreError::TranscriptOwnerMissing { .. })
            ));
            assert!(store.load_tasks().await?.is_empty());
            Ok(())
        }
        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("empty-transcript.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn transcript_writes_reject_missing_or_mismatched_owners()
    -> Result<(), Box<dyn std::error::Error>> {
        async fn verify(store: &dyn Store, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
            let project = Project::new(root.to_path_buf());
            let first = Task::draft(project.id.clone(), "First", root);
            let second = Task::draft(project.id.clone(), "Second", root);
            store.save_project(&project).await?;
            store.save_task(&first).await?;
            store.save_task(&second).await?;
            let session = SessionRecord::started(first.id.clone(), "codex", None);
            store.save_session(&session).await?;
            let mut transcript = agent_harness::Transcript::default();
            transcript.push_user_message("Keep this prompt");
            let entry = transcript.entry(0).ok_or("missing prompt")?;

            let mut mismatched = session.clone();
            mismatched.task_id = second.id.clone();
            assert!(matches!(
                store.save_transcript_entries(&mismatched, &[entry]).await,
                Err(StoreError::TranscriptOwnerMissing { .. })
            ));
            let missing = SessionRecord::started(first.id.clone(), "codex", None);
            assert!(matches!(
                store.save_transcript_entries(&missing, &[entry]).await,
                Err(StoreError::TranscriptOwnerMissing { .. })
            ));
            mismatched.task_id = TaskId::new("deleted-task");
            assert!(matches!(
                store.save_transcript_entries(&mismatched, &[entry]).await,
                Err(StoreError::TranscriptOwnerMissing { .. })
            ));
            assert!(store.load_transcript(&first.id).await?.is_empty());
            assert!(store.load_transcript(&second.id).await?.is_empty());
            store.save_transcript_entries(&session, &[entry]).await?;
            assert_eq!(
                store.load_transcript(&first.id).await?,
                transcript.entries()
            );
            Ok(())
        }

        let directory = TestDirectory::new()?;
        verify(&MemoryStore::new(), directory.path()).await?;
        verify(
            &TursoStore::open_at(&directory.join("shepherd.db")).await?,
            directory.path(),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn worktree_deletion_journal_survives_task_deletion_and_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let project = Project::new(database.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Staged", database.path());
        let deletion = WorktreeDeletion {
            task_id: task.id.clone(),
            repository_root: database.path().join("repo"),
            original_path: database.path().join("original"),
            staged_path: database.path().join("staged"),
            worktree_git_dir: database.path().join("worktree-git-dir"),
            identity: "fixture-identity".into(),
        };
        for store in [
            Box::new(MemoryStore::new()) as Box<dyn Store>,
            Box::new(TursoStore::open(&uri).await?),
        ] {
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            store.save_worktree_deletion(&deletion).await?;
            assert!(store.save_worktree_deletion(&deletion).await.is_err());
            store.delete_task(&task.id, None).await?;
            assert!(store.load_tasks().await?.is_empty());
            assert_eq!(
                store.load_worktree_deletions().await?,
                vec![deletion.clone()]
            );
        }
        let reopened = TursoStore::open(&uri).await?;
        assert_eq!(
            reopened.load_worktree_deletions().await?,
            vec![deletion.clone()]
        );
        reopened.remove_worktree_deletion(&task.id).await?;
        assert!(reopened.load_worktree_deletions().await?.is_empty());
        Ok(())
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
        let mut project = Project::new(root.to_path_buf());
        let removed = Task::draft(project.id.clone(), "Removed", root);
        let kept = Task::draft(project.id.clone(), "Kept", root);
        project.last_task_id = Some(removed.id.clone());
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
        let terminal_layout = TerminalLayout {
            revision: 3,
            ..TerminalLayout::default()
        };
        store
            .delete_task(&removed.id, Some((&project.id, &terminal_layout)))
            .await?;
        assert_eq!(
            store.load_terminal_layouts().await?,
            vec![(project.id.clone(), terminal_layout)]
        );
        assert_eq!(
            store
                .load_projects()
                .await?
                .first()
                .and_then(|project| project.last_task_id.as_ref()),
            None
        );
        assert_eq!(
            store
                .load_tasks()
                .await?
                .iter()
                .map(|task| &task.id)
                .collect::<Vec<_>>(),
            [&kept.id]
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
    async fn resumed_collection_rechecks_existing_worktree_before_removal()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::Core;
        use crate::git::GitService;
        use crate::harness::UnavailableHarnessFactory;
        use crate::test_support::TestRepository;

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = Project::new(repository.path().to_path_buf());
        let mut task = Task::draft(
            project.id.clone(),
            "Interrupted",
            &repository.repository().git_dir,
        );
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        let tip =
            GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?;
        GitService.retain_task_tip(repository.repository(), task.id.as_str(), &tip)?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.branch_tip_sha = Some(tip);
        task.collection_pending = true;
        task.state = TaskState::Done;
        task.updated_at = crate::projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
        task.worktree_last_used_at = task.updated_at;
        std::fs::write(task.proposed_worktree_path.join("new-file"), "keep")?;
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        let core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(core.archived_worktree_issues().len(), 1);
        assert!(task.proposed_worktree_path.join("new-file").exists());
        drop(core);
        let store = TursoStore::open(&database.uri()).await?;
        assert!(
            !store
                .load_tasks()
                .await?
                .first()
                .ok_or("missing task")?
                .collection_pending
        );
        std::fs::remove_dir_all(&task.proposed_worktree_path)?;
        let core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(core.archived_worktree_issues().len(), 1);
        assert!(core.task_workspace(&task.id)?.task.worktree_path.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn unrelated_missing_archived_checkout_is_not_marked_collected()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::Core;
        use crate::git::GitService;
        use crate::harness::UnavailableHarnessFactory;
        use crate::test_support::TestRepository;

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = Project::new(repository.path().to_path_buf());
        let mut task = Task::draft(
            project.id.clone(),
            "Missing",
            &repository.repository().git_dir,
        );
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.state = TaskState::Done;
        task.updated_at = crate::projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
        task.worktree_last_used_at = task.updated_at;
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        std::fs::remove_dir_all(&task.proposed_worktree_path)?;
        let core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(core.archived_worktree_issues().len(), 1);
        assert!(core.task_workspace(&task.id)?.task.worktree_path.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn failed_collection_save_keeps_a_recoverable_task()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::git::GitService;
        use crate::harness::UnavailableHarnessFactory;
        use crate::test_support::TestRepository;
        use crate::{Command, CommandResult, Core, ReviewAction};

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = Project::new(repository.path().to_path_buf());
        let mut task = Task::draft(
            project.id.clone(),
            "Archived",
            &repository.repository().git_dir,
        );
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.base_branch = Some("main".into());
        task.state = TaskState::Done;
        task.updated_at = crate::projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
        task.worktree_last_used_at = task.updated_at;
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        let tip =
            GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?;
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement("CREATE TRIGGER block_collection BEFORE UPDATE ON tasks WHEN NEW.worktree_path IS NULL BEGIN SELECT RAISE(ABORT, 'collection save blocked'); END").exec(&mut *db).await?;
        }
        let core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert!(!task.proposed_worktree_path.exists());
        assert_eq!(core.archived_worktree_issues().len(), 1);
        assert!(
            core.archived_worktree_issues()
                .first()
                .ok_or("missing issue")?
                .reason
                .contains("collection save blocked")
        );
        {
            let mut db = toasty::Db::builder()
                .build(Turso::new(database.uri())?.experimental_index_method(true))
                .await?;
            toasty::sql::statement("DROP TRIGGER block_collection")
                .exec(&mut db)
                .await?;
        }
        drop(core);
        let reference = format!("refs/shepherd/tasks/{}", task.id.as_str());
        crate::git::run_git(repository.path(), &["update-ref", "-d", &reference])?;
        let blocked = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert_eq!(blocked.archived_worktree_issues().len(), 1);
        assert!(
            blocked
                .task_workspace(&task.id)?
                .task
                .worktree_path
                .is_some()
        );
        drop(blocked);
        GitService.retain_task_tip(repository.repository(), task.id.as_str(), &tip)?;
        let mut core = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert!(core.archived_worktree_issues().is_empty());
        assert!(core.task_workspace(&task.id)?.task.worktree_path.is_none());
        let restored = core
            .execute(Command::ReviewTask {
                task_id: task.id.clone(),
                action: ReviewAction::Reopen,
            })
            .await?;
        assert!(
            matches!(restored, CommandResult::TaskWorkspace(workspace) if workspace.task.state == TaskState::Review)
        );
        assert_eq!(
            GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?,
            tip
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_task_deletion_rolls_back_and_can_be_retried()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let mut project = Project::new(database.path().to_path_buf());
        let task = Task::draft(project.id.clone(), "Keep on failure", database.path());
        project.last_task_id = Some(task.id.clone());
        let session = SessionRecord::started(task.id.clone(), "codex", None);
        let mut transcript = agent_harness::Transcript::default();
        transcript.push_user_message_with_attachments(
            "Keep this",
            vec![AttachmentMetadata {
                id: "attachment".into(),
                relative_path: PathBuf::from("attachment/file.txt"),
                mime: "text/plain".into(),
                bytes: 12,
            }],
        );
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        store.save_session(&session).await?;
        let original_layout = TerminalLayout {
            revision: 1,
            ..TerminalLayout::default()
        };
        let updated_layout = TerminalLayout {
            revision: 2,
            ..TerminalLayout::default()
        };
        store
            .save_terminal_layout(&project.id, &original_layout)
            .await?;
        store
            .save_transcript_entries(&session, &[transcript.entry(0).ok_or("missing entry")?])
            .await?;
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement("ALTER TABLE terminals RENAME TO terminals_unavailable")
                .exec(&mut *db)
                .await?;
        }
        assert!(store.delete_task(&task.id, None).await.is_err());
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement("ALTER TABLE terminals_unavailable RENAME TO terminals")
                .exec(&mut *db)
                .await?;
        }
        assert_eq!(store.load_tasks().await?.len(), 1);
        assert_eq!(
            store
                .load_projects()
                .await?
                .first()
                .and_then(|project| project.last_task_id.as_ref()),
            Some(&task.id)
        );
        assert_eq!(store.load_sessions().await?.len(), 1);
        assert_eq!(store.load_transcript(&task.id).await?, transcript.entries());
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement(
                "ALTER TABLE terminal_layouts RENAME TO terminal_layouts_unavailable",
            )
            .exec(&mut *db)
            .await?;
        }
        assert!(
            store
                .delete_task(&task.id, Some((&project.id, &updated_layout)))
                .await
                .is_err()
        );
        {
            let mut db = store.db.lock().await;
            toasty::sql::statement(
                "ALTER TABLE terminal_layouts_unavailable RENAME TO terminal_layouts",
            )
            .exec(&mut *db)
            .await?;
        }
        assert_eq!(store.load_tasks().await?.len(), 1);
        assert_eq!(store.load_transcript(&task.id).await?, transcript.entries());
        assert_eq!(
            store.load_terminal_layouts().await?,
            vec![(project.id.clone(), original_layout)]
        );
        store
            .delete_task(&task.id, Some((&project.id, &updated_layout)))
            .await?;
        assert_eq!(
            store.load_terminal_layouts().await?,
            vec![(project.id.clone(), updated_layout)]
        );
        assert!(store.load_tasks().await?.is_empty());
        assert_eq!(
            store
                .load_projects()
                .await?
                .first()
                .and_then(|project| project.last_task_id.as_ref()),
            None
        );
        assert!(store.load_transcript(&task.id).await?.is_empty());
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
        reopened.delete_task(&exact.id, None).await?;
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
        let mut earlier = first.clone();
        if let EntryKind::UserMessage { text } = &mut earlier.kind {
            *text = "An earlier update in this batch must also roll back.".into();
        }
        let mut failed_batch = vec![&earlier; 32];
        failed_batch.push(&invalid);
        assert!(
            store
                .save_transcript_entries(&session, &failed_batch)
                .await
                .is_err()
        );
        assert_eq!(store.load_transcript(&task.id).await?, transcript.entries());
        store
            .save_transcript_entries(&session, &[first, second])
            .await?;
        drop(store);

        let reopened = TursoStore::open(&uri).await?;
        assert_eq!(
            reopened.load_transcript(&task.id).await?,
            transcript.entries()
        );
        reopened.delete_task(&task.id, None).await?;
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
    async fn migration_parser_preserves_quoted_semicolons_and_comments()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut db = toasty::Db::builder().build(Turso::in_memory()).await?;
        let migrations = [Migration {
            name: "fixture.sql",
            sql: "-- comment; with a semicolon\nCREATE TABLE example (value TEXT); INSERT INTO example VALUES ('hello; it''s working'); /* comment; */",
        }];
        apply_migration_set(&mut db, &migrations).await?;
        let rows = toasty::sql::query("SELECT value FROM example")
            .column_types([Type::String])
            .exec(&mut db)
            .await?;
        let Some(Value::Record(record)) = rows.first() else {
            return Err("missing migration row".into());
        };
        assert_eq!(required_string(record.first())?, "hello; it's working");
        Ok(())
    }

    #[tokio::test]
    async fn migration_rejects_transaction_control_before_applying_any_migration()
    -> Result<(), Box<dyn std::error::Error>> {
        for statement in [
            "CREATE TABLE escaped (value TEXT); BEGIN;",
            "CREATE TABLE escaped (value TEXT); COMMIT;",
            "CREATE TABLE escaped (value TEXT); END;",
            "CREATE TABLE escaped (value TEXT); ROLLBACK;",
            "CREATE TABLE escaped (value TEXT); SAVEPOINT nested;",
            "CREATE TABLE escaped (value TEXT); RELEASE nested;",
        ] {
            let mut db = toasty::Db::builder().build(Turso::in_memory()).await?;
            let migrations = [
                Migration {
                    name: "first.sql",
                    sql: "CREATE TABLE first_table (value TEXT);",
                },
                Migration {
                    name: "unsafe.sql",
                    sql: statement,
                },
            ];
            assert!(
                matches!(
                    apply_migration_set(&mut db, &migrations).await,
                    Err(StoreError::InvalidMigration { name, reason })
                        if name == "unsafe.sql" && reason.contains("transaction control")
                ),
                "accepted {statement}"
            );
            let tables = toasty::sql::query("SELECT name FROM sqlite_schema WHERE name IN ('first_table', 'escaped', '_migrations_history')")
                .column_types([Type::String])
                .exec(&mut db).await?;
            assert!(tables.is_empty(), "schema changed for {statement}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn migration_rejects_invalid_names_before_applying_any_migration()
    -> Result<(), Box<dyn std::error::Error>> {
        for (name, reason) in [("first.sql", "duplicated"), ("  ", "empty")] {
            let mut db = toasty::Db::builder().build(Turso::in_memory()).await?;
            let migrations = [
                Migration {
                    name: "first.sql",
                    sql: "CREATE TABLE first_table (value TEXT);",
                },
                Migration {
                    name,
                    sql: "CREATE TABLE second_table (value TEXT);",
                },
            ];
            assert!(matches!(apply_migration_set(&mut db, &migrations).await,
                Err(StoreError::InvalidMigration { reason: message, .. }) if message.contains(reason)));
            let tables = toasty::sql::query("SELECT name FROM sqlite_schema WHERE name IN ('first_table', 'second_table', '_migrations_history')")
                .column_types([Type::String]).exec(&mut db).await?;
            assert!(
                tables.is_empty(),
                "schema changed for invalid name {name:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn migration_failure_rolls_back_schema_and_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut db = toasty::Db::builder().build(Turso::in_memory()).await?;
        let migrations = [Migration {
            name: "broken.sql",
            sql: "CREATE TABLE example (value TEXT); INSERT INTO missing_table VALUES ('fail');",
        }];
        assert!(
            matches!(apply_migration_set(&mut db, &migrations).await, Err(StoreError::MigrationFailed { name, statement: 2, .. }) if name == "broken.sql")
        );
        let tables = toasty::sql::query("SELECT name FROM sqlite_schema WHERE name = 'example'")
            .column_types([Type::String])
            .exec(&mut db)
            .await?;
        assert!(tables.is_empty());
        let history = toasty::sql::query("SELECT name FROM _migrations_history")
            .column_types([Type::String])
            .exec(&mut db)
            .await?;
        assert!(history.is_empty());
        let repaired = [Migration {
            name: "broken.sql",
            sql: "CREATE TABLE example (value TEXT); INSERT INTO example VALUES ('saved');",
        }];
        apply_migration_set(&mut db, &repaired).await?;
        let rows = toasty::sql::query("SELECT value FROM example")
            .column_types([Type::String])
            .exec(&mut db)
            .await?;
        assert_eq!(rows.len(), 1);
        let Some(Value::Record(record)) = rows.first() else {
            return Err("missing migrated row".into());
        };
        assert_eq!(required_string(record.first())?, "saved");
        let history =
            toasty::sql::query("SELECT name FROM _migrations_history WHERE name = 'broken.sql'")
                .column_types([Type::String])
                .exec(&mut db)
                .await?;
        assert_eq!(history.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn malformed_migration_is_rejected_before_any_schema_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut db = toasty::Db::builder().build(Turso::in_memory()).await?;
        let migrations = [Migration {
            name: "malformed.sql",
            sql: "CREATE TABLE example (value TEXT); INSERT INTO example VALUES ('unterminated);",
        }];
        assert!(
            matches!(apply_migration_set(&mut db, &migrations).await, Err(StoreError::InvalidMigration { name, .. }) if name == "malformed.sql")
        );
        let tables = toasty::sql::query(
            "SELECT name FROM sqlite_schema WHERE name IN ('example', '_migrations_history')",
        )
        .column_types([Type::String])
        .exec(&mut db)
        .await?;
        assert!(tables.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn inaccessible_database_path_fails_before_migration()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let inaccessible = directory.path().join("private");
        std::fs::create_dir(&inaccessible)?;
        let path = inaccessible.join("shepherd.db");
        std::fs::write(&path, b"unchanged")?;
        std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o000))?;
        let result = TursoStore::open_at(&path).await;
        std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o700))?;
        assert!(
            matches!(result, Err(StoreError::Backend(message)) if message.contains("could not inspect database path"))
        );
        assert_eq!(std::fs::read(&path)?, b"unchanged");
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn failed_backup_blocks_migration_and_allows_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("shepherd.db");
        let mut db = toasty::Db::builder()
            .build(Turso::file(&path).experimental_index_method(true))
            .await?;
        let (last, previous) = MIGRATIONS.split_last().ok_or("missing migrations")?;
        apply_migration_set(&mut db, previous).await?;
        drop(db);
        drop(lock_database_migrations(&path)?);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500))?;
        let failed = TursoStore::open_at(&path).await;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        assert!(matches!(failed, Err(StoreError::Backup { .. })));
        let mut old = toasty::Db::builder()
            .build(Turso::file(&path).experimental_index_method(true))
            .await?;
        let rows = toasty::sql::query("SELECT name FROM _migrations_history WHERE name = ?1")
            .column_types([Type::String])
            .bind(last.name)
            .exec(&mut old)
            .await?;
        assert!(rows.is_empty());
        drop(old);
        let migrated = TursoStore::open_at(&path).await?;
        let rows = toasty::sql::query("SELECT name FROM _migrations_history WHERE name = ?1")
            .column_types([Type::String])
            .bind(last.name)
            .exec(&mut *migrated.db.lock().await)
            .await?;
        assert_eq!(rows.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn backs_up_old_database_before_migrating_and_preserves_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("shepherd's \"data\".db");
        let mut db = toasty::Db::builder()
            .build(Turso::file(&path).experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TABLE _migrations_history (name TEXT NOT NULL PRIMARY KEY, checksum TEXT NOT NULL)")
            .exec(&mut db).await?;
        for migration in MIGRATIONS
            .iter()
            .take_while(|migration| migration.name != "0013_worktree_last_used.sql")
        {
            for statement in parse_migration(migration)? {
                toasty::sql::statement(statement).exec(&mut db).await?;
            }
            toasty::sql::statement(
                "INSERT INTO _migrations_history (name, checksum) VALUES (?1, ?2)",
            )
            .bind(migration.name)
            .bind(checksum(migration.sql))
            .exec(&mut db)
            .await?;
        }
        toasty::sql::statement("INSERT INTO projects (id, path, name, created_at) VALUES ('project', '/legacy', 'Legacy', 1)")
            .exec(&mut db).await?;
        toasty::sql::statement("INSERT INTO tasks (id, project_id, title, state, tier, proposed_worktree_path, proposed_branch, created_at, updated_at, agent) VALUES ('task', 'project', 'Legacy task', 'draft', 'native', '/legacy/worktree', 'shepherd/legacy', 1, 42, 'codex')")
            .exec(&mut db).await?;
        toasty::sql::statement("INSERT INTO sessions (id, task_id, adapter, state, started_at) VALUES ('session', 'task', 'codex', 'ended', 1)")
            .exec(&mut db).await?;
        toasty::sql::statement("INSERT INTO entries (session_id, id, sequence, kind, payload, created_at, task_id, entry_text, task_title, project_name) VALUES ('session', 'entry', 0, 'userMessage', '{}', 1, 'task', 'uniquelysearchable', 'Legacy task', 'Legacy')")
            .exec(&mut db).await?;
        drop(db);

        let migrated = TursoStore::open_at(&path).await?;
        assert_eq!(
            migrated
                .load_tasks()
                .await?
                .first()
                .map(|task| task.worktree_last_used_at),
            Some(42)
        );
        let backups = std::fs::read_dir(directory.path())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|path| path.to_string_lossy().contains(".pre-migration-"))
            .collect::<Vec<_>>();
        let [backup] = backups.as_slice() else {
            return Err("expected one pre-migration backup".into());
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(backup)?.permissions().mode() & 0o777,
                0o700
            );
        }
        let mut old = toasty::Db::builder()
            .build(Turso::file(backup.join("snapshot.db")).experimental_index_method(true))
            .await?;
        let rows = toasty::sql::query("SELECT title, updated_at FROM tasks")
            .column_types([Type::String, Type::I64])
            .exec(&mut old)
            .await?;
        assert_eq!(rows.len(), 1);
        let matches = toasty::sql::query("SELECT id FROM entries WHERE fts_match(entry_text, task_title, project_name, 'uniquelysearchable')")
            .column_types([Type::String]).exec(&mut old).await?;
        assert_eq!(matches.len(), 1);
        assert!(
            toasty::sql::query("SELECT worktree_last_used_at FROM tasks")
                .column_types([Type::I64])
                .exec(&mut old)
                .await
                .is_err()
        );
        drop(old);
        drop(migrated);
        TursoStore::open_at(&path).await?;
        let count = std::fs::read_dir(directory.path())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .filter(|path| path.to_string_lossy().contains(".pre-migration-"))
            .count();
        assert_eq!(count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn refuses_unknown_migrations_before_changing_the_database()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("shepherd.db");
        let store = TursoStore::open_at(&path).await?;
        toasty::sql::statement(
            "INSERT INTO _migrations_history (name, checksum) VALUES ('9999_future.sql', 'future')",
        )
        .exec(&mut *store.db.lock().await)
        .await?;
        drop(store);
        assert!(
            matches!(TursoStore::open_at(&path).await, Err(StoreError::UnknownMigration { name }) if name == "9999_future.sql")
        );
        assert_eq!(
            std::fs::read_dir(directory.path())?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .filter(|path| path.to_string_lossy().contains(".pre-migration-"))
                .count(),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_gap_in_migration_history_before_changing_the_database()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("shepherd.db");
        let first = MIGRATIONS.first().ok_or("missing first migration")?;
        let second = MIGRATIONS.get(1).ok_or("missing second migration")?;
        let store = TursoStore::open_at(&path).await?;
        toasty::sql::statement("DELETE FROM _migrations_history WHERE name = ?1")
            .bind(first.name)
            .exec(&mut *store.db.lock().await)
            .await?;
        drop(store);
        assert!(matches!(TursoStore::open_at(&path).await,
            Err(StoreError::MigrationHistoryGap { missing, applied }) if missing == first.name && applied == second.name));
        let entries = std::fs::read_dir(directory.path())?.collect::<Result<Vec<_>, _>>()?;
        assert!(!entries.iter().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains(".pre-migration-")
        }));
        let mut db = toasty::Db::builder()
            .build(Turso::file(&path).experimental_index_method(true))
            .await?;
        let rows = toasty::sql::query("SELECT name FROM _migrations_history WHERE name = ?1")
            .column_types([Type::String])
            .bind(second.name)
            .exec(&mut db)
            .await?;
        assert_eq!(rows.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn refuses_modified_migration_history_before_migrating()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("shepherd.db");
        let store = TursoStore::open_at(&path).await?;
        toasty::sql::statement("UPDATE _migrations_history SET checksum = 'wrong' WHERE name = ?1")
            .bind(MIGRATIONS[0].name)
            .exec(&mut *store.db.lock().await)
            .await?;
        drop(store);
        assert!(
            matches!(TursoStore::open_at(&path).await, Err(StoreError::MigrationChecksum { name, .. }) if name == MIGRATIONS[0].name)
        );
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
        for statement in parse_migration(first)? {
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

        let task = Task::draft(project.id.clone(), "Boundary", directory.path());
        store.save_task(&task).await?;
        let tasks = store.load_tasks().await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
        assert_eq!(tasks[0].state, TaskState::Draft);
        assert_eq!(TaskId::new(tasks[0].id.as_str()), task.id);
        Ok(())
    }
}

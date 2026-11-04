//! Persistence boundary and its first durable implementation.
//!
//! The trait keeps the core free of a database-specific API. `TursoStore`
//! applies the embedded schema, then stores projects and tasks as typed values.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use toasty::stmt::{Type, Value};
use toasty_driver_turso::Turso;
use tokio::sync::{Mutex, RwLock};

use crate::projects::{Project, ProjectId};
use crate::tasks::{AgentTier, Task, TaskId, TaskState};

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
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError>;
    async fn save_project(&self, project: &Project) -> Result<(), StoreError>;
    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError>;
    async fn save_task(&self, task: &Task) -> Result<(), StoreError>;
}

struct Migration {
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    name: "0001_init.sql",
    sql: include_str!("../toasty/migrations/0001_init.sql"),
}];

/// The durable store used by the application. The database handle is guarded
/// because Toasty's raw SQL API needs exclusive access for each statement.
pub struct TursoStore {
    db: Mutex<toasty::db::Db>,
}

impl TursoStore {
    pub async fn open(db_uri: &str) -> Result<Self, StoreError> {
        let driver = Turso::new(db_uri)?;
        let mut db = toasty::Db::builder().build(driver).await?;
        apply_migrations(&mut db).await?;
        Ok(Self { db: Mutex::new(db) })
    }
}

#[async_trait]
impl Store for TursoStore {
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, path, name, missing, last_task_id, created_at \
             FROM projects ORDER BY created_at, id",
        )
        .column_types([
            Type::String,
            Type::String,
            Type::String,
            Type::I64,
            Type::String,
            Type::I64,
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
            "INSERT INTO projects (id, path, name, missing, last_task_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, NULLIF(?5, ''), ?6) \
             ON CONFLICT (id) DO UPDATE SET \
                path = excluded.path, \
                name = excluded.name, \
                missing = excluded.missing, \
                last_task_id = excluded.last_task_id, \
                created_at = excluded.created_at",
        )
        .bind(project.id.as_str())
        .bind(project.path.to_string_lossy().as_ref())
        .bind(&project.name)
        .bind(i64::from(project.missing))
        .bind(last_task_id)
        .bind(project.created_at as i64)
        .exec(&mut *db)
        .await?;
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let mut db = self.db.lock().await;
        let rows = toasty::sql::query(
            "SELECT id, project_id, title, state, tier, proposed_worktree_path, \
                    proposed_branch, worktree_path, branch, base_branch, error, \
                    created_at, updated_at \
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
                created_at, updated_at\
             ) VALUES (\
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, \
                NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), ?12, ?13\
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
                updated_at = excluded.updated_at",
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
        .exec(&mut *db)
        .await?;
        Ok(())
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

        for statement in split_sql(migration.sql) {
            toasty::sql::statement(statement).exec(&mut *db).await?;
        }
        toasty::sql::statement("INSERT INTO _migrations_history (name, checksum) VALUES (?1, ?2)")
            .bind(migration.name)
            .bind(&expected)
            .exec(&mut *db)
            .await?;
    }

    Ok(())
}

fn checksum(sql: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql.as_bytes());
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
    Ok(Task {
        id: TaskId::new(required_string(record.first())?),
        project_id: ProjectId::new(required_string(record.get(1))?),
        title: required_string(record.get(2))?,
        state,
        tier,
        proposed_worktree_path: PathBuf::from(required_string(record.get(5))?),
        proposed_branch: required_string(record.get(6))?,
        worktree_path: optional_string(record.get(7))?.map(PathBuf::from),
        branch: optional_string(record.get(8))?,
        base_branch: optional_string(record.get(9))?,
        error: optional_string(record.get(10))?,
        created_at: required_u64(record.get(11))?,
        updated_at: required_u64(record.get(12))?,
    })
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
    projects: RwLock<BTreeMap<String, Project>>,
    tasks: RwLock<BTreeMap<String, Task>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for MemoryStore {
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
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
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
    async fn turso_schema_migrates_and_persists_across_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut project = Project::new(database.path().to_path_buf());
        let task = Task::draft(
            project.id.clone(),
            "Persist across restart",
            database.path(),
        );
        project.last_task_id = Some(task.id.clone());

        {
            let store = TursoStore::open(&uri).await?;
            let tables = table_names(&store).await?;
            for expected in ["projects", "tasks", "sessions", "entries"] {
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
        assert_eq!(tasks[0].tier, AgentTier::Native);
        assert_eq!(tasks[0].proposed_branch, task.proposed_branch);
        assert!(tasks[0].worktree_path.is_none());
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

use std::backtrace::Backtrace;

use serde::Deserialize;
use snafu::{ResultExt, Snafu};
use turso::{Connection, params};
use turso_mappers::{QueryAsByName, TryFromRowByName};

use crate::{
    types::Task,
    utils::{ms_to_systime, systime_to_ms},
};

pub struct TursoStorage {
    conn: Connection,
    // db: libsql_orm::Database,
}

impl TursoStorage {
    pub async fn new(builder: turso::Builder) -> StorResult<Self> {
        // let db = libsql_orm::Database::new_connect(":memory:", "")
        //     .await
        //     .context(LibsqlErr {
        //         msg: "new_connect()".to_string(),
        //     })?;
        let db = builder.build().await.context(DbErr)?;
        let conn = db.connect().context(DbErr)?;
        // let db = libsql_orm::Database::new(conn);
        let self_ = Self { conn };

        // Apply all pending migrations
        self_.init_schema().await?;
        // Self::MIGRATIONS.to_latest(&mut conn).await.unwrap();

        Ok(self_)
    }

    // const MIGRATIONS: Migrations<'static> = {
    //     async fn my_migration(conn: &turso::Connection) -> turso::Result<()> {
    //         conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", ())
    //             .await?;
    //         Ok(())
    //     }
    //     Migrations::new(&[
    //         Migration::up(
    //             "001",
    //             "CREATE TABLE IF NOT EXISTS issues (
    //                 id TEXT PRIMARY KEY,
    //                 identifier TEXT NOT NULL,
    //                 title TEXT NOT NULL,
    //                 description TEXT,
    //                 priority INTEGER,
    //                 state TEXT NOT NULL,
    //                 branch_name TEXT,
    //                 url TEXT,
    //                 labels TEXT NOT NULL,       -- JSON Array
    //                 blocked_by TEXT NOT NULL,   -- JSON Array
    //                 created_at INTEGER,         -- Unix milliseconds
    //                 updated_at INTEGER          -- Unix milliseconds
    //             );",
    //         ),
    //         Migration::up(
    //             "002",
    //             "CREATE TABLE IF NOT EXISTS run_attempts (
    //                 task_id TEXT PRIMARY KEY,
    //                 task_identifier TEXT NOT NULL,
    //                 attempt INTEGER,
    //                 workspace_path TEXT NOT NULL,
    //                 started_at INTEGER NOT NULL,
    //                 status TEXT NOT NULL,
    //                 error TEXT
    //             );",
    //         ),
    //         Migration::up(
    //             "003",
    //             "CREATE TABLE IF NOT EXISTS retry_entries (
    //                 task_id TEXT PRIMARY KEY,
    //                 identifier TEXT NOT NULL,
    //                 attempt INTEGER NOT NULL,
    //                 due_at_ms INTEGER NOT NULL,
    //                 timer_handle INTEGER,
    //                 error TEXT
    //             );",
    //         ),
    //         // 2. From a file
    //         // up_file!("../tests/migration-files/001_test.sql"),
    //         // 3. Rust function
    //         // up_fn!("003", my_migration),
    //     ])
    // };

    /// Setup schema tables
    pub async fn init_schema(&self) -> StorResult<()> {
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS tasks (
                      id TEXT PRIMARY KEY,
                      title TEXT NOT NULL,
                      description TEXT,
                      branch_name TEXT,
                      labels TEXT NOT NULL,                         -- JSON Array
                      blocked_by TEXT NOT NULL,                     -- JSON Array
                      updated_at INTEGER,                           -- Unix milliseconds
                      created_at INTEGER,                           -- Unix milliseconds
                      deadline INTEGER,                             -- Unix milliseconds
                      importance_factor REAL NOT NULL DEFAULT 1.0   -- Simple multiplier (e.g., 1.0 to 10.0)
                  ); ",
                (),
            )
            .await
            .context(DbErr)?;

        // self.conn.execute("CREATE TABLE IF NOT EXISTS workflow_definitions (
        //           id TEXT PRIMARY KEY,        -- Using a static key 'current' if singleton, or dynamic keys
        //           config TEXT NOT NULL,       -- JSON Text
        //           prompt_template TEXT NOT NULL
        //       ); ", ()).await.context(DbErr)?;

        // self.conn
        //     .execute(
        //         "CREATE TABLE IF NOT EXISTS run_attempts (
        //               task_id TEXT PRIMARY KEY,
        //               task_identifier TEXT NOT NULL,
        //               attempt INTEGER,
        //               workspace_path TEXT NOT NULL,
        //               started_at INTEGER NOT NULL,
        //               status TEXT NOT NULL,
        //               error TEXT
        //           ); ",
        //         (),
        //     )
        //     .await
        //     .context(DbErr)?;

        // self.conn
        //     .execute(
        //         "CREATE TABLE IF NOT EXISTS live_sessions (
        //               session_id TEXT PRIMARY KEY,
        //               thread_id TEXT NOT NULL,
        //               turn_id TEXT NOT NULL,
        //               codex_app_server_pid INTEGER,
        //               last_codex_event TEXT,
        //               last_codex_timestamp INTEGER,
        //               last_codex_message TEXT,
        //               codex_input_tokens INTEGER NOT NULL,
        //               codex_output_tokens INTEGER NOT NULL,
        //               codex_total_tokens INTEGER NOT NULL,
        //               last_reported_input_tokens INTEGER NOT NULL,
        //               last_reported_output_tokens INTEGER NOT NULL,
        //               last_reported_total_tokens INTEGER NOT NULL,
        //               turn_count INTEGER NOT NULL
        //           ); ",
        //         (),
        //     )
        //     .await
        //     .context(DbErr)?;

        // self.conn
        //     .execute(
        //         "CREATE TABLE IF NOT EXISTS retry_entries (
        //               task_id TEXT PRIMARY KEY,
        //               identifier TEXT NOT NULL,
        //               attempt INTEGER NOT NULL,
        //               due_at_ms INTEGER NOT NULL,
        //               timer_handle INTEGER,
        //               error TEXT
        //           ); ",
        //         (),
        //     )
        //     .await
        //     .context(DbErr)?;

        Ok(())
    }
}

#[derive(Debug, TryFromRowByName, Deserialize)]
struct TaskDb {
    pub id: String, // Primary Key string
    pub title: String,
    pub description: Option<String>,
    // pub state: String,
    pub branch_name: Option<String>,
    pub labels: String, // Serialized JSON Array string
    pub blocked_by: String, // Serialized JSON Array string
                        // pub created_at: Option<i64>, // Stored as Unix Milliseconds
                        // pub updated_at: Option<i64>, // Stored as Unix Milliseconds
}
impl TryFrom<&Task> for TaskDb {
    type Error = StorageError;
    fn try_from(domain: &Task) -> Result<Self, Self::Error> {
        Ok(Self {
            id: domain.id.clone(),
            title: domain.title.clone(),
            description: domain.description.clone(),
            branch_name: domain.branch_name.clone(),
            labels: serde_json::to_string(&domain.labels).context(SerializationErr)?,
            blocked_by: serde_json::to_string(&domain.blocked_by).context(SerializationErr)?,
            // created_at: domain.created_at.map(systime_to_ms),
            // updated_at: domain.updated_at.map(systime_to_ms),
        })
    }
}
impl TryFrom<TaskDb> for Task {
    type Error = StorageError;
    fn try_from(db_record: TaskDb) -> Result<Self, Self::Error> {
        Ok(Self {
            id: db_record.id,
            title: db_record.title,
            description: db_record.description,
            branch_name: db_record.branch_name,
            labels: serde_json::from_str(&db_record.labels).context(ParseRowErr)?,
            blocked_by: serde_json::from_str(&db_record.blocked_by).context(ParseRowErr)?,
            // created_at: db_record.created_at.map(ms_to_systime),
            // updated_at: db_record.updated_at.map(ms_to_systime),
        })
        .context(MapRowErr)
    }
}
impl TursoStorage {
    pub async fn save_task(&self, task: &Task) -> StorResult<()> {
        let labels_json = serde_json::to_string(&task.labels).context(SerializationErr)?;
        let blocked_json = serde_json::to_string(&task.blocked_by).context(SerializationErr)?;
        // let created_at = task.created_at.map(systime_to_ms);
        // let updated_at = task.updated_at.map(systime_to_ms);

        self.conn.execute(
              "INSERT OR REPLACE INTO tasks (id, title, description, branch_name, labels, blocked_by)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
              params![
                  task.id.clone(), task.title.clone(), task.description.clone(),
                  task.branch_name.clone(),
                  labels_json, blocked_json
              ]
          ).await.context(DbErr)?;

        Ok(())
    }

    pub async fn get_task(&self, id: &str) -> StorResult<Task> {
        // Querying records matching field conditions via Filter
        // let results = TaskDb::find_where(FilterOperator::Single(Filter::eq("id", id)), &self.db)
        //     .await
        //     .context(OrmErr)?;

        let tasks: Vec<TaskDb> = self
            .conn
            .query_as_by_name::<TaskDb>("SELECT * FROM tasks WHERE id = ?", [id])
            .await
            .context(MapRowErr)?;
        if let Some(task) = tasks.into_iter().next() {
            Ok(Task::try_from(task)?)
        } else {
            Err(StorageError::NotFound {
                kind: "task".to_string(),
                id: id.to_string(),
            })
        }
    }

    pub async fn list_tasks_by_priority(&self) -> Result<Vec<Task>, StorageError> {
        #[derive(Debug, TryFromRowByName, Deserialize)]
        pub struct TempTask {
            pub id: String,
            pub title: String,
            pub importance_factor: f64,
            pub urgency_factor: f64,
            pub priority_score: f64,
        }

        let sql = r#"
            SELECT id, title, importance_factor,
                   CASE WHEN deadline IS NULL THEN 1.0
                        ELSE 86400.0 / MAX(1.0, deadline - unixepoch('now'))
                   END AS urgency_factor,
                   importance_factor * CASE WHEN deadline IS NULL THEN 1.0
                                            ELSE 86400.0 / MAX(1.0, deadline - unixepoch('now'))
                                       END AS priority_score
            FROM tasks
            ORDER BY priority_score DESC
        "#;

        let tasks = self
            .conn
            .query_as_by_name::<TempTask>(sql, ())
            .await
            .context(MapRowErr)?;
        dbg!(&tasks);

        // Ok(tasks)
        todo!()
    }
}

pub type StorResult<T, E = StorageError> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Err)))]
pub enum StorageError {
    #[snafu(display("Database query failed: {source}"))]
    DbError {
        // #[snafu(source(from(turso::Error, |e:turso::Error| e.to_string())))]
        #[snafu(source(from(exact)))]
        source: turso::Error,
        #[snafu(backtrace)]
        backtrace: Backtrace,
    },

    #[snafu(display("Failed to map Db row: {}", source))]
    MapRowError {
        #[snafu(source(from(exact)))]
        source: turso_mappers::TursoMapperError,
    },

    #[snafu(display("Failed to parse Db row: {source}"))]
    ParseRowError {
        #[snafu(source(from(serde_json::Error, Box::new)))]
        source: Box<dyn core::error::Error>,
        // msg: String,
        #[snafu(backtrace)]
        backtrace: Backtrace,
    },

    // #[snafu(whatever, display("{message}"))]
    // OtherError {
    //     message: String,
    //     // Having a `source` is optional, but if it is present, it must
    //     // have this specific attribute and type:
    //     #[snafu(source(from(Box<dyn std::error::Error>, Some)))]
    //     source: Option<Box<dyn std::error::Error>>,
    // },
    // #[snafu(display("Failed DB query: {source}"))]
    // LibsqlError {
    //     // msg: Option<String>,
    //     #[snafu(source(from(exact)))]
    //     source: libsql_orm::compat::LibsqlError,
    // },
    // #[snafu(display("ORM error: {source}"))]
    // OrmError {
    //     // msg: Option<String>,
    //     #[snafu(source(from(exact)))]
    //     source: libsql_orm::Error,
    // },
    #[snafu(display("Serialization/Deserialization failed: {}", source))]
    SerializationError { source: serde_json::Error },

    #[snafu(display("Data corruption or unexpected null type in column: {}", message))]
    DataCorrupted { message: String },

    #[snafu(display("{kind} not found: {id}"))]
    NotFound { kind: String, id: String },
}

// #[derive(Debug, Snafu)]
// #[snafu(display("Could not read file {path}"))]
// #[snafu(whatever)]
// struct AnyErr {
//     source: std::io::Error,
//     path: String,
// }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BlockerRef;
    use turso::Builder;

    pub struct TestState {
        db: TursoStorage,
    }
    impl TestState {
        pub async fn new() -> anyhow::Result<Self> {
            let db = Builder::new_local(":memory:").build().await?;
            Ok(Self {
                db: TursoStorage::new(Builder::new_local(":memory:"))
                    .await
                    .unwrap(),
            })
        }
        pub async fn new_default() -> anyhow::Result<Self> {
            let db = Self::new().await?;
            db.generate_test_issues(10).await.unwrap();
            Ok(db)
        }
        pub async fn generate_test_issues(&self, count: usize) -> Result<(), StorageError> {
            let mut prev_issues: Vec<Task> = Vec::new();
            for i in 1..=count {
                let mut issue = Task {
                    id: format!("task_{:02}", i),
                    title: format!("Task {}", i),
                    description: Some(format!("Task description {}", i)),
                    branch_name: Some(format!("fix/pipeline-{}", i)),
                    labels: vec!["bug".to_string(), "backend".to_string()],
                    blocked_by: vec![BlockerRef {
                        id: Some("task_00".to_string()),
                    }],
                    // created_at: Some(now),
                    // updated_at: Some(now),
                };
                if i > 1 {
                    issue.blocked_by = vec![BlockerRef {
                        id: Some(prev_issues[i - 2].id.clone()),
                    }];
                }
                self.db.save_task(&issue).await?;
                prev_issues.push(issue);
            }
            Ok(())
        }
    }

    #[tokio::test]
    #[snafu::report]
    async fn test_task_lifecycle() -> Result<(), StorageError> {
        let storage = TursoStorage::new(Builder::new_local(":memory:")).await?;
        // let now = SystemTime::now();

        let task = Task {
            id: "task_01".to_string(),
            title: "Fix bug in pipeline".to_string(),
            description: Some("CI/CD pipeline failing on step 3".to_string()),
            branch_name: Some("fix/pipeline".to_string()),
            labels: vec!["bug".to_string(), "backend".to_string()],
            blocked_by: vec![BlockerRef {
                id: Some("task_00".to_string()),
            }],
            // created_at: Some(now),
            // updated_at: Some(now),
        };

        storage.save_task(&task).await?;

        let retrieved = storage.get_task("task_01").await?;
        assert_eq!(retrieved.id, task.id);
        assert_eq!(retrieved.labels, vec!["bug", "backend"]);
        assert_eq!(retrieved.blocked_by[0].id.as_deref(), Some("task_00"));

        Ok(())
    }

    #[tokio::test]
    #[snafu::report]
    async fn test_list_by_prio() -> Result<(), StorageError> {
        // TODO test re-sort and assert prios
        // TODO test urgency factor by setting deadline
        let test = TestState::new_default().await.unwrap();

        let tasks = test.db.list_tasks_by_priority().await?;

        let task_with_deadline_priority = tasks
            .iter()
            .find(|t| t.id == "task_01")
            .expect("Task with deadline not found");
        let task_without_deadline_priority = tasks
            .iter()
            .find(|t| t.id == "task_02")
            .expect("Task without deadline not found");
        // assert!(
        //     task_with_deadline_priority.priority_score
        //         > task_without_deadline_priority.priority_score
        // );

        Ok(())
    }
}

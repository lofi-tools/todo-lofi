use snafu::{ResultExt, Snafu};
use turso::{Connection, params};

use crate::{
    types::Issue,
    utils::{ms_to_systime, systime_to_ms},
};

pub struct TursoStorage {
    conn: Connection,
}

impl TursoStorage {
    pub async fn new(builder: turso::Builder) -> StorResult<Self> {
        let db = builder.build().await.context(DbErr)?;
        let conn = db.connect().context(DbErr)?;
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
    //                 issue_id TEXT PRIMARY KEY,
    //                 issue_identifier TEXT NOT NULL,
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
    //                 issue_id TEXT PRIMARY KEY,
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
                "CREATE TABLE IF NOT EXISTS issues (
                      id TEXT PRIMARY KEY,
                      identifier TEXT NOT NULL,
                      title TEXT NOT NULL,
                      description TEXT,
                      priority INTEGER,
                      state TEXT NOT NULL,
                      branch_name TEXT,
                      url TEXT,
                      labels TEXT NOT NULL,       -- JSON Array
                      blocked_by TEXT NOT NULL,   -- JSON Array
                      created_at INTEGER,         -- Unix milliseconds
                      updated_at INTEGER          -- Unix milliseconds
                  ); ",
                (),
            )
            .await
            .context(DbErr)?;

        self.conn.execute("CREATE TABLE IF NOT EXISTS workflow_definitions (
                  id TEXT PRIMARY KEY,        -- Using a static key 'current' if singleton, or dynamic keys
                  config TEXT NOT NULL,       -- JSON Text
                  prompt_template TEXT NOT NULL
              ); ", ()).await.context(DbErr)?;

        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS run_attempts (
                      issue_id TEXT PRIMARY KEY,
                      issue_identifier TEXT NOT NULL,
                      attempt INTEGER,
                      workspace_path TEXT NOT NULL,
                      started_at INTEGER NOT NULL,
                      status TEXT NOT NULL,
                      error TEXT
                  ); ",
                (),
            )
            .await
            .context(DbErr)?;

        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS live_sessions (
                      session_id TEXT PRIMARY KEY,
                      thread_id TEXT NOT NULL,
                      turn_id TEXT NOT NULL,
                      codex_app_server_pid INTEGER,
                      last_codex_event TEXT,
                      last_codex_timestamp INTEGER,
                      last_codex_message TEXT,
                      codex_input_tokens INTEGER NOT NULL,
                      codex_output_tokens INTEGER NOT NULL,
                      codex_total_tokens INTEGER NOT NULL,
                      last_reported_input_tokens INTEGER NOT NULL,
                      last_reported_output_tokens INTEGER NOT NULL,
                      last_reported_total_tokens INTEGER NOT NULL,
                      turn_count INTEGER NOT NULL
                  ); ",
                (),
            )
            .await
            .context(DbErr)?;

        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS retry_entries (
                      issue_id TEXT PRIMARY KEY,
                      identifier TEXT NOT NULL,
                      attempt INTEGER NOT NULL,
                      due_at_ms INTEGER NOT NULL,
                      timer_handle INTEGER,
                      error TEXT
                  ); ",
                (),
            )
            .await
            .context(DbErr)?;

        Ok(())
    }

    pub async fn save_issue(&self, issue: &Issue) -> StorResult<()> {
        let labels_json = serde_json::to_string(&issue.labels).context(SerializationErr)?;
        let blocked_json = serde_json::to_string(&issue.blocked_by).context(SerializationErr)?;
        let created_at = issue.created_at.map(systime_to_ms);
        let updated_at = issue.updated_at.map(systime_to_ms);

        self.conn.execute(
              "INSERT OR REPLACE INTO issues (id, identifier, title, description, priority, state, branch_name, url, labels, blocked_by, created_at, updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
              params![
                  issue.id.clone(), issue.identifier.clone(), issue.title.clone(), issue.description.clone(),
                  issue.priority, issue.state.clone(), issue.branch_name.clone(), issue.url.clone(),
                  labels_json, blocked_json, created_at, updated_at
              ]
          ).await.context(DbErr)?;

        Ok(())
    }

    pub async fn get_issue(&self, id: &str) -> StorResult<Option<Issue>> {
        let mut rows = self
            .conn
            .query("SELECT * FROM issues WHERE id = ?", params![id])
            .await
            .context(DbErr)?;

        if let Some(row) = rows.next().await.context(DbErr)? {
            let labels_raw: String = row.get(8).context(DbErr)?;
            let blocked_raw: String = row.get(9).context(DbErr)?;
            let created_raw: Option<i64> = row.get(10).context(DbErr)?;
            let updated_raw: Option<i64> = row.get(11).context(DbErr)?;

            Ok(Some(Issue {
                id: row.get(0).context(DbErr)?,
                identifier: row.get(1).context(DbErr)?,
                title: row.get(2).context(DbErr)?,
                description: row.get(3).context(DbErr)?,
                priority: row.get(4).context(DbErr)?,
                state: row.get(5).context(DbErr)?,
                branch_name: row.get(6).context(DbErr)?,
                url: row.get(7).context(DbErr)?,
                labels: serde_json::from_str(&labels_raw).context(SerializationErr)?,
                blocked_by: serde_json::from_str(&blocked_raw).context(SerializationErr)?,
                created_at: created_raw.map(ms_to_systime),
                updated_at: updated_raw.map(ms_to_systime),
            }))
        } else {
            Ok(None)
        }
    }
}

pub type StorResult<T, E = StorageError> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Err)))]
pub enum StorageError {
    #[snafu(display("Database query failed: {}", source))]
    DbError {
        #[snafu(source(from(exact)))]
        // #[snafu(source(from(turso::Error, |e| e)))]
        source: turso::Error,
    },

    #[snafu(display("Serialization/Deserialization failed: {}", source))]
    SerializationError { source: serde_json::Error },

    #[snafu(display("Data corruption or unexpected null type in column: {}", message))]
    DataCorrupted { message: String },
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::prelude::*;
    use crate::types::BlockerRef;
    use turso::Builder;

    #[tokio::test]
    async fn test_issue_lifecycle() -> WResult<()> {
        let storage = TursoStorage::new(Builder::new_local(":memory:")).await?;
        let now = SystemTime::now();

        let sample_issue = Issue {
            id: "issue_01".to_string(),
            identifier: "PROJ-404".to_string(),
            title: "Fix bug in pipeline".to_string(),
            description: Some("CI/CD pipeline failing on step 3".to_string()),
            priority: Some(1),
            state: "In Progress".to_string(),
            branch_name: Some("fix/pipeline".to_string()),
            url: Some("https://tracker.com/PROJ-404".to_string()),
            labels: vec!["bug".to_string(), "backend".to_string()],
            blocked_by: vec![BlockerRef {
                id: Some("issue_00".to_string()),
                identifier: Some("PROJ-403".to_string()),
                state: Some("Open".to_string()),
            }],
            created_at: Some(now),
            updated_at: Some(now),
        };

        // Roundtrip Save & Retrieve
        storage.save_issue(&sample_issue).await?;
        let retrieved = storage.get_issue("issue_01").await?.unwrap();

        assert_eq!(retrieved.id, sample_issue.id);
        assert_eq!(retrieved.identifier, sample_issue.identifier);
        assert_eq!(retrieved.labels, vec!["bug", "backend"]);
        assert_eq!(
            retrieved.blocked_by[0].identifier.as_deref(),
            Some("PROJ-403")
        );

        Ok(())
    }
}

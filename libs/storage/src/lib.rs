use snafu::ResultExt;
use toasty_driver_turso::Turso;

pub mod error;
pub mod migrations;
pub mod tag;
pub mod task;
pub mod tracing_setup;

pub mod prelude {
    pub use crate::error::{self, QueryErr, Result, StorageSetupErr};
    pub use crate::migrations::{MigrationEntry, MigrationError};
    pub use crate::tag::{Tag, TagId, TagNode};
    pub use crate::task::{BlockerRef, Task};
    pub use crate::{StorageConfig, TodoStore};
}
pub use prelude::*;

pub struct StorageConfig {
    pub db_uri: String,
}
impl std::fmt::Display for StorageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "db_uri: {}", self.db_uri)
    }
}

pub struct TodoStore {
    pub db: toasty::db::Db,
}
impl TodoStore {
    #[fastrace::trace(properties = { "config": "{config}" })]
    pub async fn new(config: &StorageConfig) -> Result<Self, StorageSetupErr> {
        let driver = Turso::new(&config.db_uri).context(error::TursoDriverSnafu)?;

        let db = toasty::Db::builder()
            .models(toasty::models!(task::Task, tag::Tag))
            .build(driver)
            .await
            .context(error::DbBuildSnafu)?;

        let mut store = Self { db };
        store.apply_pending_migrations().await?;

        tracing::info!("TodoStore initialized");
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use crate::{StorageConfig, TodoStore};

    const TAG_DAG_DDL: &[&str] = &[
        r#"CREATE TABLE IF NOT EXISTS tag_implications (
            "implier_id" INTEGER NOT NULL REFERENCES tags("id") ON DELETE CASCADE,
            "implied_id" INTEGER NOT NULL REFERENCES tags("id") ON DELETE CASCADE,
            PRIMARY KEY ("implier_id", "implied_id")
        )"#,
        r#"CREATE INDEX IF NOT EXISTS idx_tag_implications_implier ON tag_implications("implier_id")"#,
        r#"CREATE INDEX IF NOT EXISTS idx_tag_implications_implied ON tag_implications("implied_id")"#,
        r#"CREATE TABLE IF NOT EXISTS direct_task_tags (
            "task_id" INTEGER NOT NULL REFERENCES tasks("id") ON DELETE CASCADE,
            "tag_id" INTEGER NOT NULL REFERENCES tags("id") ON DELETE CASCADE,
            PRIMARY KEY ("task_id", "tag_id")
        )"#,
    ];

    impl TodoStore {
        #[cfg(test)]
        pub async fn for_test() -> Result<Self, crate::StorageSetupErr> {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = Self::new(&config).await?;

            for sql in TAG_DAG_DDL {
                toasty::sql::statement(sql.to_string())
                    .exec(&mut store.db)
                    .await
                    .map_err(|e| crate::StorageSetupErr::DbBuild { source: e })?;
            }

            Ok(store)
        }
    }
}

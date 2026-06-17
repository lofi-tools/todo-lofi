use snafu::ResultExt;
use toasty_driver_turso::Turso;

pub mod error;
pub mod migrations;
pub mod task;
pub mod tracing_setup;

pub mod prelude {
    pub use crate::error::{self, QueryErr, Result, StorageSetupErr};
    pub use crate::migrations::{MigrationEntry, MigrationError};
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
            .models(toasty::models!(task::Task))
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
    use crate::prelude::*;
    use crate::{StorageConfig, TodoStore};
    impl TodoStore {
        #[cfg(test)]
        pub async fn for_test() -> Result<Self, StorageSetupErr> {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            Self::new(&config).await
        }
    }
}

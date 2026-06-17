use sha2::{Digest, Sha256};
use snafu::ResultExt;
use std::collections::HashMap;
use toasty::schema::db::Migration;
use toasty_driver_turso::Turso;

pub mod error;
pub mod migrations;
pub mod task;
pub mod tracing_setup;

pub mod prelude {
    pub use crate::error::{Error, Result};
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
    pub async fn new(config: &StorageConfig) -> crate::error::Result<Self> {
        crate::tracing_setup::init_tracing();

        use snafu::ResultExt;
        let driver = Turso::new(&config.db_uri).context(crate::error::TursoDriverSnafu)?;

        let mut db = toasty::Db::builder()
            .models(toasty::models!(task::Task))
            .build(driver)
            .await
            .context(crate::error::DbBuildSnafu)?;

        Self::apply_pending_migrations(&mut db).await?;

        tracing::info!("TodoStore initialized");
        Ok(Self { db })
    }
}

#[cfg(test)]
mod tests {
    use crate::{StorageConfig, TodoStore};
    impl TodoStore {
        #[cfg(test)]
        pub async fn for_test() -> crate::error::Result<Self> {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            Self::new(&config).await
        }
    }
}

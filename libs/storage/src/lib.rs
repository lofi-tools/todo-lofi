pub mod task;
pub mod tracing_setup;

pub mod prelude {
    pub use crate::task::{BlockerRef, Task};
    pub use crate::{StorageConfig, TodoStore};
}
pub use prelude::*;
use toasty_driver_turso::Turso;

use std::collections::HashSet;
use std::path::Path;
use toasty::migration::History;
use toasty::schema::db::Migration;

pub struct StorageConfig {
    pub db_uri: String,
    pub toasty_toml: std::path::PathBuf,
}
impl std::fmt::Display for StorageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "db_uri: {}, toasty_toml: {}",
            self.db_uri,
            self.toasty_toml.display()
        )
    }
}

pub struct TodoStore {
    pub db: toasty::db::Db,
}
impl TodoStore {
    #[fastrace::trace(properties = { "config": "{config}" })]
    pub async fn new(config: &StorageConfig) -> anyhow::Result<Self> {
        crate::tracing_setup::init_tracing();

        let driver = Turso::new(&config.db_uri)?;
        // .experimental_triggers(true) // Enable triggers
        // .concurrent_writes()
        // .experimental_custom_types(true)
        // .experimental_generated_columns(true)
        // .experimental_materialized_views(true)
        // .experimental_vacuum(true)

        let db = toasty::Db::builder()
            .models(toasty::models!(task::Task))
            .build(driver)
            .await?;

        Self::apply_pending_migrations(&db, &config.toasty_toml).await?;

        tracing::info!("TodoStore initialized");
        Ok(Self { db })
    }

    #[fastrace::trace]
    async fn apply_pending_migrations(
        db: &toasty::db::Db,
        toasty_toml: &Path,
    ) -> toasty::Result<()> {
        let toasty_config = toasty_cli::Config::load_from(toasty_toml)
            .map_err(|e| toasty::Error::from_args(format_args!("{e}")))?;

        let history_path = toasty_config.migration.get_history_file_path();
        let migrations_dir = toasty_config.migration.get_migrations_dir();

        let history = History::load_or_default(&history_path)?;

        if history.entries().is_empty() {
            return Ok(());
        }

        let mut conn = db.driver().connect().await?;
        let applied = conn.applied_migrations().await?;
        let applied_ids: HashSet<u64> = applied.iter().map(|m| m.id()).collect();

        let pending: Vec<_> = history
            .entries()
            .iter()
            .filter(|m| !applied_ids.contains(&m.id))
            .collect();
        // dbg!(&pending);

        if pending.is_empty() {
            return Ok(());
        }

        for entry in &pending {
            let sql_path = migrations_dir.join(&entry.name);
            let sql = std::fs::read_to_string(&sql_path)?;
            // dbg!(&sql);
            let migration = Migration::new_sql(sql);
            conn.apply_migration(entry.id, &entry.name, &migration)
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{StorageConfig, TodoStore};
    impl TodoStore {
        #[cfg(test)]
        pub async fn for_test() -> anyhow::Result<Self> {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
                toasty_toml: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("Toasty.toml"),
            };
            Self::new(&config).await
        }
    }
}

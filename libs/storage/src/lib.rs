use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};
use snafu::ResultExt;
use std::collections::HashMap;
use std::path::Path;
use toasty::schema::db::Migration;
use toasty_driver_turso::Turso;

pub mod error;
pub mod task;
pub mod tracing_setup;

pub mod prelude {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::LazyLock;

    pub use crate::error::{Error, Result};
    pub use crate::task::{BlockerRef, Task};
    pub use crate::{StorageConfig, TodoStore};

    // pub static REPO: LazyLock<Result<PathBuf, String>> = LazyLock::new(|| {
    //     let path_bytes = Command::new("git")
    //         .arg("rev-parse")
    //         .arg("--show-toplevel")
    //         .output()
    //         .map_err(|e| e.to_string())?
    //         .stdout;
    //     let path_str = str::from_utf8(&path_bytes)
    //         .map_err(|e| e.to_string())?
    //         .trim();
    //     Ok(PathBuf::from(path_str))
    // });
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

    fn compute_migration_id(name: &str) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        let result = hasher.finalize();
        u64::from_be_bytes(result[..8].try_into().unwrap())
    }

    fn compute_checksum(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let bytes = hasher.finalize();
        bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
            .to_string()
    }

    #[fastrace::trace]
    async fn apply_pending_migrations(db: &mut toasty::db::Db) -> crate::error::Result<()> {
        let migrations_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("toasty/migrations");
        // dbg!(&migrations_dir);
        // static MIGRATIONS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

        // Ensure _migrations_history table exists
        toasty::sql::statement(
            r#"CREATE TABLE IF NOT EXISTS "_migrations_history" (
                "id" INTEGER PRIMARY KEY,
                "name" TEXT NOT NULL UNIQUE,
                "checksum" TEXT NOT NULL
            )"#,
        )
        .exec(db)
        .await
        .context(crate::error::MigrationsSnafu)?;

        let mut conn = db
            .driver()
            .connect()
            .await
            .context(crate::error::DbConnectSnafu)?;

        // Read migration files
        let mut entries: Vec<_> = std::fs::read_dir(&migrations_dir)
            .context(crate::error::MigrationsDirSnafu {
                path: migrations_dir.display().to_string(),
            })?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "sql"))
            .collect();
        entries.sort_by_key(|e| e.path());

        // Get applied migrations from _migrations_history
        let rows = toasty::sql::query(r#"SELECT name, checksum FROM "_migrations_history""#)
            .column_types([toasty::stmt::Type::String, toasty::stmt::Type::String])
            .exec(db)
            .await
            .context(crate::error::MigrationsSnafu)?;

        let mut applied: HashMap<String, String> = HashMap::new();
        for row in rows {
            if let toasty::stmt::Value::List(items) = row
                && items.len() >= 2
                && let (toasty::stmt::Value::String(name), toasty::stmt::Value::String(checksum)) =
                    (&items[0], &items[1])
            {
                applied.insert(name.clone(), checksum.clone());
            }
        }

        // Process each migration
        for entry in &entries {
            let name = entry.file_name().to_string_lossy().to_string();
            let sql_path = entry.path();
            let sql =
                std::fs::read_to_string(&sql_path).context(crate::error::MigrationSqlSnafu {
                    path: sql_path.display().to_string(),
                })?;
            let checksum = Self::compute_checksum(&sql);
            let id = Self::compute_migration_id(&name);

            if let Some(existing_checksum) = applied.get(&name) {
                if existing_checksum != &checksum {
                    return Err(crate::error::Error::MigrationChecksumMismatch {
                        name,
                        expected: existing_checksum.clone(),
                        actual: checksum,
                    });
                }
                tracing::debug!(name = %name, "migration verified");
            } else {
                tracing::info!(name = %name, "applying migration");
                let migration = Migration::new_sql(sql);
                match conn.apply_migration(id, &name, &migration).await {
                    Ok(_) => {}
                    Err(e) => {
                        return Err(crate::error::Error::Migrations { source: e });
                    }
                }

                toasty::sql::statement(
                    r#"INSERT INTO "_migrations_history" (id, name, checksum) VALUES (?1, ?2, ?3)"#,
                )
                .bind(id as i64)
                .bind(&name)
                .bind(&checksum)
                .exec(db)
                .await
                .context(crate::error::MigrationsSnafu)?;
            }
        }

        Ok(())
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

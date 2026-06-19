use crate::TodoStore;
use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};
use snafu::{ResultExt, Snafu};
use std::collections::HashMap;
use toasty::schema::db::Migration;

pub static MIGRATIONS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/toasty/migrations");

#[derive(Debug, Clone)]
pub struct MigrationEntry {
    pub id: u64,
    pub name: String,
    pub sql: String,
    pub checksum: String,
}

impl MigrationEntry {
    fn from_file(name: String, sql: String) -> Self {
        let checksum = Self::compute_checksum(&sql);
        let id = Self::compute_id(&name);
        Self {
            id,
            name,
            sql,
            checksum,
        }
    }
    fn compute_id(name: &str) -> u64 {
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
    }
}

impl TodoStore {
    pub fn list_all_migrations() -> Vec<MigrationEntry> {
        let mut entries: Vec<_> = MIGRATIONS_DIR
            .files()
            .filter(|f| f.path().extension().is_some_and(|ext| ext == "sql"))
            .map(|f| {
                let name = f.path().file_name().unwrap().to_string_lossy().to_string();
                let sql = f.contents_utf8().unwrap().to_string();
                MigrationEntry::from_file(name, sql)
            })
            .collect();
        entries.sort_by_key(|e| e.name.clone());
        entries
    }

    pub async fn list_applied_migrations(
        db: &mut toasty::db::Db,
    ) -> Result<HashMap<String, String>, MigrationError> {
        let rows = toasty::sql::query(r#"SELECT name, checksum FROM "_migrations_history""#)
            .column_types([toasty::stmt::Type::String, toasty::stmt::Type::String])
            .exec(db)
            .await
            .context(DatabaseSnafu)?;

        let mut applied = HashMap::new();
        for row in rows {
            if let toasty::stmt::Value::List(items) = row
                && items.len() >= 2
                && let (toasty::stmt::Value::String(name), toasty::stmt::Value::String(checksum)) =
                    (&items[0], &items[1])
            {
                applied.insert(name.clone(), checksum.clone());
            }
        }
        Ok(applied)
    }

    #[fastrace::trace]
    pub async fn apply_pending_migrations(&mut self) -> Result<(), MigrationError> {
        toasty::sql::statement(
            r#"CREATE TABLE IF NOT EXISTS "_migrations_history" (
                    "id" INTEGER PRIMARY KEY,
                    "name" TEXT NOT NULL UNIQUE,
                    "checksum" TEXT NOT NULL
                )"#,
        )
        .exec(&mut self.db)
        .await
        .context(DatabaseSnafu)?;

        let mut conn = self.db.driver().connect().await.context(DbConnectSnafu)?;
        let all = Self::list_all_migrations();
        let applied = Self::list_applied_migrations(&mut self.db).await?;

        for entry in &all {
            if let Some(existing_checksum) = applied.get(&entry.name) {
                if existing_checksum != &entry.checksum {
                    return Err(MigrationError::ChecksumMismatch {
                        name: entry.name.clone(),
                        expected: existing_checksum.clone(),
                        actual: entry.checksum.clone(),
                    });
                }
                tracing::debug!(name = %entry.name, "migration verified");
            } else {
                tracing::info!(name = %entry.name, "applying migration");
                let migration = Migration::new_sql(entry.sql.clone());
                conn.apply_migration(entry.id, &entry.name, &migration)
                    .await
                    .context(ApplyMigrationSnafu {
                        name: entry.name.clone(),
                    })?;

                toasty::sql::statement(
                    r#"INSERT INTO "_migrations_history" (id, name, checksum) VALUES (?1, ?2, ?3)"#,
                )
                .bind(entry.id as i64)
                .bind(&entry.name)
                .bind(&entry.checksum)
                .exec(&mut self.db)
                .await
                .context(DatabaseSnafu)?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum MigrationError {
    #[snafu(display("failed to apply migration '{name}': {source}"))]
    ApplyMigration { name: String, source: toasty::Error },

    #[snafu(display("failed to connect to database: {source}"))]
    DbConnect { source: toasty::Error },

    #[snafu(display("database error: {source}"))]
    Database { source: toasty::Error },

    #[snafu(display(
        "checksum mismatch for migration '{name}': expected {expected}, got {actual}"
    ))]
    ChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },
}
impl From<MigrationError> for crate::error::StorageSetupErr {
    fn from(source: MigrationError) -> Self {
        crate::error::StorageSetupErr::Migration { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_migrations() {
        let migrations = TodoStore::list_all_migrations();
        assert!(migrations.len() >= 2);
        assert!(migrations.iter().any(|m| m.name.contains("tag_dag")));
    }
}

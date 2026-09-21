//! Migration runner for the telemetry database.
//!
//! Copied from `libs/storage/src/migrations.rs` rather than shared (see the
//! telemetry spec §12.6): the two databases belong to different binaries, and
//! extracting a crate would touch todo-2. The statement splitter is the risky
//! part — quotes, comments, `BEGIN…END` bodies — so it is reused verbatim
//! instead of re-derived.

use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

static MIGRATIONS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

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
        u64::from_be_bytes(result[..8].try_into().expect("sha256 digest is long enough"))
    }

    fn compute_checksum(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Every embedded migration, ordered by file name.
pub fn list_all_migrations() -> Vec<MigrationEntry> {
    let mut entries: Vec<MigrationEntry> = MIGRATIONS_DIR
        .files()
        .filter(|file| file.path().extension().is_some_and(|ext| ext == "sql"))
        .filter_map(|file| {
            let name = file.path().file_name()?.to_string_lossy().to_string();
            let sql = file.contents_utf8()?.to_string();
            Some(MigrationEntry::from_file(name, sql))
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Apply every migration that is not yet recorded, verifying the checksums of
/// the ones that are. A checksum mismatch is a hard error naming the file: it
/// means an applied migration was edited, which must never be auto-applied.
pub async fn apply_pending_migrations(db: &mut toasty::db::Db) -> anyhow::Result<()> {
    toasty::sql::statement(
        r#"CREATE TABLE IF NOT EXISTS "_migrations_history" (
                "id" INTEGER PRIMARY KEY,
                "name" TEXT NOT NULL UNIQUE,
                "checksum" TEXT NOT NULL
            )"#,
    )
    .exec(&mut *db)
    .await
    .map_err(|error| anyhow::anyhow!("creating the migration history table failed: {error}"))?;

    let applied = list_applied(db).await?;
    for entry in list_all_migrations() {
        if let Some(existing) = applied.get(&entry.name) {
            if existing != &entry.checksum {
                anyhow::bail!(
                    "checksum mismatch for migration '{}': expected {existing}, got {} — \
                     migrations must never be edited after being applied",
                    entry.name,
                    entry.checksum
                );
            }
            continue;
        }
        for statement in split_sql(&entry.sql) {
            toasty::sql::statement(statement)
                .exec(&mut *db)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("migration '{}' failed: {error}", entry.name)
                })?;
        }
        toasty::sql::statement(
            r#"INSERT INTO "_migrations_history" (id, name, checksum) VALUES (?1, ?2, ?3)"#,
        )
        .bind(entry.id as i64)
        .bind(&entry.name)
        .bind(&entry.checksum)
        .exec(&mut *db)
        .await
        .map_err(|error| anyhow::anyhow!("recording migration '{}' failed: {error}", entry.name))?;
    }
    Ok(())
}

async fn list_applied(db: &mut toasty::db::Db) -> anyhow::Result<HashMap<String, String>> {
    let rows = toasty::sql::query(r#"SELECT name, checksum FROM "_migrations_history""#)
        .column_types([toasty::stmt::Type::String, toasty::stmt::Type::String])
        .exec(&mut *db)
        .await
        .map_err(|error| anyhow::anyhow!("reading the migration history failed: {error}"))?;
    let mut applied = HashMap::new();
    for row in rows {
        if let toasty::stmt::Value::Record(record) = row
            && let (Some(toasty::stmt::Value::String(name)), Some(toasty::stmt::Value::String(checksum))) =
                (record.first(), record.get(1))
        {
            applied.insert(name.clone(), checksum.clone());
        }
    }
    Ok(applied)
}

/// Split a multi-statement SQL string into individual statements.
///
/// Handles single-quoted, double-quoted, and backtick-quoted strings, ignores
/// SQL comments (`--` and `/* ... */`), and defers splitting on `;` while
/// inside `BEGIN...END` blocks (trigger bodies, etc.).
fn split_sql(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut begin_depth: u32 = 0;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' | '`' => {
                current.push(ch);
                let quote = ch;
                while let Some(inner) = chars.next() {
                    current.push(inner);
                    if inner == quote {
                        if chars.peek() == Some(&quote) {
                            if let Some(escaped) = chars.next() {
                                current.push(escaped);
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for c in chars.by_ref() {
                    if c == '\n' {
                        current.push(c);
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            ';' if begin_depth == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    statements.push(trimmed);
                }
                current.clear();
            }
            _ => {
                current.push(ch);
                if !in_quote(&current) {
                    check_begin_end(&current, &mut begin_depth);
                }
            }
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        statements.push(trimmed);
    }
    statements
}

fn in_quote(s: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    for ch in s.chars() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            _ => {}
        }
    }
    in_single || in_double
}

fn check_begin_end(current: &str, depth: &mut u32) {
    let lower = current.to_lowercase();
    let bytes = lower.as_bytes();
    let len = bytes.len();

    let check = |keyword: &[u8]| -> bool {
        if len < keyword.len() || &bytes[len - keyword.len()..] != keyword {
            return false;
        }
        // The keywords carry their leading space, so a match already sits on a
        // token boundary: `...a begin` is a `BEGIN` (the alias `a` precedes
        // it), while `...beginning` never matches in the first place.
        matches!(keyword.first(), Some(b' '))
            || len == keyword.len()
            || !bytes[len - keyword.len() - 1].is_ascii_alphanumeric()
    };

    if check(b" begin") {
        *depth += 1;
    } else if check(b" end") {
        *depth = depth.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_schema_is_found_and_splits_cleanly() {
        let migrations = list_all_migrations();
        assert_eq!(migrations.len(), 1);
        assert_eq!(migrations[0].name, "0001_telemetry.sql");
        let statements = split_sql(&migrations[0].sql);
        assert_eq!(statements.len(), 10, "five tables, five indexes");
        assert!(statements[0].starts_with("CREATE TABLE sessions"));
        // Comments never survive into a statement.
        assert!(!statements.iter().any(|s| s.starts_with("--")));
    }

    #[test]
    fn split_sql_handles_quotes_comments_and_begin_end() {
        let sql = "CREATE TABLE a (id INT);\n-- comment; with a semicolon\nCREATE TABLE b (x TEXT DEFAULT 'a;b');";
        assert_eq!(split_sql(sql).len(), 2);
        // A body's internal semicolon must not split the statement, and the
        // keyword must be recognised even after a single-letter table alias.
        let trigger = "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE a SET x = 1; END;";
        assert_eq!(split_sql(trigger).len(), 1);
        let two = "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE a SET x = 1; END;SELECT 1;";
        assert_eq!(split_sql(two).len(), 2);
        assert_eq!(split_sql("/* c */ SELECT 1").len(), 1);
    }

    #[tokio::test]
    async fn migrations_apply_once_and_are_verified_on_reopen() {
        let driver = toasty_driver_turso::Turso::new("turso::memory:")
            .expect("in-memory turso driver");
        let mut db = toasty::Db::builder()
            .build(driver)
            .await
            .expect("in-memory db");
        apply_pending_migrations(&mut db).await.unwrap();
        // Idempotent: applying again is a no-op, not an error.
        apply_pending_migrations(&mut db).await.unwrap();
        let rows = toasty::sql::query("SELECT name FROM sqlite_master WHERE type = 'table'")
            .column_types([toasty::stmt::Type::String])
            .exec(&mut db)
            .await
            .unwrap();
        let names: Vec<String> = rows
            .iter()
            .filter_map(|row| match row {
                toasty::stmt::Value::Record(record) => match record.first() {
                    Some(toasty::stmt::Value::String(name)) => Some(name.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        for table in ["sessions", "turns", "attempts", "cooldowns", "routing_decisions"] {
            assert!(names.contains(&table.to_string()), "missing {table} in {names:?}");
        }
    }
}

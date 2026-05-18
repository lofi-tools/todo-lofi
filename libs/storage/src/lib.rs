pub mod prelude {
    pub use crate::utils::TopLevelErr;
    pub use crate::utils::WResult;
}

pub mod storage;
pub mod types;

// #[tokio::main]
// async fn main() -> turso::Result<()> {
//     // Create an in-memory database
//     let db = Builder::new_local(":memory:").build().await?;
//     let mut conn = db.connect()?;

//     // Apply all pending migrations
//     MIGRATIONS.to_latest(&mut conn).await.unwrap();

//     // Create a table
//     conn.execute(
//         "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)",
//         (),
//     )
//     .await?;

//     // Insert data
//     conn.execute(
//         "INSERT INTO users (name, email) VALUES (?1, ?2)",
//         ["Alice", "alice@example.com"],
//     )
//     .await?;

//     conn.execute(
//         "INSERT INTO users (name, email) VALUES (?1, ?2)",
//         ["Bob", "bob@example.com"],
//     )
//     .await?;

//     // Query data
//     let mut rows = conn.query("SELECT * FROM users", ()).await?;

//     while let Some(row) = rows.next().await? {
//         let id = row.get_value(0)?;
//         let name = row.get_value(1)?;
//         let email = row.get_value(2)?;
//         println!(
//             "User: {} - {} ({})",
//             id.as_integer().unwrap_or(&0),
//             name.as_text().unwrap_or(&"".to_string()),
//             email.as_text().unwrap_or(&"".to_string())
//         );
//     }

//     Ok(())
// }

pub mod utils {
    use std::time::{SystemTime, UNIX_EPOCH};

    pub struct TopLevelErr(pub Box<dyn std::error::Error + Send + Sync>);
    impl std::fmt::Display for TopLevelErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Error: {}", self.0)
        }
    }
    impl std::fmt::Debug for TopLevelErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self)
        }
    }
    impl<E> From<E> for TopLevelErr
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        fn from(error: E) -> Self {
            TopLevelErr(Box::new(error))
        }
    }
    pub type WResult<T, E = TopLevelErr> = Result<T, E>;

    pub fn systime_to_ms(t: SystemTime) -> i64 {
        t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
    }
    pub fn ms_to_systime(ms: i64) -> SystemTime {
        UNIX_EPOCH + std::time::Duration::from_millis(ms as u64)
    }
}

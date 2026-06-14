pub mod task;

pub mod prelude {
    pub use crate::task::{BlockerRef, Task};
    pub use crate::{StorageConfig, TodoStore};
}
pub use prelude::*;

pub struct StorageConfig {
    pub db_url: String,
}

pub struct TodoStore {
    pub db: toasty::db::Db,
}
impl TodoStore {
    pub async fn new(db_uri: &str) -> toasty::Result<Self> {
        let db = toasty::Db::builder()
            .models(toasty::models!(task::Task))
            .connect(db_uri)
            .await?;
        Ok(Self { db })
    }
}

#[cfg(test)]
mod tests {
    use crate::TodoStore;
    impl TodoStore {
        #[cfg(test)]
        pub async fn for_test() -> toasty::Result<Self> {
            let storage = Self::new("turso::memory:").await?;
            storage.db.push_schema().await?;
            Ok(storage)
        }
    }
}

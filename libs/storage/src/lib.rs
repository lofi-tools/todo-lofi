use snafu::ResultExt;
use toasty_driver_turso::Turso;

pub mod error;
pub mod external;
pub mod link;
pub mod migrations;
pub mod repeat;
pub mod tag;
pub mod tag_settings;
pub mod task;
pub mod todoist;
pub mod testing;
pub mod tracing_setup;
pub mod trip;
pub mod workflow;

pub mod prelude {
    pub use crate::error::{self, QueryErr, QueryResult, StorageSetupErr};
    pub use crate::external::{ExternalComment, Integration, TagLink, TaskLink};
    pub use crate::link::LinkKind;
    pub use crate::migrations::{MigrationEntry, MigrationError};
    pub use crate::repeat::RepeatTaskTemplate;
    pub use crate::tag::{Tag, TagId, TagNode};
    pub use crate::tag_settings::{SyncTarget, TagSection, TagSettings};
    pub use crate::todoist::SyncSummary;
    pub use crate::task::{Task, TaskWithMeta};
    pub use crate::trip::{Trip, TripSection, TripWithItems};
    pub use crate::workflow::{
        Recipe, RecipeEdge, RecipeMeta, RecipeNode, RunStepView, RunView, WorkflowRecipe,
        WorkflowRun,
    };
    pub use crate::{StorageConfig, TodoStore};
    pub use toasty::Deferred;
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
    pub async fn new(config: &StorageConfig) -> QueryResult<Self, StorageSetupErr> {
        let driver = Turso::new(&config.db_uri).context(error::TursoDriverSnafu)?;

        let db = toasty::Db::builder()
            .models(toasty::models!(
                task::Task,
                tag::Tag,
                repeat::RepeatTaskTemplate,
                workflow::WorkflowRecipe,
                workflow::WorkflowRun,
                trip::Trip
            ))
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

    impl TodoStore {
        #[cfg(test)]
        pub async fn for_test() -> Result<Self, crate::StorageSetupErr> {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            TodoStore::new(&config).await
        }
    }
}

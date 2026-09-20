use snafu::ResultExt;
use toasty_driver_turso::Turso;

pub mod error;
pub mod external;
pub mod github;
pub mod link;
pub mod managed;
pub mod migrations;
pub mod repeat;
pub mod tag;
pub mod tag_settings;
pub mod task;
pub mod task_extra;
pub mod todoist;
pub mod testing;
pub mod tracing_setup;
pub mod trip;
pub mod workflow;

pub mod prelude {
    pub use crate::error::{self, QueryErr, QueryResult, StorageSetupErr};
    pub use crate::external::{ExternalComment, Integration, TagLink, TaskLink};
    pub use crate::github::{
        BoundRepo, DirtyWorktrees, FieldDecision, FieldMerge, GithubClient, GithubHttpClient,
        GithubSyncOutcome, GithubSyncSummary, ISSUE_FIELDS, IssueFieldState, IssueLink, IssuePage,
        IssuePatch, IssueRef, NewPullRequest, NewRunPullRequest, NewRunWorktree,
        PULL_REQUEST_CLOSED, PULL_REQUEST_MERGED, PULL_REQUEST_OPEN, PULL_REQUEST_WAIVED,
        RemoteIssue, RemoteIssueFields, RemotePullRequest, RemoteRepo, RunPullRequest, RunWorktree,
        SyncCursor, SyncFailure, SyncYield, TaskIssue, classify_status, comment_from_json,
        format_pull_request_body, format_pull_request_title, issue_external_id,
        parse_issue_external_id, plan_issue_merge, release_note_for, remote_issue_from_json,
        remote_pull_request_from_json, repos_from_json, resolve_field, summary_from_notes,
    };
    pub use crate::link::LinkKind;
    pub use crate::managed::{
        App, AppTagBinding, BindingRole, DEMO_APP_SLUG, ManagedIntegrityReport, ManagedMode,
        TRAVEL_APP_SLUG, TaskOwnership,
    };
    pub use crate::migrations::{MigrationEntry, MigrationError};
    pub use crate::repeat::RepeatTaskTemplate;
    pub use crate::tag::{Tag, TagId, TagNode, TagTreeNode, TagTreeRow};
    pub use crate::tag_settings::{SyncTarget, TagSection, TagSettings};
    pub use crate::todoist::SyncSummary;
    pub use crate::task::{Task, TaskWithMeta, factors_between};
    pub use crate::task_extra::{CODING_NAMESPACE, SPEC_KEY, TaskExtra};
    pub use crate::workflow::{
        CODING_PHASES, CoverageSummary, RECIPE_ROLES, Recipe, RecipeEdge, RecipeMeta, RecipeNode,
        RUN_NOTES_KEY, STEP_ROLE, RunNote, RunStepView, RunView, SubtaskCoverage, SubtaskSpecInput,
        WorkflowRecipe, WorkflowRun, coding_recipes, coverage_summary, is_step, issue_branch_name,
        issue_branch_slug, normalize_branch_name, run_notes, subtask_coverage,
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
    /// How many `with_transaction` frames are open. A nested call joins the
    /// transaction already in progress instead of issuing a second `BEGIN`,
    /// which SQLite rejects.
    pub(crate) transaction_depth: u32,
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
                workflow::WorkflowRun
            ))
            .build(driver)
            .await
            .context(error::DbBuildSnafu)?;

        let mut store = Self {
            db,
            transaction_depth: 0,
        };
        store.apply_pending_migrations().await?;
        store.ensure_builtin_apps().await?;
        // Report (never repair) any ownership rows left pointing at content
        // that no longer exists; SQLite does not enforce these references.
        store.log_managed_integrity().await;

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

use snafu::Snafu;

// -- Setup errors (init / migration) --

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum StorageSetupErr {
    #[snafu(display("failed to create Turso driver: {source}"))]
    TursoDriver { source: toasty::Error },

    #[snafu(display("failed to build database: {source}"))]
    DbBuild { source: toasty::Error },

    #[snafu(display("migration error: {source}"))]
    Migration {
        source: crate::migrations::MigrationError,
    },
}

// -- Query errors (runtime) --

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum QueryErr {
    // -- Tag operations --
    #[snafu(display("create tag '{name}': {source}"))]
    CreateTag { name: String, source: toasty::Error },

    #[snafu(display("get tag by id {id}: {source}"))]
    GetTag { id: u64, source: toasty::Error },

    #[snafu(display("find tag by name '{name}': {source}"))]
    FindTagByName { name: String, source: toasty::Error },

    #[snafu(display("delete tag {id}: {source}"))]
    DeleteTag { id: u64, source: toasty::Error },

    #[snafu(display("add tag implication {implier_id} -> {implied_id}: {source}"))]
    AddTagImplication {
        implier_id: u64,
        implied_id: u64,
        source: toasty::Error,
    },

    #[snafu(display("remove tag implication {implier_id} -> {implied_id}: {source}"))]
    RemoveTagImplication {
        implier_id: u64,
        implied_id: u64,
        source: toasty::Error,
    },

    #[snafu(display("assign tag '{tag_name}' to task {task_id}: {source}"))]
    AssignTagToTask {
        task_id: u64,
        tag_name: String,
        source: toasty::Error,
    },

    #[snafu(display("remove tag {tag_id} from task {task_id}: {source}"))]
    RemoveTagFromTask {
        task_id: u64,
        tag_id: u64,
        source: toasty::Error,
    },

    #[snafu(display("query tags ({context}): {source}"))]
    QueryTags {
        context: String,
        source: toasty::Error,
    },

    // -- Task operations --
    #[snafu(display("create task: {source}"))]
    CreateTask { source: toasty::Error },

    #[snafu(display("get task {id}: {source}"))]
    GetTask { id: u64, source: toasty::Error },

    #[snafu(display("delete task {id}: {source}"))]
    DeleteTask { id: u64, source: toasty::Error },

    #[snafu(display("update task {id}: {source}"))]
    UpdateTask { id: u64, source: toasty::Error },

    #[snafu(display("list tasks by priority: {source}"))]
    ListTasksByPriority { source: toasty::Error },

    #[snafu(display("list tasks by tag {tag_id}: {source}"))]
    ListTasksByTag { tag_id: u64, source: toasty::Error },

    #[snafu(display("load tags for task {task_id}: {source}"))]
    LoadTaskTags { task_id: u64, source: toasty::Error },

    // -- Task link operations --
    #[snafu(display("add task link {task_id} -> {other_id}: {source}"))]
    AddTaskLink {
        task_id: u64,
        other_id: u64,
        source: toasty::Error,
    },

    #[snafu(display("remove task link {task_id} -> {other_id}: {source}"))]
    RemoveTaskLink {
        task_id: u64,
        other_id: u64,
        source: toasty::Error,
    },

    #[snafu(display("link {task_id} -> {other_id} ({kind}) would close a dependency cycle"))]
    LinkCycle {
        task_id: u64,
        other_id: u64,
        kind: String,
    },

    // -- Generic fallback --
    #[snafu(display("database error: {source}"))]
    Database { source: toasty::Error },

    // -- Other --
    #[snafu(display("failed to parse timestamp: {source}"))]
    TimestampParse { source: jiff::Error },

    #[snafu(display("unexpected value in query result: {message}"))]
    UnexpectedValue { message: String },

    #[snafu(display("deserialization error: {source}"))]
    Deserialization { source: serde_json::Error },

    #[snafu(display("system time error: {source}"))]
    SystemTime { source: std::time::SystemTimeError },
}

pub type QueryResult<T, E = QueryErr> = std::result::Result<T, E>;

impl From<toasty::Error> for QueryErr {
    fn from(source: toasty::Error) -> Self {
        QueryErr::Database { source }
    }
}
impl From<jiff::Error> for QueryErr {
    fn from(source: jiff::Error) -> Self {
        QueryErr::TimestampParse { source }
    }
}
impl From<serde_json::Error> for QueryErr {
    fn from(source: serde_json::Error) -> Self {
        QueryErr::Deserialization { source }
    }
}
impl From<std::time::SystemTimeError> for QueryErr {
    fn from(source: std::time::SystemTimeError) -> Self {
        QueryErr::SystemTime { source }
    }
}

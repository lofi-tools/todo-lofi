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
    #[snafu(display("database error: {source}"))]
    Database { source: toasty::Error },

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

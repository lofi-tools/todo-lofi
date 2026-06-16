use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("database error: {source}"))]
    Database { source: toasty::Error },

    #[snafu(display("failed to create Turso driver: {source}"))]
    TursoDriver { source: toasty::Error },

    #[snafu(display("failed to build database: {source}"))]
    DbBuild { source: toasty::Error },

    #[snafu(display("failed to apply pending migrations: {source}"))]
    Migrations { source: toasty::Error },

    #[snafu(display("failed to load Toasty config: {source}"))]
    ToastyConfig { source: anyhow::Error },

    #[snafu(display("failed to load migration history: {source}"))]
    MigrationHistory { source: toasty::Error },

    #[snafu(display("failed to connect to database: {source}"))]
    DbConnect { source: toasty::Error },

    #[snafu(display("failed to get applied migrations: {source}"))]
    AppliedMigrations { source: toasty::Error },

    #[snafu(display("failed to read migration SQL {path}: {source}"))]
    MigrationSql {
        source: std::io::Error,
        path: String,
    },

    #[snafu(display("failed to parse timestamp: {source}"))]
    TimestampParse { source: jiff::Error },

    #[snafu(display("unexpected value in query result: {message}"))]
    UnexpectedValue { message: String },

    #[snafu(display("deserialization error: {source}"))]
    Deserialization { source: serde_json::Error },

    #[snafu(display("system time error: {source}"))]
    SystemTime { source: std::time::SystemTimeError },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<toasty::Error> for Error {
    fn from(source: toasty::Error) -> Self {
        Error::Database { source }
    }
}

impl From<jiff::Error> for Error {
    fn from(source: jiff::Error) -> Self {
        Error::TimestampParse { source }
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Error::Deserialization { source }
    }
}

impl From<std::time::SystemTimeError> for Error {
    fn from(source: std::time::SystemTimeError) -> Self {
        Error::SystemTime { source }
    }
}

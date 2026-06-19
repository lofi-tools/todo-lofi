use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum SymphonyError {
    /// Workflow file not found
    #[snafu(display("Workflow file not found: {}", path))]
    MissingWorkflowFile { path: String },

    /// Error parsing workflow file (YAML or Markdown)
    #[snafu(display("Failed to parse workflow file: {}", source))]
    WorkflowParseError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Workflow front matter is not a map/object
    #[snafu(display("Workflow front matter is not a map/object"))]
    WorkflowFrontMatterNotAMap,

    /// Error during prompt template parsing
    #[snafu(display("Template parse error: {}", source))]
    TemplateParseError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Error during prompt template rendering (unknown variable/filter)
    #[snafu(display("Template render error: {}", source))]
    TemplateRenderError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Tracker kind is not supported
    #[snafu(display("Unsupported tracker kind: {}", kind))]
    UnsupportedTrackerKind { kind: String },

    /// Missing tracker API key
    #[snafu(display("Missing tracker API key"))]
    MissingTrackerApiKey,

    /// Missing tracker project slug (required for Linear)
    #[snafu(display("Missing tracker project slug"))]
    MissingTrackerProjectSlug,

    /// Linear API request failed (transport/error)
    #[snafu(display("Linear API request failed: {}", source))]
    LinearApiRequest {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Linear API returned non-200 status
    #[snafu(display("Linear API status error: {} {}", status, body))]
    LinearApiStatus { status: u16, body: String },

    /// Linear GraphQL response contained errors
    #[snafu(display("Linear GraphQL errors: {:?}", errors))]
    LinearGraphqlErrors { errors: Vec<String> },

    /// Unknown or unexpected Linear API payload
    #[snafu(display("Linear unknown payload: {}", payload))]
    LinearUnknownPayload { payload: String },

    /// Missing end cursor in Linear pagination (integrity error)
    #[snafu(display("Linear missing end cursor in pagination"))]
    LinearMissingEndCursor,

    /// Configuration validation failed
    #[snafu(display("Configuration validation failed: {}", message))]
    ConfigValidation { message: String },

    /// Workspace path is outside allowed root
    #[snafu(display("Workspace path {} is outside allowed root {}", path, root))]
    InvalidWorkspacePath { path: String, root: String },

    /// Error creating or accessing workspace
    #[snafu(display("Workspace error: {}", source))]
    WorkspaceError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Error launching agent process
    #[snafu(display("Agent launch failed: {}", source))]
    AgentLaunchError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Error communicating with agent process
    #[snafu(display("Agent communication error: {}", source))]
    AgentCommunicationError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Agent process timed out
    #[snafu(display("Agent process timed out"))]
    AgentTimeout,

    /// Agent process stalled (no activity for too long)
    #[snafu(display("Agent process stalled"))]
    AgentStalled,

    /// Orchestrator internal error
    #[snafu(display("Orchestrator error: {}", message))]
    OrchestratorError { message: String },

    /// Generic catch-all error
    #[snafu(display("{}", message))]
    Generic { message: String },
}

/// Convenience type for Result with SymphonyError
pub type Result<T> = std::result::Result<T, SymphonyError>;

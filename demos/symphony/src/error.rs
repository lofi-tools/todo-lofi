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

    /// The coding-agent executable could not be launched
    #[snafu(display("Codex executable not found for command: {}", command))]
    CodexNotFound { command: String },

    /// The agent was asked to run outside of its per-issue workspace
    #[snafu(display("Invalid workspace cwd: {}", path))]
    InvalidWorkspaceCwd { path: String },

    /// A request/response exchange with the agent timed out
    #[snafu(display("Agent response timeout for {} after {} ms", method, timeout_ms))]
    ResponseTimeout { method: String, timeout_ms: u64 },

    /// A coding-agent turn exceeded `codex.turn_timeout_ms`
    #[snafu(display("Agent turn timeout after {} ms", timeout_ms))]
    TurnTimeout { timeout_ms: u64 },

    /// The agent subprocess exited unexpectedly
    #[snafu(display("Agent subprocess exited: {}", message))]
    PortExit { message: String },

    /// The agent returned a JSON-RPC error for a request
    #[snafu(display("Agent response error for {}: {}", method, message))]
    ResponseError { method: String, message: String },

    /// The agent reported a failed turn
    #[snafu(display("Agent turn failed: {}", message))]
    TurnFailed { message: String },

    /// The agent reported a cancelled turn
    #[snafu(display("Agent turn cancelled"))]
    TurnCancelled,

    /// The agent requested user input, which this implementation treats as a failure
    #[snafu(display("Agent turn requires user input: {}", prompt))]
    TurnInputRequired { prompt: String },

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

    /// The optional HTTP observability server could not be started
    #[snafu(display("HTTP server error: {}", message))]
    HttpServerError { message: String },

    /// Generic catch-all error
    #[snafu(display("{}", message))]
    Generic { message: String },
}

/// Convenience type for Result with SymphonyError
pub type Result<T> = std::result::Result<T, SymphonyError>;

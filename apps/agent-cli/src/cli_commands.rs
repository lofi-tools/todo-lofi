use clap::{Parser, Subcommand};

// ─── CLI definition ────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "abstract",
    about = "A high-performance AI coding agent",
    version,
    after_help = "Examples:\n  abstract                        Start interactive REPL\n  abstract \"fix the tests\"        Single-shot mode\n  abstract --resume               Resume last session\n  abstract --model opus --max     Use Opus with max thinking"
)]
pub struct Cli {
    /// Prompt to run in single-shot mode (omit for REPL)
    #[arg(short = 'p', long = "prompt", value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Resume a previous session
    #[arg(long, value_name = "SESSION_ID", num_args = 0..=1, default_missing_value = "last")]
    pub resume: Option<String>,

    /// Model to use (e.g., opus, sonnet, haiku, gpt-4o)
    #[arg(short, long)]
    pub model: Option<String>,

    /// Provider to use (anthropic, openai)
    #[arg(short = 'P', long)]
    pub provider: Option<String>,

    /// Fast mode (low effort, minimal thinking)
    #[arg(long, conflicts_with = "max")]
    pub fast: bool,

    /// Max mode (maximum thinking budget)
    #[arg(long, conflicts_with = "fast")]
    pub max: bool,

    /// Fallback models (comma-separated) for provider switching on error
    #[arg(long, value_delimiter = ',', value_name = "MODELS")]
    pub fallback: Vec<String>,

    /// Auto-approve all tool permissions (CI/headless mode)
    #[arg(long)]
    pub no_permissions: bool,

    /// Output events as NDJSON (for piping)
    #[arg(long)]
    pub json: bool,

    /// Enable verbose/debug logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Working directory override
    #[arg(short = 'C', long)]
    pub directory: Option<String>,

    /// Headless autonomous mode: task-focused prompt, auto-approve all tools, extended turns
    #[arg(long, alias = "benchmark")]
    pub headless: bool,

    /// Enable embedding API for semantic code search reranking (uses your LLM provider's embeddings)
    #[arg(long)]
    pub embedding_api: bool,

    /// Output format: text (default) or stream-json (NDJSON events)
    #[arg(long, value_name = "FORMAT")]
    pub output_format: Option<String>,

    /// Use a local proxy (VibeProxy or compatible) instead of direct API keys
    #[arg(long)]
    pub proxy: bool,

    /// Proxy URL (default: http://localhost:8317/v1)
    #[arg(long, value_name = "URL")]
    pub proxy_url: Option<String>,

    /// Compress tool outputs before they reach the LLM: off (default), minimal, or aggressive.
    #[arg(long, value_name = "LEVEL")]
    pub compress: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage sessions
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage memory
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Initialize project (.abstract/ directory)
    Init,
    /// Authenticate with a provider
    Login {
        /// Provider: claude, openai, key, status (default: interactive)
        provider: Option<String>,
    },
    /// Remove saved credentials
    Logout,
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// List all sessions
    #[command(alias = "ls")]
    List,
    /// Show a session transcript
    Show { id: String },
    /// Delete a session
    Rm { id: String },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show current configuration
    Show,
    /// Set a configuration value
    Set { key: String, value: String },
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Show memory status
    Show,
    /// Clear all memory
    Clear,
}

#[derive(Subcommand)]
pub enum McpAction {
    /// Add an MCP server
    Add {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// List configured MCP servers
    List,
    /// Remove an MCP server
    Remove { name: String },
}

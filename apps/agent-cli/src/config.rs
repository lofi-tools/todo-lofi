//! JSON configuration with layered loading.
//!
//! Priority (lowest → highest):
//! 1. Hardcoded defaults
//! 2. ~/.abstract/config.json   (user global)
//! 3. .abstract/config.json     (project local)
//! 4. Environment variables     (ABSTRACT_MODEL, etc.)
//! 5. CLI flags
//!
//! Legacy `.toml` files are still read when no `.json` file exists, so
//! existing configs keep working after the format switch.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::cli_commands::Cli;

// ─── Config structs ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub model: String,
    pub provider: String,
    pub max_turns: u32,
    pub max_tokens: u32,
    pub effort: String,
    pub output_style: String,
    pub theme: String,
    pub auto_compact: bool,
    pub graph_memory: bool,
    pub permissions_mode: String,
    pub working_dir: PathBuf,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// Fallback across providers on errors / rate limits.
    #[serde(default)]
    pub fallback: FallbackConfig,
    /// Per-provider overrides (base_url, api_key, models). Keys extend or
    /// override the built-in providers (poolside, openrouter, groq, nvidia,
    /// tokenrouter).
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfigEntry>,
    /// Named fallback "combos": exposed as the virtual provider `combos` with
    /// one virtual model per entry (e.g. `combos/coding`). Selecting one runs
    /// on the first listed (provider, model) and transparently retries across
    /// the rest on errors / rate limits.
    #[serde(default)]
    pub combos: std::collections::HashMap<String, Vec<ComboEntry>>,
    /// Only show free coding models by default (TUI picker and ACP
    /// `availableModels`). Set to `false` to show every configured model.
    #[serde(default = "default_true")]
    pub free_models_only: bool,
    #[serde(default)]
    pub benchmark_mode: bool,
    #[serde(default)]
    pub embedding_api: bool,
    #[serde(default = "default_output_format")]
    pub output_format: String,
    #[serde(default = "default_compression")]
    pub compression_level: String,
}

fn default_output_format() -> String {
    "text".into()
}
fn default_compression() -> String {
    "off".into()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: "auto".into(),
            provider: "auto".into(),
            max_turns: 50,
            max_tokens: 16384,
            effort: "medium".into(),
            output_style: "default".into(),
            theme: "dark".into(),
            auto_compact: true,
            graph_memory: true,
            permissions_mode: "interactive".into(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            fallback_models: Vec::new(),
            mcp_servers: Vec::new(),
            hooks: Vec::new(),
            proxy: ProxyConfig::default(),
            fallback: FallbackConfig::default(),
            providers: std::collections::HashMap::new(),
            combos: std::collections::HashMap::new(),
            free_models_only: true,
            benchmark_mode: false,
            embedding_api: false,
            output_format: "text".into(),
            compression_level: "off".into(),
        }
    }
}

/// Per-provider config-file entry. All fields optional: set only what you want
/// to override from the built-in defaults (or define a brand-new provider).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfigEntry {
    #[serde(default)]
    pub base_url: Option<String>,
    /// `!command` (shell output), `env:VAR`, or a literal key.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

/// Proxy configuration for routing through VibeProxy or similar local proxies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Enable proxy auto-detection.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Force proxy even when API keys are available (set by --proxy flag).
    #[serde(default)]
    pub force: bool,
    /// Proxy URL (default: http://localhost:8317/v1).
    #[serde(default = "default_proxy_url")]
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            force: false,
            url: default_proxy_url(),
        }
    }
}

/// Combo fallback tuning. Automatic fallback for individually picked models
/// was removed: fallback only happens inside a `combos` selection, where the
/// combo's own entry list defines the priority order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbackConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How long a combo entry stays excluded after a failure, in seconds.
    #[serde(default = "default_fallback_cooldown")]
    pub cooldown_seconds: u64,
    /// Where combo cooldowns are persisted so a rate-limited provider stays
    /// cooled down across restarts of the process. Defaults to
    /// `~/.abstract/cooldowns.json`; set to a path to relocate, or to an empty
    /// string to disable persistence.
    #[serde(default)]
    pub cooldowns_file: Option<PathBuf>,
}

impl Default for FallbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_seconds: default_fallback_cooldown(),
            cooldowns_file: None,
        }
    }
}

/// One (provider, model) entry inside a `combos` list. In JSON it can be
/// written either as a two-element array `["provider", "model"]` or as an
/// object `{ "provider": ..., "model": ... }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComboEntry {
    pub provider: String,
    pub model: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ComboEntryRepr {
    Tuple(String, String),
    Object { provider: String, model: String },
}

impl<'de> serde::Deserialize<'de> for ComboEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match ComboEntryRepr::deserialize(deserializer)? {
            ComboEntryRepr::Tuple(provider, model) => Ok(Self { provider, model }),
            ComboEntryRepr::Object { provider, model } => Ok(Self { provider, model }),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_proxy_url() -> String {
    "http://localhost:8317/v1".into()
}
fn default_fallback_cooldown() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    pub event: String,
    pub command: String,
}

// ─── Config directories ────────────────────────────────────────────────────

/// ~/.abstract/
pub fn global_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".abstract")
}

/// .abstract/ in the current project
pub fn project_config_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".abstract")
}

/// ~/.abstract/config.json
pub fn global_config_path() -> PathBuf {
    global_config_dir().join("config.json")
}

/// .abstract/config.json
pub fn project_config_path() -> PathBuf {
    project_config_dir().join("config.json")
}

// Legacy pre-JSON paths; still read when no `.json` file exists.
fn legacy_global_config_path() -> PathBuf {
    global_config_dir().join("config.toml")
}
fn legacy_project_config_path() -> PathBuf {
    project_config_dir().join("config.toml")
}

/// ~/.abstract/history
pub fn history_path() -> PathBuf {
    global_config_dir().join("history")
}

/// ~/.abstract/graph.db
pub fn graph_db_path() -> PathBuf {
    global_config_dir().join("graph.db")
}

/// ~/.abstract/cooldowns.json — combo cooldown state persisted across
/// restarts (override via `fallback.cooldowns_file`).
pub fn cooldowns_path() -> PathBuf {
    global_config_dir().join("cooldowns.json")
}

// ─── Loading ───────────────────────────────────────────────────────────────

/// Load config with layered merging.
pub fn load() -> AppConfig {
    let mut config = AppConfig::default();

    // Layer 2: global config (~/.abstract/config.json, legacy .toml fallback)
    if let Some(loaded) =
        load_json_file(&global_config_path()).or_else(|| load_toml_file(&legacy_global_config_path()))
    {
        merge(&mut config, loaded);
    }

    // Layer 3: project config (.abstract/config.json, legacy .toml fallback)
    if let Some(loaded) =
        load_json_file(&project_config_path()).or_else(|| load_toml_file(&legacy_project_config_path()))
    {
        merge(&mut config, loaded);
    }

    // Layer 4: environment variables
    apply_env(&mut config);

    config
}

fn load_json_file(path: &std::path::Path) -> Option<AppConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn load_toml_file(path: &std::path::Path) -> Option<AppConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

/// Overlay config-file values onto the defaults, only touching fields the
/// file explicitly set (i.e. fields that differ from the defaults).
fn merge(base: &mut AppConfig, overlay: AppConfig) {
    let defaults = AppConfig::default();
    macro_rules! copy_if_set {
        ($field:ident) => {
            if overlay.$field != defaults.$field {
                base.$field = overlay.$field;
            }
        };
    }
    copy_if_set!(model);
    copy_if_set!(provider);
    copy_if_set!(max_turns);
    copy_if_set!(max_tokens);
    copy_if_set!(effort);
    copy_if_set!(output_style);
    copy_if_set!(theme);
    copy_if_set!(auto_compact);
    copy_if_set!(graph_memory);
    copy_if_set!(permissions_mode);
    copy_if_set!(working_dir);
    copy_if_set!(output_format);
    copy_if_set!(compression_level);
    copy_if_set!(free_models_only);
    copy_if_set!(fallback);
    copy_if_set!(embedding_api);
    copy_if_set!(benchmark_mode);
    copy_if_set!(proxy);
    if !overlay.fallback_models.is_empty() {
        base.fallback_models = overlay.fallback_models;
    }
    if !overlay.mcp_servers.is_empty() {
        base.mcp_servers = overlay.mcp_servers;
    }
    if !overlay.hooks.is_empty() {
        base.hooks = overlay.hooks;
    }
    if !overlay.providers.is_empty() {
        base.providers = overlay.providers;
    }
    if !overlay.combos.is_empty() {
        base.combos = overlay.combos;
    }
}

fn apply_env(config: &mut AppConfig) {
    if let Ok(v) = std::env::var("ABSTRACT_MODEL") {
        config.model = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_PROVIDER") {
        config.provider = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_EFFORT") {
        config.effort = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_THEME") {
        config.theme = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_FALLBACK_MODELS") {
        config.fallback_models = v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Ok(v) = std::env::var("ABSTRACT_MAX_TURNS")
        && let Ok(n) = v.parse()
    {
        config.max_turns = n;
    }
    if let Ok(v) = std::env::var("ABSTRACT_COMPRESSION") {
        config.compression_level = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_FREE_MODELS_ONLY") {
        config.free_models_only = v == "1" || v.eq_ignore_ascii_case("true");
    }
}

pub fn apply_cli_overrides(cli: &Cli, config: &mut AppConfig) {
    if let Some(m) = &cli.model {
        config.model = resolve_model_alias(m);
        // Derive the provider from the model's prefix ("groq/compound",
        // "openrouter/...") unless the user picked a provider explicitly.
        if cli.provider.is_none()
            && let Some(prefix) = config.model.split('/').next()
            && !prefix.is_empty()
        {
            config.provider = prefix.to_string();
        }
    }
    if let Some(p) = &cli.provider {
        config.provider = p.clone();
    }
    if cli.fast {
        config.effort = "low".into();
    }
    if cli.max {
        config.effort = "max".into();
    }
    if cli.no_permissions {
        config.permissions_mode = "allow_all".into();
    }
    if let Some(dir) = &cli.directory {
        config.working_dir = std::path::PathBuf::from(dir);
    }
    if !cli.fallback.is_empty() {
        config.fallback_models = cli.fallback.clone();
    }
    if cli.proxy {
        config.proxy.enabled = true;
        config.proxy.force = true;
    }
    if cli.headless {
        config.benchmark_mode = true;
        config.permissions_mode = "allow_all".into();
        config.max_turns = 80;
    }
    if cli.embedding_api {
        config.embedding_api = true;
    }
    if let Some(fmt) = &cli.output_format {
        config.output_format = fmt.clone();
    }
    if let Some(url) = &cli.proxy_url {
        config.proxy.enabled = true;
        config.proxy.url = url.clone();
    }
    if let Some(lvl) = &cli.compress {
        config.compression_level = lvl.clone();
    }
}

fn resolve_model_alias(alias: &str) -> String {
    // Model ids are "provider/model". Providers that aren't built in are
    // reached through openrouter.
    match alias {
        "opus" => "openrouter/anthropic/claude-opus-4-6".into(),
        "sonnet" => "openrouter/anthropic/claude-sonnet-4-6".into(),
        "haiku" => "openrouter/anthropic/claude-haiku-4-5".into(),
        "gpt4o" | "4o" => "openrouter/openai/gpt-4o".into(),
        "gemini" => "openrouter/google/gemini-3.1-pro-preview".into(),
        "llama" => "groq/llama-3.1-70b-versatile".into(),
        "deepseek" => "openrouter/deepseek/deepseek-chat".into(),
        "grok" => "openrouter/x-ai/grok-2".into(),
        "mistral" => "openrouter/mistralai/mistral-large-latest".into(),
        other => other.into(),
    }
}

// /// Save config to a TOML file.
// pub fn save_to(config: &AppConfig, path: &Path) -> anyhow::Result<()> {
//     if let Some(parent) = path.parent() {
//         std::fs::create_dir_all(parent)?;
//     }
//     let content = toml::to_string_pretty(config)?;
//     std::fs::write(path, content)?;
//     Ok(())
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos_parse_tuple_and_object_forms() {
        let config: AppConfig = serde_json::from_str(
            r#"{
                "combos": {
                    "coding": [
                        ["poolside", "poolside/laguna-xs-2.1"],
                        { "provider": "openrouter", "model": "openrouter/free" }
                    ]
                }
            }"#,
        )
        .unwrap();
        let coding = config.combos.get("coding").unwrap();
        assert_eq!(coding.len(), 2);
        assert_eq!(coding[0].provider, "poolside");
        assert_eq!(coding[0].model, "poolside/laguna-xs-2.1");
        assert_eq!(coding[1].provider, "openrouter");
        assert_eq!(coding[1].model, "openrouter/free");
    }

    #[test]
    fn legacy_fallback_priority_is_ignored() {
        // Old configs with `fallback.priority` still load; the key is dropped
        // and the default cooldown applies.
        let config: AppConfig = serde_json::from_str(
            r#"{
                "fallback": {
                    "enabled": false,
                    "priority": ["poolside", "openrouter"]
                }
            }"#,
        )
        .unwrap();
        assert!(!config.fallback.enabled);
        assert_eq!(config.fallback.cooldown_seconds, default_fallback_cooldown());
    }
}
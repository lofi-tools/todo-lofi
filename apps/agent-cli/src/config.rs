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

use anyhow::Context as _;
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
    /// Per-model response format families: model id (e.g. "stealth/ox-alpha")
    /// → family name ("reasoning", "reasoning_content", "plain"). The family
    /// determines how a model's streamed thinking is delimited from its
    /// answer text — see `crate::response_format::family`.
    #[serde(default)]
    pub model_families: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub benchmark_mode: bool,
    #[serde(default)]
    pub embedding_api: bool,
    #[serde(default = "default_output_format")]
    pub output_format: String,
    #[serde(default = "default_compression")]
    pub compression_level: String,
    /// Extra environment variables made available to agent tools (e.g. the
    /// WebSearch tool's `TINYFISH_API_KEY` / `LANGSEARCH_API_KEY` — its
    /// first provider, Parallel Search via MCP, needs no key). Keys are env
    /// var names; values use the same spec format as `providers.*.api_key`:
    /// `!command` (run shell, trimmed stdout), `env:VAR` (copy another
    /// variable), or a literal value. An env var already set in the
    /// environment wins; the config value is only a fallback.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
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
            mcp_servers: Vec::new(),
            hooks: Vec::new(),
            proxy: ProxyConfig::default(),
            fallback: FallbackConfig::default(),
            providers: std::collections::HashMap::new(),
            combos: std::collections::HashMap::new(),
            free_models_only: true,
            model_families: std::collections::HashMap::new(),
            benchmark_mode: false,
            embedding_api: false,
            output_format: "text".into(),
            compression_level: "off".into(),
            env: std::collections::HashMap::new(),
        }
    }
}

// ─── Default-config template ────────────────────────────────────────────────

/// Render every config field with its default value as pretty-printed JSONC
/// (JSON with `//` comments). Each comment documents the field's possible
/// values; the output parses with `serde_json_lenient`, so it can be saved
/// directly as `config.json`.
pub fn default_config_jsonc() -> String {
    let defaults = AppConfig::default();
    let value = serde_json::to_value(&defaults).expect("default config is serializable");
    let obj = value.as_object().expect("config serializes to an object");
    let mut out = String::from("{\n");
    // (field, possible values). Order mirrors the struct declaration.
    let fields: &[(&str, &str)] = &[
        ("model", "Model id or alias (\"auto\", \"opus\", \"sonnet\", \"haiku\", \"gpt4o\", \"gemini\", \"llama\", \"deepseek\", \"grok\", \"mistral\", or \"provider/model\")"),
        ("provider", "Provider (\"auto\", \"poolside\", \"openrouter\", \"groq\", \"nvidia\", \"tokenrouter\", \"combos\", or a name from `providers`)"),
        ("max_turns", "Maximum agent turns per run (u32)"),
        ("max_tokens", "Maximum output tokens per response (u32)"),
        ("effort", "Thinking effort: \"low\", \"medium\", \"high\", \"max\""),
        ("output_style", "Answer style: \"default\""),
        ("theme", "TUI theme: \"enterprise\", \"light\", \"solarized\""),
        ("auto_compact", "Auto-compact context near the limit (true/false)"),
        ("graph_memory", "Enable memory graph (true/false)"),
        ("permissions_mode", "Tool permission mode: \"interactive\", \"allow_all\""),
        ("working_dir", "Working directory (path)"),
        ("mcp_servers", "MCP servers: [{ \"name\", \"command\", \"args\", \"env\" }]"),
        ("hooks", "Lifecycle hooks: [{ \"event\", \"command\" }]"),
        ("proxy", "Proxy routing (VibeProxy or compatible)"),
        ("fallback", "Combo fallback tuning"),
        ("providers", "Per-provider overrides (base_url, api_key, models)"),
        ("combos", "Named fallback combos: { name: [[\"provider\", \"model\"], ...] }"),
        ("free_models_only", "Only show free coding models in pickers (true/false)"),
        ("model_families", "Model id → response format family: \"reasoning\", \"reasoning_content\", \"plain\""),
        ("benchmark_mode", "Benchmark/headless mode (true/false)"),
        ("embedding_api", "Enable embedding API for semantic search (true/false)"),
        ("output_format", "Output format: \"text\", \"stream-json\""),
        ("compression_level", "Tool output compression: \"off\", \"minimal\", \"aggressive\""),
        ("env", "Extra environment variables for agent tools: { name: \"!cmd | env:VAR | literal\" }"),
    ];
    for (i, (key, comment)) in fields.iter().enumerate() {
        let field_value = obj.get(*key).expect("serialized config has every field");
        out.push_str(&format!("  // {comment}\n"));
        out.push_str(&format!("  \"{key}\": "));
        match *key {
            "proxy" => append_nested_jsonc(
                &mut out,
                field_value,
                &[
                    ("enabled", "Auto-detect the proxy (true/false)"),
                    ("force", "Force the proxy even when API keys are present (true/false)"),
                    ("url", "Proxy base URL (default http://localhost:8317/v1)"),
                ],
            ),
            "fallback" => append_nested_jsonc(
                &mut out,
                field_value,
                &[
                    ("enabled", "Enable combo fallback (true/false)"),
                    ("cooldown_seconds", "Cooldown after a failed entry, in seconds (u64)"),
                    ("cooldowns_file", "Cooldown persistence path, or null to disable (path | null)"),
                ],
            ),
            "providers" => append_providers_jsonc(&mut out),
            "env" => append_env_jsonc(&mut out),
            _ => out.push_str(&pretty_indented(field_value, 2)),
        }
        out.push_str(if i + 1 < fields.len() { "," } else { "" });
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

/// Append `value` as a JSONC object with a comment line above each subfield.
fn append_nested_jsonc(out: &mut String, value: &serde_json::Value, subfields: &[(&str, &str)]) {
    let obj = value.as_object().expect("nested config field is an object");
    out.push_str("{\n");
    for (i, (key, comment)) in subfields.iter().enumerate() {
        let field_value = obj.get(*key).expect("nested field present");
        out.push_str(&format!("    // {comment}\n"));
        out.push_str(&format!("    \"{key}\": "));
        out.push_str(&pretty_indented(field_value, 4));
        out.push_str(if i + 1 < subfields.len() { "," } else { "" });
        out.push('\n');
    }
    out.push_str("  }");
}

/// Render the default `providers` object: the built-in providers as
/// config-file entries (one model each), so the template shows the real
/// defaults a user would edit instead of an empty `{}`.
fn append_providers_jsonc(out: &mut String) {
    out.push_str("{\n");
    out.push_str("    // The built-in providers, each with one model. Listing\n");
    out.push_str("    // `models` takes full control of the list (free-model\n");
    out.push_str("    // filtering no longer applies to it). API keys resolve\n");
    out.push_str("    // from the `env` vars below or the shell environment.\n");
    let entries = crate::providers::builtin_provider_entries();
    let len = entries.len();
    for (i, (name, entry)) in entries.into_iter().enumerate() {
        let value = serde_json::to_value(&entry).expect("provider entry serializes");
        out.push_str(&format!("    \"{name}\": "));
        out.push_str(&pretty_indented(&value, 4));
        out.push_str(if i + 1 < len { "," } else { "" });
        out.push('\n');
    }
    out.push_str("  }");
}

/// Render the default `env` object: one explicit field per env var the agent
/// needs, each defaulting to a `!echo VAR` placeholder spec (documents the
/// variable without failing; a variable already set in the shell still wins
/// at startup).
fn append_env_jsonc(out: &mut String) {
    out.push_str("{\n");
    out.push_str("    // Environment variables for agent tools. Each entry is a\n");
    out.push_str("    // fallback: a variable already set in your shell wins; the\n");
    out.push_str("    // config value only fills in when it isn't. The `!echo VAR`\n");
    out.push_str("    // defaults are placeholders — replace them with a real\n");
    out.push_str("    // source (\"!cat ~/.key\", a literal, …).\n");
    out.push_str("    // Provider API keys (resolved via the `env:` specs in `providers`).\n");
    out.push_str("    \"POOLSIDE_API_KEY\": \"!echo POOLSIDE_API_KEY\",\n");
    out.push_str("    \"OPENROUTER_API_KEY\": \"!echo OPENROUTER_API_KEY\",\n");
    out.push_str("    \"GROQ_API_KEY\": \"!echo GROQ_API_KEY\",\n");
    out.push_str("    \"NVIDIA_API_KEY\": \"!echo NVIDIA_API_KEY\",\n");
    out.push_str("    \"TOKENROUTER_API_KEY\": \"!echo TOKENROUTER_API_KEY\",\n");
    out.push_str("    // WebSearch keys (Parallel Search via MCP needs no key).\n");
    out.push_str("    \"TINYFISH_API_KEY\": \"!echo TINYFISH_API_KEY\",\n");
    out.push_str("    \"LANGSEARCH_API_KEY\": \"!echo LANGSEARCH_API_KEY\"\n");
    out.push_str("  }");
}

/// Pretty-print `value`, indenting continuation lines by `base_indent` spaces
/// so they align under the key that introduced the value.
fn pretty_indented(value: &serde_json::Value, base_indent: usize) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    if !pretty.contains('\n') {
        return pretty;
    }
    let pad = " ".repeat(base_indent);
    let mut lines = pretty.lines();
    let mut out = String::from(lines.next().unwrap_or_default());
    for line in lines {
        out.push('\n');
        out.push_str(&pad);
        out.push_str(line);
    }
    out
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
    // Lenient parser: accepts `//` comments and trailing commas, so a
    // commented default-config template can be saved as config.json.
    serde_json_lenient::from_str(&content).ok()
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
    if !overlay.model_families.is_empty() {
        base.model_families = overlay.model_families;
    }
    if !overlay.env.is_empty() {
        base.env = overlay.env;
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

/// Resolve each entry of the config `env` map (spec format like api_key:
/// `!command`, `env:VAR`, or a literal) and set it in the process
/// environment, so agent tools that read env vars (e.g. the WebSearch tool's
/// `TINYFISH_API_KEY` / `LANGSEARCH_API_KEY`) can find them.
///
/// Precedence: a variable already set in the environment wins as-is; the
/// config value only fills in when the variable is not already set.
pub fn apply_config_env(config: &AppConfig) -> anyhow::Result<()> {
    for (name, spec) in &config.env {
        // An existing env var takes priority; skip resolution entirely so a
        // configured `!command` isn't run when its value would be unused.
        if std::env::var_os(name).is_some() {
            continue;
        }
        let value = crate::providers::resolve_value_spec(spec, "env")
            .with_context(|| format!("failed to resolve config env var '{name}'"))?;
        // SAFETY: called once at startup on the main thread before agent
        // threads spawn; matches the existing `set_var` usage in this crate.
        unsafe { std::env::set_var(name, value) };
    }
    Ok(())
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
    fn model_families_merge_into_defaults() {
        let mut config = AppConfig::default();
        let overlay: AppConfig = serde_json::from_str(
            r#"{
                "model_families": {
                    "stealth/ox-alpha": "reasoning",
                    "deepseek/deepseek-chat": "reasoning_content",
                    "nvidia/nemotron-3-ultra-550b-a55b": "reasoning_content"
                }
            }"#,
        )
        .unwrap();
        merge(&mut config, overlay);
        assert_eq!(
            config.model_families.get("stealth/ox-alpha").map(String::as_str),
            Some("reasoning")
        );
        assert_eq!(
            config.model_families.get("deepseek/deepseek-chat").map(String::as_str),
            Some("reasoning_content")
        );
        assert_eq!(
            config.model_families.get("nvidia/nemotron-3-ultra-550b-a55b").map(String::as_str),
            Some("reasoning_content")
        );
        // An empty map leaves existing families untouched.
        let mut config2 = config.clone();
        merge(&mut config2, AppConfig::default());
        assert_eq!(config2.model_families.len(), 3);
    }

    #[test]
    fn default_config_jsonc_lists_all_fields_with_comments() {
        let jsonc = default_config_jsonc();
        // Every config field appears with its default value.
        for (key, value) in [
            ("model", "auto"),
            ("provider", "auto"),
            ("max_turns", "50"),
            ("max_tokens", "16384"),
            ("effort", "medium"),
            ("theme", "dark"),
            ("permissions_mode", "interactive"),
            ("free_models_only", "true"),
        ] {
            assert!(
                jsonc.contains(&format!("\"{key}\": \"{value}\""))
                    || jsonc.contains(&format!("\"{key}\": {value}")),
                "missing {key} = {value} in:\n{jsonc}"
            );
        }
        // Possible-value comments sit above the fields.
        assert!(jsonc.contains("// Model id or alias"));
        assert!(jsonc.contains("// Thinking effort"));
        assert!(jsonc.contains("// Tool output compression"));
        // Nested proxy fields carry their own comments.
        assert!(jsonc.contains("// Auto-detect the proxy"));
        assert!(jsonc.contains("// Proxy base URL"));
    }

    #[test]
    fn default_config_jsonc_round_trips_through_lenient_parser() {
        let jsonc = default_config_jsonc();
        // serde_json_lenient accepts the `//` comments; the strict parser
        // rejects them, proving the file is JSONC, not plain JSON.
        let parsed: AppConfig = serde_json_lenient::from_str(&jsonc).unwrap();
        let defaults = AppConfig::default();
        assert_eq!(parsed.model, defaults.model);
        assert_eq!(parsed.provider, defaults.provider);
        assert_eq!(parsed.max_turns, defaults.max_turns);
        assert_eq!(parsed.max_tokens, defaults.max_tokens);
        assert_eq!(parsed.effort, defaults.effort);
        assert_eq!(parsed.proxy.enabled, defaults.proxy.enabled);
        assert_eq!(parsed.proxy.url, defaults.proxy.url);
        assert_eq!(parsed.fallback.cooldown_seconds, defaults.fallback.cooldown_seconds);
        // The providers/env sections are template content (the runtime
        // defaults keep empty maps), so assert the rendered entries parse
        // back: the five default providers and seven necessary env vars.
        assert_eq!(parsed.providers.len(), 5);
        assert_eq!(parsed.env.len(), 7);
        assert!(serde_json::from_str::<AppConfig>(&jsonc).is_err());
    }

    #[test]
    fn default_config_jsonc_lists_default_providers_and_env_vars() {
        let jsonc = default_config_jsonc();
        // Each default provider appears with its base_url, env: key spec,
        // and at least one model.
        for (name, key) in [
            ("poolside", "POOLSIDE_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("groq", "GROQ_API_KEY"),
            ("nvidia", "NVIDIA_API_KEY"),
            ("tokenrouter", "TOKENROUTER_API_KEY"),
        ] {
            assert!(
                jsonc.contains(&format!("    \"{name}\": {{")),
                "missing provider {name} in:\n{jsonc}"
            );
            assert!(
                jsonc.contains(&format!("\"api_key\": \"env:{key}\"")),
                "missing env: key spec for {name} in:\n{jsonc}"
            );
        }
        // Every env var the agent needs gets an explicit field defaulting to
        // a `!echo VAR` placeholder spec.
        for name in [
            "POOLSIDE_API_KEY",
            "OPENROUTER_API_KEY",
            "GROQ_API_KEY",
            "NVIDIA_API_KEY",
            "TOKENROUTER_API_KEY",
            "TINYFISH_API_KEY",
            "LANGSEARCH_API_KEY",
        ] {
            assert!(
                jsonc.contains(&format!("\"{name}\": \"!echo {name}\"")),
                "missing env placeholder for {name} in:\n{jsonc}"
            );
        }
    }

    #[test]
    fn env_map_merges_into_defaults() {
        let mut config = AppConfig::default();
        let overlay: AppConfig = serde_json::from_str(
            r#"{
                "env": {
                    "TINYFISH_API_KEY": "!cat ~/.tinyfish_key",
                    "LANGSEARCH_API_KEY": "!cat ~/.langsearch_key",
                    "MY_LITERAL": "literal-value"
                }
            }"#,
        )
        .unwrap();
        merge(&mut config, overlay);
        assert_eq!(config.env.get("TINYFISH_API_KEY").map(String::as_str), Some("!cat ~/.tinyfish_key"));
        assert_eq!(config.env.get("LANGSEARCH_API_KEY").map(String::as_str), Some("!cat ~/.langsearch_key"));
        assert_eq!(config.env.get("MY_LITERAL").map(String::as_str), Some("literal-value"));
        // An empty map leaves existing entries untouched.
        let mut config2 = config.clone();
        merge(&mut config2, AppConfig::default());
        assert_eq!(config2.env.len(), 3);
        // The default config template shows both web search key examples.
        let jsonc = default_config_jsonc();
        assert!(jsonc.contains("TINYFISH_API_KEY"));
        assert!(jsonc.contains("LANGSEARCH_API_KEY"));
    }

    #[test]
    fn apply_config_env_resolves_specs_into_process_env() {
        // Source for the `env:VAR` copy form — read from the real process
        // environment, not from sibling entries (HashMap order is arbitrary).
        unsafe { std::env::set_var("TEST_ABSTRACT_SOURCE", "source-value") };
        // A var already set in the environment must win over the config
        // value (fallback only fills in when nothing is set).
        unsafe { std::env::set_var("TEST_ABSTRACT_PRECEDENCE", "from-env") };
        let config: AppConfig = serde_json::from_str(
            r#"{
                "env": {
                    "TEST_ABSTRACT_LITERAL": "literal-value",
                    "TEST_ABSTRACT_CMD": "!echo cmd-value",
                    "TEST_ABSTRACT_COPY": "env:TEST_ABSTRACT_SOURCE",
                    "TEST_ABSTRACT_PRECEDENCE": "from-config"
                }
            }"#,
        )
        .unwrap();
        apply_config_env(&config).unwrap();
        assert_eq!(std::env::var("TEST_ABSTRACT_LITERAL").unwrap(), "literal-value");
        assert_eq!(std::env::var("TEST_ABSTRACT_CMD").unwrap(), "cmd-value");
        assert_eq!(std::env::var("TEST_ABSTRACT_COPY").unwrap(), "source-value");
        // Real env var beats the config fallback.
        assert_eq!(std::env::var("TEST_ABSTRACT_PRECEDENCE").unwrap(), "from-env");
        // A failing spec surfaces as an error instead of being swallowed.
        let bad: AppConfig = serde_json::from_str(
            r#"{ "env": { "TEST_ABSTRACT_BAD": "!exit 1" } }"#,
        )
        .unwrap();
        assert!(apply_config_env(&bad).is_err());
        let missing: AppConfig = serde_json::from_str(
            r#"{ "env": { "TEST_ABSTRACT_MISSING": "env:TEST_ABSTRACT_DOES_NOT_EXIST_XYZ" } }"#,
        )
        .unwrap();
        assert!(apply_config_env(&missing).is_err());
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
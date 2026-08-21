//! Provider registry: built-in providers, config-file overrides, API key
//! resolution, and shared agent construction.
//!
//! Built-in providers (all OpenAI-compatible): poolside, openrouter, groq,
//! nvidia nim, and the tokenrouter gateway. Their base URL / api key / models
//! can be overridden — or new providers added — via the config file's
//! `[providers.NAME]` section.
//!
//! An `api_key` value in config is either:
//! - `!command` — run the rest as a shell command and use its trimmed stdout,
//! - `env:VAR` — read the environment variable,
//! - a literal key.

use crate::config::AppConfig;
use anyhow::Context as _;
use cersei::tools::permissions::{AllowAll, AllowReadOnly};
use cersei::types::{Message, Role};
use cersei::{Agent, OpenAi};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// A configured provider (built-in defaults merged with the config file).
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    /// API key spec: `!command`, `env:VAR`, or a literal key.
    pub api_key: String,
    /// Subset of `models` that are free coding models. When
    /// `free_models_only` is enabled and this list is non-empty, `models` is
    /// filtered down to it. Empty means "unknown" (user-defined providers,
    /// or an explicit `models` override) → all models are shown.
    pub free_models: Vec<String>,
    pub models: Vec<String>,
}

/// The full tool set for the agent: cersei's coding tools (file
/// read/write/edit, glob/grep, bash, web fetch/search — the pi.dev-style
/// basics) plus the `ReadDocs` (Context7 library docs), `SyntheticOutput`
/// (structured output), `spawn_agents` (freebuff-style sub-agents), and
/// `suggest_followups` tools — matching the freebuff agent's tool surface.
pub fn agent_tools(
    resolved: &Resolved,
    parent: crate::subagents::ParentHandle,
    followups: crate::subagents::FollowupSink,
    events: crate::subagents::SubAgentEventSink,
    readonly: bool,
) -> Vec<Box<dyn cersei::tools::Tool>> {
    let mut tools = cersei::tools::coding();
    // Replace the built-in Grep with our ripgrep version (raw `rg` flag
    // passthrough, per-file and global result caps — freebuff-style).
    tools.retain(|t| t.name() != "Grep");
    tools.push(Box::new(crate::tools::RgSearchTool));
    tools.push(Box::new(crate::tools::ReadDocsTool));
    tools.push(Box::new(cersei::tools::synthetic_output::SyntheticOutputTool));
    // write_todos tracking, used by the phase workflow in the system prompt.
    tools.push(Box::new(cersei::tools::todo_write::TodoWriteTool));
    tools.push(Box::new(crate::subagents::SuggestFollowupsTool::new(followups)));
    // Read-only sessions (ACP readonly mode) can't spawn sub-agents: the
    // sub-agents run with AllowAll and could modify files.
    if !readonly {
        tools.push(Box::new(crate::subagents::SpawnAgentsTool::new(
            resolved.clone(),
            parent,
            events,
        )));
    }
    tools
}

/// A provider/model selection with the API key already resolved.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}

/// Parameters shared by all agent builders (TUI and ACP sessions).
pub struct BuildParams {
    pub working_dir: PathBuf,
    pub max_turns: u32,
    pub session_id: Option<String>,
    pub messages: Vec<Message>,
    pub cancel_token: CancellationToken,
    /// Use a read-only permission policy (denies modifying/executing tools).
    pub readonly: bool,
    /// Filled with a weak handle to the built agent, so sub-agents with
    /// `include_message_history` can inherit the parent conversation.
    pub parent: crate::subagents::ParentHandle,
    /// Sink where the `suggest_followups` tool stores suggestions.
    pub followups: crate::subagents::FollowupSink,
    /// Broadcast channel that sub-agent activity is forwarded to (the TUI
    /// subscribes to render nested tool calls). None in headless/ACP mode.
    pub subagent_events: crate::subagents::SubAgentEventSink,
}

// ─── Provider registry ──────────────────────────────────────────────────────

fn builtin_providers() -> Vec<Provider> {
    vec![
        Provider {
            name: "poolside".into(),
            base_url: "https://inference.poolside.ai/v1".into(),
            api_key: "env:POOLSIDE_API_KEY".into(),
            // Laguna is poolside's coding model family; both are free.
            free_models: vec![
                "poolside/laguna-xs-2.1".into(),
                "poolside/laguna-s-2.1".into(),
            ],
            models: vec![
                "poolside/laguna-xs-2.1".into(),
                "poolside/laguna-s-2.1".into(),
            ],
        },
        Provider {
            name: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "env:OPENROUTER_API_KEY".into(),
            // Free coding models on openrouter (zero-priced `:free` variants).
            free_models: vec![
                "openrouter/free".into(),
                "openai/gpt-oss-20b:free".into(),
                "cohere/north-mini-code:free".into(),
                "poolside/laguna-xs-2.1:free".into(),
            ],
            models: vec![
                "openrouter/auto".into(),
                "openrouter/free".into(),
                "openrouter/fusion".into(),
                "google/gemini-3.7-flash".into(),
                "deepseek/deepseek-v4-pro-0813".into(),
                "openai/gpt-oss-20b:free".into(),
                "cohere/north-mini-code:free".into(),
                "poolside/laguna-xs-2.1:free".into(),
            ],
        },
        Provider {
            name: "groq".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            api_key: "env:GROQ_API_KEY".into(),
            // groq/compound and compound-mini are free; the gpt-oss models
            // hosted on groq are paid.
            free_models: vec![
                "groq/compound".into(),
                "groq/compound-mini".into(),
            ],
            models: vec![
                "groq/compound".into(),
                "groq/compound-mini".into(),
                "openai/gpt-oss-120b".into(),
                "openai/gpt-oss-20b".into(),
            ],
        },
        Provider {
            name: "nvidia".into(),
            base_url: "https://integrate.api.nvidia.com/v1".into(),
            api_key: "env:NVIDIA_API_KEY".into(),
            // NVIDIA NIM offers free serverless APIs for development; all
            // built-in models are available through the free tier.
            free_models: vec![
                "nvidia/llama-3.3-nemotron-super-49b-v1".into(),
                "meta/llama-3.3-70b-instruct".into(),
                "deepseek-ai/deepseek-r1".into(),
                "qwen/qwen2.5-72b-instruct".into(),
            ],
            models: vec![
                "nvidia/llama-3.3-nemotron-super-49b-v1".into(),
                "meta/llama-3.3-70b-instruct".into(),
                "deepseek-ai/deepseek-r1".into(),
                "qwen/qwen2.5-72b-instruct".into(),
            ],
        },
        Provider {
            name: "tokenrouter".into(),
            base_url: "https://api.tokenrouter.com/v1".into(),
            api_key: "env:TOKENROUTER_API_KEY".into(),
            // TokenRouter is a unified gateway; this is a curated subset of its
            // coding-capable chat models (all exposed over the OpenAI-compatible
            // endpoint). The `-free` variants are zero-ratio (free) on the
            // gateway.
            free_models: vec![
                "deepseek/deepseek-v4-pro-0813-free".into(),
                "qwen/qwen3.8-max-free".into(),
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free".into(),
            ],
            models: vec![
                "deepseek/deepseek-v4-pro-0813-free".into(),
                "qwen/qwen3.8-max-free".into(),
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free".into(),
                "deepseek/deepseek-v4-pro-0813".into(),
                "deepseek/deepseek-v4-flash".into(),
                "qwen/qwen3-coder-next".into(),
                "openai/gpt-oss-120b".into(),
                "moonshotai/kimi-k2.7-code".into(),
                "mistralai/devstral-2512".into(),
                "z-ai/glm-5.2".into(),
                "x-ai/grok-4.5".into(),
            ],
        },
    ]
}

/// A configured combo: a named fallback list of (provider, model) pairs,
/// selectable as the virtual provider `combos` / model `<name>`.
#[derive(Debug, Clone)]
pub struct Combo {
    pub name: String,
    pub entries: Vec<crate::config::ComboEntry>,
}

/// Whether `name` names a real provider: a built-in or a config-file entry
/// (the virtual `combos` provider itself doesn't count — combo entries must
/// name concrete providers). Used by `combos` for validation — it must not go
/// through `provider`/`providers`, which call back into `combos`.
fn is_known_provider(config: &AppConfig, name: &str) -> bool {
    (builtin_names().iter().any(|n| n == name) || config.providers.contains_key(name))
        && name != "combos"
}

/// All combos defined in config, in config order. Entries naming an unknown
/// provider are skipped with a warning; a combo with no valid entries is
/// dropped entirely.
pub fn combos(config: &AppConfig) -> Vec<Combo> {
    let mut out = Vec::new();
    for (name, entries) in &config.combos {
        let mut valid = Vec::new();
        for entry in entries {
            if is_known_provider(config, &entry.provider) {
                valid.push(entry.clone());
            } else {
                eprintln!(
                    "warning: combo '{name}' references unknown provider '{}' — entry skipped",
                    entry.provider
                );
            }
        }
        if !valid.is_empty() {
            out.push(Combo {
                name: name.clone(),
                entries: valid,
            });
        } else {
            eprintln!("warning: combo '{name}' has no valid entries — dropped");
        }
    }
    out
}

/// The named combo, or None if no such combo is configured.
pub fn combo(config: &AppConfig, name: &str) -> Option<Combo> {
    combos(config).into_iter().find(|c| c.name == name)
}

/// The first entry of a combo — the (provider, model) a `combos/<name>`
/// selection actually starts on.
pub fn combo_first_entry(config: &AppConfig, name: &str) -> Option<crate::config::ComboEntry> {
    combo(config, name).and_then(|c| c.entries.into_iter().next())
}

/// The concrete (provider, model) an agent built for a selection runs on: for
/// the virtual `combos` provider this is the first entry of the named combo;
/// otherwise it's the selection itself.
pub fn effective_selection(
    config: &AppConfig,
    provider: &str,
    model: &str,
) -> anyhow::Result<(String, String)> {
    if provider == "combos" {
        let entry = combo_first_entry(config, model)
            .ok_or_else(|| anyhow::anyhow!("unknown combo '{model}'"))?;
        Ok((entry.provider, entry.model))
    } else {
        Ok((provider.to_string(), model.to_string()))
    }
}

/// The fallback manager for a selection: combo selections get the combo's
/// entries in order; any other selection gets no fallback at all (automatic
/// cross-provider fallback was removed).
pub fn fallback_for(config: &AppConfig, provider: &str, model: &str) -> FallbackManager {
    let entries: Vec<FallbackEntry> = if provider == "combos" {
        combo(config, model)
            .map(|c| {
                c.entries
                    .into_iter()
                    .map(|e| FallbackEntry {
                        provider: e.provider,
                        model: e.model,
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    FallbackManager::new(config, entries)
}

/// All providers in deterministic order: built-ins first, then the virtual
/// `combos` provider, then config-file additions (sorted). Config entries
/// override built-in fields by name.
pub fn providers(config: &AppConfig) -> Vec<Provider> {
    let mut by_name: HashMap<String, Provider> = builtin_providers()
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();

    // Virtual "combos" provider: one model per configured combo. It is never
    // resolved directly — `resolve` maps it to the combo's first entry.
    let combo_names = combos(config);
    if !combo_names.is_empty() {
        by_name.insert(
            "combos".into(),
            Provider {
                name: "combos".into(),
                base_url: String::new(),
                api_key: String::new(),
                free_models: Vec::new(),
                models: combo_names.into_iter().map(|c| c.name).collect(),
            },
        );
    }

    for (name, entry) in &config.providers {
        let provider = by_name.entry(name.clone()).or_insert_with(|| Provider {
            name: name.clone(),
            base_url: String::new(),
            api_key: String::new(),
            free_models: Vec::new(),
            models: Vec::new(),
        });
        if let Some(base_url) = &entry.base_url {
            provider.base_url = base_url.clone();
        }
        if let Some(api_key) = &entry.api_key {
            provider.api_key = api_key.clone();
        }
        if !entry.models.is_empty() {
            // An explicit `models` override takes full control of the list:
            // free-model filtering no longer applies to it.
            provider.models = entry.models.clone();
            provider.free_models.clear();
        }
    }

    let mut all: Vec<Provider> = by_name.into_values().collect();
    all.sort_by_key(|p| {
        // Built-ins first, then the virtual combos provider, then config-file
        // additions (alphabetical).
        let builtin_rank = builtin_names()
            .iter()
            .position(|n| n == &p.name)
            .or_else(|| (p.name == "combos").then(|| builtin_names().len()))
            .unwrap_or(usize::MAX);
        (builtin_rank, p.name.clone())
    });

    if config.free_models_only {
        for p in &mut all {
            if !p.free_models.is_empty() {
                p.models.retain(|m| p.free_models.contains(m));
            }
        }
    }
    all
}

fn builtin_names() -> Vec<String> {
    builtin_providers().into_iter().map(|p| p.name).collect()
}

/// Look up a single provider by name.
pub fn provider(config: &AppConfig, name: &str) -> Option<Provider> {
    providers(config).into_iter().find(|p| p.name == name)
}

/// Resolve the provider/model that the config points at ("auto" defaults to
/// poolside and the provider's first model; a bare `model` like "groq/compound"
/// implies its provider).
pub fn default_selection(config: &AppConfig) -> anyhow::Result<(String, String)> {
    let all = providers(config);
    let known: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();

    let mut provider = config.provider.trim().to_string();
    let mut model = config.model.trim().to_string();

    if provider.is_empty() || provider == "auto" {
        provider = match model.split('/').next().filter(|prefix| known.contains(prefix)) {
            Some(prefix) => prefix.to_string(),
            None => all
                .first()
                .map(|p| p.name.clone())
                .ok_or_else(|| anyhow::anyhow!("no providers configured"))?,
        };
    }
    if model.is_empty() || model == "auto" {
        model = default_model(config, &provider)?;
    }
    Ok((provider, model))
}

/// The first configured model of a provider.
pub fn default_model(config: &AppConfig, provider_name: &str) -> anyhow::Result<String> {
    let p = provider(config, provider_name)
        .ok_or_else(|| anyhow::anyhow!("unknown provider: {provider_name}"))?;
    p.models
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider '{provider_name}' has no models configured"))
}

/// User-facing "provider/model" id. Model ids are wire-level ids which may
/// already carry the provider prefix (e.g. "poolside/laguna-xs-2.1" or
/// "openrouter/auto") or not (e.g. "google/gemini-3.7-flash"), so only add
/// the prefix when it isn't already there.
pub fn display_model_id(provider: &str, model: &str) -> String {
    if model.starts_with(&format!("{provider}/")) {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

/// All (provider, model) entries, deterministic order.
pub fn entries(config: &AppConfig) -> Vec<(String, String)> {
    providers(config)
        .into_iter()
        .flat_map(|p| {
            p.models
                .into_iter()
                .map(move |model| (p.name.clone(), model))
        })
        .collect()
}

// ─── API key resolution ─────────────────────────────────────────────────────

pub fn resolve_api_key(spec: &str) -> anyhow::Result<String> {
    if let Some(cmd) = spec.strip_prefix('!') {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            anyhow::bail!("empty api_key command in config");
        }
        let output = std::process::Command::new("sh")
            .args(["-c", cmd])
            .output()
            .with_context(|| format!("failed to run api_key command: {cmd}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "api_key command failed ({status}): {stderr}",
                status = output.status,
                stderr = String::from_utf8_lossy(&output.stderr).trim(),
            );
        }
        let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if key.is_empty() {
            anyhow::bail!("api_key command produced no output: {cmd}");
        }
        Ok(key)
    } else if let Some(var) = spec.strip_prefix("env:") {
        std::env::var(var.trim())
            .with_context(|| format!("environment variable '{var}' is not set (used for api_key)"))
    } else {
        Ok(spec.to_string())
    }
}

/// Resolve a provider + model into concrete base URL and API key. The virtual
/// `combos` provider resolves to the first entry of the named combo.
pub fn resolve(config: &AppConfig, provider_name: &str, model: &str) -> anyhow::Result<Resolved> {
    if provider_name == "combos" {
        let entry = combo_first_entry(config, model)
            .ok_or_else(|| anyhow::anyhow!("unknown combo '{model}'"))?;
        return resolve(config, &entry.provider, &entry.model);
    }
    let p = provider(config, provider_name).ok_or_else(|| {
        let known = providers(config)
            .into_iter()
            .map(|p| p.name)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!("unknown provider '{provider_name}'; known providers: {known}")
    })?;
    let api_key = resolve_api_key(&p.api_key)
        .with_context(|| format!("resolving api_key for provider '{provider_name}'"))?;
    Ok(Resolved {
        provider: p.name,
        model: model.to_string(),
        base_url: p.base_url,
        api_key,
    })
}

/// Drop the trailing user message left behind by a failed run (the prompt that
/// `run_stream` pushed) so a retry can re-seed the conversation and re-push the
/// prompt exactly once. Only safe to call when the failed run produced no
/// output, in which case the trailing user message is guaranteed to be the
/// prompt.
pub fn drop_trailing_user_message(messages: &mut Vec<Message>) {
    if matches!(messages.last(), Some(m) if m.role == Role::User) {
        messages.pop();
    }
}

/// One concrete (provider, model) target a combo can fall back to.
#[derive(Debug, Clone)]
pub struct FallbackEntry {
    pub provider: String,
    pub model: String,
}

/// Tracks which combo entries are in a failure cooldown and the order in
/// which a combo should try them. Cloning shares the cooldown state, so
/// per-run clones keep failures recorded by earlier runs of the same combo.
#[derive(Clone)]
pub struct FallbackManager {
    enabled: bool,
    /// Entries in priority order (most preferred first).
    priority: Vec<FallbackEntry>,
    cooldown: Duration,
    /// "provider\0model" → cooldown expiry.
    failures: Arc<Mutex<HashMap<String, Instant>>>,
}

impl FallbackManager {
    /// `entries` is the combo's fallback list in order. An empty list (a
    /// non-combo selection) means fallback is disabled.
    pub fn new(config: &AppConfig, entries: Vec<FallbackEntry>) -> Self {
        Self {
            enabled: config.fallback.enabled && !entries.is_empty(),
            priority: entries,
            cooldown: Duration::from_secs(config.fallback.cooldown_seconds),
            failures: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn key(provider: &str, model: &str) -> String {
        format!("{provider}\0{model}")
    }

    /// Mark an entry as failed; it won't be selected for fallback again until
    /// the cooldown expires.
    pub fn record_failure(&self, provider: &str, model: &str) {
        self.failures.lock().insert(
            Self::key(provider, model),
            Instant::now() + self.cooldown,
        );
    }

    /// The most preferred entry to fall back to after `(provider, model)`
    /// failed, skipping the current entry and any still cooling down.
    pub fn next_entry(&self, provider: &str, model: &str) -> Option<FallbackEntry> {
        let now = Instant::now();
        let mut failures = self.failures.lock();
        failures.retain(|_, until| *until > now);
        self.priority.iter().find(|e| {
            (e.provider != provider || e.model != model)
                && !failures.contains_key(&Self::key(&e.provider, &e.model))
        })
        .cloned()
    }
}

/// Resolve a user-facing reference like "groq", "openrouter/auto", or a bare
/// model name into a concrete (provider, model) pair. Used by `/model <text>`
/// and ACP `session/set_config_option`.
pub fn resolve_selection(
    config: &AppConfig,
    _current_provider: &str,
    text: &str,
) -> anyhow::Result<(String, String)> {
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("empty provider/model reference");
    }
    let all = providers(config);

    // Exact provider name → that provider's default model.
    if let Some(p) = all.iter().find(|p| p.name == text) {
        let model = p
            .models
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("provider '{text}' has no models configured"))?;
        return Ok((p.name.clone(), model));
    }

    // "{provider}/{model...}" — match the provider prefix.
    for p in &all {
        if let Some(rest) = text.strip_prefix(&format!("{}/", p.name)) {
            for m in &p.models {
                if m == rest || m == text {
                    return Ok((p.name.clone(), m.clone()));
                }
            }
            return Err(anyhow::anyhow!(
                "unknown model '{rest}' for provider '{}' (available: {})",
                p.name,
                p.models.join(", "),
            ));
        }
    }

    // Bare model name — match against every provider's models.
    for p in &all {
        for m in &p.models {
            if m == text
                || m.strip_prefix(&format!("{}/", p.name)).is_some_and(|rest| rest == text)
            {
                return Ok((p.name.clone(), m.clone()));
            }
        }
    }

    Err(anyhow::anyhow!("unknown provider or model: '{text}'"))
}

// ─── Agent construction ─────────────────────────────────────────────────────

pub fn build_agent(resolved: &Resolved, params: BuildParams) -> anyhow::Result<Arc<Agent>> {
    let provider = OpenAi::builder()
        .base_url(&resolved.base_url)
        .api_key(&resolved.api_key)
        .model(&resolved.model)
        .build()
        .context("failed to build provider")?;

    let tools = agent_tools(
        resolved,
        params.parent.clone(),
        params.followups.clone(),
        params.subagent_events.clone(),
        params.readonly,
    );
    let mut builder = Agent::builder()
        .provider(provider)
        .model(&resolved.model)
        .max_turns(params.max_turns)
        .working_dir(params.working_dir)
        .cancel_token(params.cancel_token)
        .with_messages(params.messages)
        .tools(tools);
    if let Some(session_id) = params.session_id {
        builder = builder.session_id(session_id);
    }
    builder = if params.readonly {
        builder.permission_policy(AllowReadOnly)
    } else {
        // The spawner system prompt lists the sub-agents and when to spawn
        // them (freebuff's prompt-encoded workflow). Read-only sessions skip
        // it since they can't spawn sub-agents.
        builder
            .permission_policy(AllowAll)
            .system_prompt(crate::subagents::spawner_system_prompt())
    };

    let agent = Arc::new(builder.build().context("failed to build agent")?);
    // Make the built agent visible to sub-agents that inherit history.
    *params.parent.lock() = Some(Arc::downgrade(&agent));
    Ok(agent)
}

// ─── Runtime agent holder (TUI) ─────────────────────────────────────────────

/// Holds the live agent and lets the TUI switch provider/model at runtime.
pub struct AgentRuntime {
    inner: Mutex<AgentRuntimeInner>,
}

struct AgentRuntimeInner {
    agent: Arc<Agent>,
    /// User-facing selection (provider, model). For the `combos` provider the
    /// model is the combo name; the agent underneath runs on `effective_*`.
    provider: String,
    model: String,
    /// The concrete (provider, model) the current agent actually runs on.
    effective_provider: String,
    effective_model: String,
    config: AppConfig,
    parent: crate::subagents::ParentHandle,
    followups: crate::subagents::FollowupSink,
    /// Sender side of the sub-agent activity channel; rebuilt agents (fallback
    /// / switch) keep using the same channel so TUI receivers stay valid.
    subagent_tx: tokio::sync::broadcast::Sender<crate::subagents::SubAgentActivity>,
    /// Fallback state for the current selection (a combo, or empty = disabled).
    fallback: FallbackManager,
}

impl AgentRuntime {
    pub fn new(config: &AppConfig) -> anyhow::Result<Self> {
        let (provider, model) = default_selection(config)?;
        let (effective_provider, effective_model) = effective_selection(config, &provider, &model)?;
        let resolved = resolve(config, &effective_provider, &effective_model)?;
        let parent = Arc::new(Mutex::new(None));
        let followups = Arc::new(Mutex::new(Vec::new()));
        let (subagent_tx, _) = tokio::sync::broadcast::channel(1024);
        let agent = build_agent(
            &resolved,
            BuildParams {
                working_dir: config.working_dir.clone(),
                max_turns: config.max_turns,
                session_id: None,
                messages: Vec::new(),
                cancel_token: CancellationToken::new(),
                readonly: false,
                parent: parent.clone(),
                followups: followups.clone(),
                subagent_events: Some(subagent_tx.clone()),
            },
        )?;
        let fallback = fallback_for(config, &provider, &model);
        Ok(Self {
            inner: Mutex::new(AgentRuntimeInner {
                agent,
                provider,
                model,
                effective_provider,
                effective_model,
                config: config.clone(),
                parent,
                followups,
                subagent_tx,
                fallback,
            }),
        })
    }

    pub fn agent(&self) -> Arc<Agent> {
        self.inner.lock().agent.clone()
    }

    /// Subscribe to the sub-agent activity stream (rendered by the TUI as
    /// nested tool calls under each `spawn_agents` call).
    pub fn subscribe_subagents(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::subagents::SubAgentActivity> {
        self.inner.lock().subagent_tx.subscribe()
    }

    /// Drain the followup suggestions collected by the `suggest_followups`
    /// tool during the last run.
    pub fn take_followups(&self) -> Vec<crate::subagents::Followup> {
        std::mem::take(&mut *self.inner.lock().followups.lock())
    }

    pub fn current(&self) -> (String, String) {
        let g = self.inner.lock();
        (g.provider.clone(), g.model.clone())
    }

    /// The concrete (provider, model) the current agent runs on — the combo's
    /// current fallback entry when the selection is a combo.
    pub fn effective(&self) -> (String, String) {
        let g = self.inner.lock();
        (g.effective_provider.clone(), g.effective_model.clone())
    }

    /// All (provider, model) entries for the picker.
    pub fn entries(&self) -> Vec<(String, String)> {
        entries(&self.inner.lock().config)
    }

    /// Resolve a `/model <text>` reference against the runtime's config.
    pub fn select_text(&self, text: &str) -> anyhow::Result<(String, String)> {
        let config = &self.inner.lock().config;
        resolve_selection(config, "", text)
    }

    // ── Combo fallback ──────────────────────────────────────────────────

    /// Whether the current selection falls back across combo entries (only
    /// true while a combo is selected and `fallback.enabled` is on).
    pub fn fallback_enabled(&self) -> bool {
        self.inner.lock().fallback.enabled()
    }

    /// The next combo entry to fall back to after `(provider, model)` failed,
    /// or None if every other entry is cooling down.
    pub fn next_fallback_entry(&self, provider: &str, model: &str) -> Option<FallbackEntry> {
        self.inner.lock().fallback.next_entry(provider, model)
    }

    pub fn record_failure(&self, provider: &str, model: &str) {
        self.inner.lock().fallback.record_failure(provider, model);
    }

    /// Rebuild the agent on a different (provider, model) combo entry,
    /// preserving the conversation (minus the failed run's just-pushed prompt)
    /// and the user-facing selection (the combo stays selected).
    pub fn fallback_to(&self, provider: &str, model: &str) -> anyhow::Result<()> {
        let (config, working_dir, max_turns, parent, followups, subagent_tx) = {
            let g = self.inner.lock();
            (
                g.config.clone(),
                g.config.working_dir.clone(),
                g.config.max_turns,
                g.parent.clone(),
                g.followups.clone(),
                g.subagent_tx.clone(),
            )
        };
        let resolved = resolve(&config, provider, model)?;
        let mut messages = self.inner.lock().agent.messages();
        drop_trailing_user_message(&mut messages);
        let agent = build_agent(
            &resolved,
            BuildParams {
                working_dir,
                max_turns,
                session_id: None,
                messages,
                cancel_token: CancellationToken::new(),
                readonly: false,
                parent: parent.clone(),
                followups: followups.clone(),
                subagent_events: Some(subagent_tx.clone()),
            },
        )?;
        let mut g = self.inner.lock();
        g.agent = agent;
        g.effective_provider = provider.to_string();
        g.effective_model = model.to_string();
        Ok(())
    }

    /// Rebuild the agent with a new provider/model.
    pub fn switch(&self, provider: &str, model: &str) -> anyhow::Result<()> {
        let (config, working_dir, max_turns, parent, followups, subagent_tx) = {
            let g = self.inner.lock();
            (
                g.config.clone(),
                g.config.working_dir.clone(),
                g.config.max_turns,
                g.parent.clone(),
                g.followups.clone(),
                g.subagent_tx.clone(),
            )
        };
        let (effective_provider, effective_model) =
            effective_selection(&config, provider, model)?;
        let resolved = resolve(&config, &effective_provider, &effective_model)?;
        let agent = build_agent(
            &resolved,
            BuildParams {
                working_dir,
                max_turns,
                session_id: None,
                messages: Vec::new(),
                cancel_token: CancellationToken::new(),
                readonly: false,
                parent: parent.clone(),
                followups: followups.clone(),
                subagent_events: Some(subagent_tx.clone()),
            },
        )?;
        let fallback = fallback_for(&config, provider, model);
        let mut g = self.inner.lock();
        g.agent = agent;
        g.provider = provider.to_string();
        g.model = model.to_string();
        g.effective_provider = effective_provider;
        g.effective_model = effective_model;
        g.fallback = fallback;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ProviderConfigEntry};

    #[test]
    fn resolve_api_key_shell_command() {
        assert_eq!(resolve_api_key("!echo test-key").unwrap(), "test-key");
        assert!(resolve_api_key("!exit 1").is_err());
    }

    #[test]
    fn resolve_api_key_env() {
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("ABSTRACT_TEST_KEY", "env-key") };
        assert_eq!(resolve_api_key("env:ABSTRACT_TEST_KEY").unwrap(), "env-key");
        assert!(resolve_api_key("env:ABSTRACT_TEST_MISSING_XYZ").is_err());
    }

    #[test]
    fn resolve_api_key_literal() {
        assert_eq!(resolve_api_key("sk-literal-123").unwrap(), "sk-literal-123");
    }

    #[test]
    fn default_selection_derives_provider_from_model() {
        let mut config = AppConfig::default();
        config.model = "groq/compound".into();
        let (provider, model) = default_selection(&config).unwrap();
        assert_eq!(provider, "groq");
        assert_eq!(model, "groq/compound");
    }

    #[test]
    fn default_selection_auto_uses_poolside() {
        let config = AppConfig::default(); // provider/model are "auto"
        let (provider, model) = default_selection(&config).unwrap();
        assert_eq!(provider, "poolside");
        assert_eq!(model, "poolside/laguna-xs-2.1");
    }

    #[test]
    fn resolve_selection_variants() {
        let config = AppConfig::default();
        assert_eq!(
            resolve_selection(&config, "", "groq").unwrap(),
            ("groq".to_string(), "groq/compound".to_string())
        );
        assert_eq!(
            resolve_selection(&config, "", "openrouter/openai/gpt-oss-20b:free").unwrap(),
            ("openrouter".to_string(), "openai/gpt-oss-20b:free".to_string())
        );
        assert_eq!(
            resolve_selection(&config, "", "groq/groq/compound").unwrap(),
            ("groq".to_string(), "groq/compound".to_string())
        );
        assert_eq!(
            resolve_selection(&config, "", "laguna-xs-2.1").unwrap(),
            ("poolside".to_string(), "poolside/laguna-xs-2.1".to_string())
        );
        assert!(resolve_selection(&config, "", "unknown-provider/model").is_err());
    }

    #[test]
    fn config_file_provider_is_merged() {
        let mut config = AppConfig::default();
        config.providers.insert(
            "acme".to_string(),
            ProviderConfigEntry {
                base_url: Some("https://acme.example/v1".into()),
                api_key: Some("!echo acme-key".into()),
                models: vec!["acme/big".into(), "acme/small".into()],
            },
        );
        // Custom provider is listed and resolved.
        let resolved = resolve(&config, "acme", "acme/big").unwrap();
        assert_eq!(resolved.base_url, "https://acme.example/v1");
        assert_eq!(resolved.api_key, "acme-key");
        // Built-ins are still present.
        assert!(provider(&config, "poolside").is_some());
        assert!(provider(&config, "openrouter").is_some());
    }

    #[test]
    fn free_models_only_filters_builtins_by_default() {
        let config = AppConfig::default(); // free_models_only: true
        let openrouter = provider(&config, "openrouter").unwrap();
        assert_eq!(
            openrouter.models,
            vec![
                "openrouter/free",
                "openai/gpt-oss-20b:free",
                "cohere/north-mini-code:free",
                "poolside/laguna-xs-2.1:free",
            ]
        );
        // gpt-oss and the paid frontier models are hidden from the default view.
        let groq = provider(&config, "groq").unwrap();
        assert_eq!(groq.models, vec!["groq/compound", "groq/compound-mini"]);
        assert!(!groq.models.iter().any(|m| m.contains("gpt-oss")));
    }

    #[test]
    fn free_models_only_can_be_disabled() {
        let mut config = AppConfig::default();
        config.free_models_only = false;
        let openrouter = provider(&config, "openrouter").unwrap();
        assert!(openrouter.models.contains(&"openrouter/auto".to_string()));
        assert!(openrouter.models.contains(&"deepseek/deepseek-v4-pro-0813".to_string()));
    }

    #[test]
    fn tokenrouter_builtin_provider() {
        let config = AppConfig::default();
        let tr = provider(&config, "tokenrouter").unwrap();
        assert_eq!(tr.base_url, "https://api.tokenrouter.com/v1");
        assert_eq!(tr.api_key, "env:TOKENROUTER_API_KEY");
        // With the free filter on (default), only the free gateway models show.
        assert_eq!(
            tr.models,
            vec![
                "deepseek/deepseek-v4-pro-0813-free",
                "qwen/qwen3.8-max-free",
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
            ]
        );
        // The paid coding models appear once the filter is off.
        let mut config = AppConfig::default();
        config.free_models_only = false;
        let tr = provider(&config, "tokenrouter").unwrap();
        assert!(tr.models.contains(&"deepseek/deepseek-v4-pro-0813".to_string()));
        assert!(tr.models.contains(&"qwen/qwen3-coder-next".to_string()));
        assert!(tr.models.contains(&"openai/gpt-oss-120b".to_string()));
        // Resolution wires up the gateway base URL.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("TOKENROUTER_API_KEY", "tr-test-key") };
        let resolved = resolve(&config, "tokenrouter", "qwen/qwen3-coder-next").unwrap();
        assert_eq!(resolved.base_url, "https://api.tokenrouter.com/v1");
        assert_eq!(resolved.model, "qwen/qwen3-coder-next");
        assert_eq!(resolved.api_key, "tr-test-key");
    }

    #[test]
    fn fallback_priority_and_cooldown() {
        let config = AppConfig::default();
        let entries = vec![
            FallbackEntry {
                provider: "poolside".into(),
                model: "poolside/laguna-xs-2.1".into(),
            },
            FallbackEntry {
                provider: "openrouter".into(),
                model: "openrouter/free".into(),
            },
            FallbackEntry {
                provider: "groq".into(),
                model: "groq/compound".into(),
            },
        ];
        let fb = FallbackManager::new(&config, entries);
        assert!(fb.enabled());
        // Priority is the entry list order, current entry excluded.
        let next = fb.next_entry("poolside", "poolside/laguna-xs-2.1").unwrap();
        assert_eq!((next.provider.as_str(), next.model.as_str()), ("openrouter", "openrouter/free"));
        fb.record_failure("openrouter", "openrouter/free");
        let next = fb.next_entry("poolside", "poolside/laguna-xs-2.1").unwrap();
        assert_eq!((next.provider.as_str(), next.model.as_str()), ("groq", "groq/compound"));
        fb.record_failure("groq", "groq/compound");
        // All alternates cooling down → nothing left to fall back to.
        assert!(fb.next_entry("poolside", "poolside/laguna-xs-2.1").is_none());
    }

    #[test]
    fn fallback_can_be_disabled() {
        let mut config = AppConfig::default();
        config.fallback.enabled = false;
        let entries = vec![FallbackEntry {
            provider: "groq".into(),
            model: "groq/compound".into(),
        }];
        let fb = FallbackManager::new(&config, entries);
        assert!(!fb.enabled());
    }

    #[test]
    fn fallback_skips_current_entry() {
        let config = AppConfig::default();
        let entries = vec![
            FallbackEntry {
                provider: "groq".into(),
                model: "groq/compound".into(),
            },
            FallbackEntry {
                provider: "nvidia".into(),
                model: "nvidia/llama-3.3-nemotron-super-49b-v1".into(),
            },
        ];
        let fb = FallbackManager::new(&config, entries);
        // The current entry itself is never re-selected, even if preferred.
        let next = fb.next_entry("groq", "groq/compound").unwrap();
        assert_eq!((next.provider.as_str(), next.model.as_str()), ("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1"));
        // After nvidia fails too, groq is tried again (it never cooled down).
        let next = fb.next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1").unwrap();
        assert_eq!((next.provider.as_str(), next.model.as_str()), ("groq", "groq/compound"));
        // With groq cooling down and nvidia current, nothing is left.
        fb.record_failure("groq", "groq/compound");
        assert!(fb.next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1").is_none());
    }

    #[test]
    fn plain_selection_has_no_fallback() {
        let config = AppConfig::default();
        let fb = fallback_for(&config, "groq", "groq/compound");
        assert!(!fb.enabled());
        assert!(fb.next_entry("groq", "groq/compound").is_none());
    }

    #[test]
    fn combos_expose_virtual_provider_and_resolve() {
        use crate::config::ComboEntry;
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("GROQ_API_KEY", "combo-test-key") };
        let mut config = AppConfig::default();
        config.combos.insert(
            "coding".into(),
            vec![
                ComboEntry {
                    provider: "groq".into(),
                    model: "groq/compound".into(),
                },
                ComboEntry {
                    provider: "nvidia".into(),
                    model: "nvidia/llama-3.3-nemotron-super-49b-v1".into(),
                },
            ],
        );
        // The virtual provider shows up with one model per combo.
        let combos_provider = provider(&config, "combos").unwrap();
        assert_eq!(combos_provider.models, vec!["coding"]);
        // Resolution maps the combo to its first entry.
        let resolved = resolve(&config, "combos", "coding").unwrap();
        assert_eq!(resolved.provider, "groq");
        assert_eq!(resolved.model, "groq/compound");
        // effective_selection agrees; fallback_for is enabled for the combo.
        assert_eq!(
            effective_selection(&config, "combos", "coding").unwrap(),
            ("groq".to_string(), "groq/compound".to_string())
        );
        let fb = fallback_for(&config, "combos", "coding");
        assert!(fb.enabled());
        let next = fb.next_entry("groq", "groq/compound").unwrap();
        assert_eq!(next.provider, "nvidia");
    }

    #[test]
    fn combo_entries_validate_providers() {
        use crate::config::ComboEntry;
        let mut config = AppConfig::default();
        config.combos.insert(
            "mixed".into(),
            vec![
                ComboEntry {
                    provider: "groq".into(),
                    model: "groq/compound".into(),
                },
                ComboEntry {
                    provider: "no-such-provider".into(),
                    model: "x/y".into(),
                },
            ],
        );
        let combos = combos(&config);
        assert_eq!(combos.len(), 1);
        assert_eq!(combos[0].entries.len(), 1);
        assert_eq!(combos[0].entries[0].provider, "groq");
    }

    #[test]
    fn combo_default_selection_resolves() {
        use crate::config::ComboEntry;
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("GROQ_API_KEY", "combo-test-key") };
        let mut config = AppConfig::default();
        config.combos.insert(
            "coding".into(),
            vec![ComboEntry {
                provider: "groq".into(),
                model: "groq/compound".into(),
            }],
        );
        config.provider = "combos".into();
        config.model = "coding".into();
        let (provider, model) = default_selection(&config).unwrap();
        assert_eq!((provider.as_str(), model.as_str()), ("combos", "coding"));
        let resolved = resolve(&config, &provider, &model).unwrap();
        assert_eq!(resolved.model, "groq/compound");
        // /model-style resolution of "combos/coding" works too.
        assert_eq!(
            resolve_selection(&config, "", "combos/coding").unwrap(),
            ("combos".to_string(), "coding".to_string())
        );
    }

    #[test]
    fn drop_trailing_user_message_strips_prompt() {
        let mut messages = vec![
            Message::user("first prompt"),
            Message::assistant("a reply"),
            Message::user("second prompt"),
        ];
        drop_trailing_user_message(&mut messages);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages.last().unwrap().role, Role::Assistant);
        // Empty / already-stripped conversations are left alone.
        let mut empty = Vec::new();
        drop_trailing_user_message(&mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn explicit_models_override_skips_free_filter() {
        let mut config = AppConfig::default();
        config.providers.insert(
            "openrouter".to_string(),
            ProviderConfigEntry {
                base_url: None,
                api_key: None,
                models: vec!["openrouter/auto".into()],
            },
        );
        // A config-file `models` list is shown verbatim, even when the free
        // filter is on.
        let openrouter = provider(&config, "openrouter").unwrap();
        assert_eq!(openrouter.models, vec!["openrouter/auto"]);
    }

    // ─── End-to-end agent run against a mock provider ────────────────────
    //
    // These tests drive the real agent loop (tools, permissions, streaming)
    // against a local fake OpenAI-compatible server, so they need no network
    // or API key: the mock answers the first request with a `Write` tool call
    // and every later request with plain text.

    /// A fake OpenAI chat-completions SSE server that walks the agent through
    /// one tool call. Returns the base URL to point a provider at.
    async fn mock_openai_server(file_path: &str, content: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let file_path = file_path.to_string();
        let content = content.to_string();

        tokio::spawn(async move {
            let mut request_count = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                request_count += 1;
                let first_request = request_count == 1;
                let file_path = file_path.clone();
                let content = content.clone();
                tokio::spawn(async move {
                    // Read the full request (headers + body). The body is not
                    // inspected, but must be drained so the client can read
                    // the response.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(end) = find_header_end(&buf) else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..end]);
                        let content_length = head.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        });
                        match content_length {
                            // Fixed-length body: wait until it has arrived.
                            Some(len) if buf.len() >= end + 4 + len => break,
                            // Chunked body: wait for the terminating chunk.
                            Some(_) => continue,
                            None if buf.windows(5).any(|w| w == b"0\r\n\r\n") => break,
                            None => continue,
                        }
                    }

                    let body = if first_request {
                        // Turn 1: ask the agent to Write the file.
                        let args = serde_json::json!({ "file_path": file_path, "content": content });
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({
                                "id": "chatcmpl-1",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": {
                                        "tool_calls": [{
                                            "index": 0,
                                            "id": "call_1",
                                            "type": "function",
                                            "function": {
                                                "name": "Write",
                                                "arguments": args.to_string(),
                                            }
                                        }]
                                    },
                                    "finish_reason": null
                                }]
                            }),
                            serde_json::json!({
                                "id": "chatcmpl-1",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": {},
                                    "finish_reason": "tool_calls"
                                }]
                            }),
                        )
                    } else {
                        // Turn 2+: reply with plain text to end the run.
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({
                                "id": "chatcmpl-2",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": { "content": "Done: wrote the requested file." },
                                    "finish_reason": null
                                }]
                            }),
                            serde_json::json!({
                                "id": "chatcmpl-2",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": {},
                                    "finish_reason": "stop"
                                }]
                            }),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    // Closing the connection terminates the SSE body.
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{addr}")
    }

    /// A fake OpenAI-compatible SSE server that answers the first request with
    /// a `spawn_agents` tool call (one researcher-web sub-agent) and every
    /// later request with plain text — drives both the parent's tool loop and
    /// the spawned sub-agent (which uses the same resolved provider).
    async fn spawn_mock_server() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let mut request_count = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                request_count += 1;
                let first_request = request_count == 1;
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(end) = find_header_end(&buf) else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..end]);
                        let content_length = head.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        });
                        match content_length {
                            Some(len) if buf.len() >= end + 4 + len => break,
                            Some(_) => continue,
                            None if buf.windows(5).any(|w| w == b"0\r\n\r\n") => break,
                            None => continue,
                        }
                    }

                    let body = if first_request {
                        let args = serde_json::json!({
                            "agents": [{
                                "agent_type": "researcher-web",
                                "prompt": "What is the answer?"
                            }]
                        });
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({
                                "id": "chatcmpl-spawn",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": {
                                        "tool_calls": [{
                                            "index": 0,
                                            "id": "call_spawn",
                                            "type": "function",
                                            "function": {
                                                "name": "spawn_agents",
                                                "arguments": args.to_string(),
                                            }
                                        }]
                                    },
                                    "finish_reason": null
                                }]
                            }),
                            serde_json::json!({
                                "id": "chatcmpl-spawn",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": {},
                                    "finish_reason": "tool_calls"
                                }]
                            }),
                        )
                    } else {
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({
                                "id": "chatcmpl-sub",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": { "content": "The answer is 42." },
                                    "finish_reason": null
                                }]
                            }),
                            serde_json::json!({
                                "id": "chatcmpl-sub",
                                "object": "chat.completion.chunk",
                                "choices": [{
                                    "index": 0,
                                    "delta": {},
                                    "finish_reason": "stop"
                                }]
                            }),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{addr}")
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    /// Headless single-shot run: build an agent pointed at the mock provider,
    /// ask it to write a test file into the repo root, and assert the file was
    /// actually written by the agent's tools.
    #[tokio::test]
    async fn agent_writes_test_file_to_repo_root() {
        use cersei::events::AgentEvent;
        use std::time::Duration;

        // Scratch repo: the "root of the repo" the agent operates in.
        let dir = std::env::temp_dir().join(format!("agent-smoke-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("agent_test.txt");

        let base_url =
            mock_openai_server(file_path.to_str().unwrap(), "hello from the agent").await;

        let mut config = AppConfig::default();
        config.provider = "mock".into();
        config.model = "mock/test-model".into();
        config.working_dir = dir.clone();
        config.permissions_mode = "allow_all".into();
        config.providers.insert(
            "mock".into(),
            ProviderConfigEntry {
                base_url: Some(base_url),
                api_key: Some("test-key".into()),
                models: vec!["mock/test-model".into()],
            },
        );

        let runtime = AgentRuntime::new(&config).unwrap();
        let mut stream = runtime.agent().run_stream(
            "Write a test file named agent_test.txt in the root of this repo \
             with the content 'hello from the agent'.",
        );

        let mut saw_final_text = false;
        let outcome = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(event) = stream.next().await {
                match event {
                    AgentEvent::TextDelta(_) => saw_final_text = true,
                    AgentEvent::Complete(_) => return Ok(()),
                    AgentEvent::Error(e) => return Err(anyhow::anyhow!("agent error: {e}")),
                    _ => {}
                }
            }
            Err(anyhow::anyhow!("agent stream ended without completing"))
        })
        .await;

        // Always clean up the scratch repo, even on assertion failure.
        let written = std::fs::read_to_string(&file_path);
        let _ = std::fs::remove_dir_all(&dir);

        outcome.expect("agent run did not complete in time").unwrap();
        assert!(
            saw_final_text,
            "agent never produced the final reply text"
        );
        assert_eq!(
            written.expect("agent did not write the test file"),
            "hello from the agent",
            "agent wrote the wrong content"
        );
    }

    /// End-to-end: the parent spawns a researcher-web sub-agent and the
    /// sub-agent's activity (Started/Finished) is forwarded to broadcast
    /// subscribers — the exact path the TUI uses to render nested tool calls.
    #[tokio::test]
    async fn subagent_activity_flows_to_subscribers() {
        use cersei::events::AgentEvent;
        use std::time::Duration;

        let base_url = spawn_mock_server().await;
        let mut config = AppConfig::default();
        config.provider = "mock".into();
        config.model = "mock/test-model".into();
        config.working_dir = std::env::temp_dir();
        config.permissions_mode = "allow_all".into();
        config.providers.insert(
            "mock".into(),
            ProviderConfigEntry {
                base_url: Some(base_url),
                api_key: Some("test-key".into()),
                models: vec!["mock/test-model".into()],
            },
        );

        let runtime = AgentRuntime::new(&config).unwrap();
        let mut sub_rx = runtime.subscribe_subagents();
        let mut stream = runtime.agent().run_stream("Research the answer.");

        // Drive the parent run to completion.
        tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(event) = stream.next().await {
                match event {
                    AgentEvent::Complete(_) => return,
                    AgentEvent::Error(e) => panic!("agent error: {e}"),
                    _ => {}
                }
            }
            panic!("agent stream ended without completing");
        })
        .await
        .expect("parent run did not finish in time");

        // Drain the activity channel: the sub-agent's Started/Finished must
        // have been forwarded (the mock replies with plain text, so no tool
        // events beyond those).
        let mut saw_started = false;
        let mut saw_finished = false;
        loop {
            match tokio::time::timeout(Duration::from_millis(500), sub_rx.recv()).await {
                Ok(Ok(activity)) => match activity {
                    crate::subagents::SubAgentActivity::Started { agent_type, .. } => {
                        assert_eq!(agent_type, "researcher-web");
                        saw_started = true;
                    }
                    crate::subagents::SubAgentActivity::Finished { .. } => saw_finished = true,
                    _ => {}
                },
                _ => break,
            }
        }
        assert!(saw_started, "no Started event forwarded to subscribers");
        assert!(saw_finished, "no Finished event forwarded to subscribers");
    }
}

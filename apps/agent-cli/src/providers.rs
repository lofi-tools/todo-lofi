//! Provider registry: built-in providers, config-file overrides, API key
//! resolution, and shared agent construction.
//!
//! Built-in providers (all OpenAI-compatible): poolside, openrouter, groq,
//! nvidia nim, the tokenrouter gateway, kiosapi, google (AI Studio), ollama
//! (ollama.com's hosted cloud API), opencode-zen (opencode.ai's Zen gateway),
//! and orcarouter (orcarouter.ai's meta-router). Their base URL / api key /
//! models can be overridden — or new providers added — via the config file's
//! `[providers.NAME]` section.
//!
//! An `api_key` value in config is either:
//! - `!command` — run the rest as a shell command and use its trimmed stdout,
//! - `env:VAR` — read the environment variable,
//! - a literal key.

use crate::config::AppConfig;
use ai_providers::{
    AttemptScope, Catalog, CooldownRegistry, FailureKind, ModelKey, ModelSpec, NullStore,
    ProviderSpec, RoutingDecision, StoreHandle,
};
use anyhow::Context as _;
use cersei::tools::permissions::{AllowAll, AllowReadOnly};
use cersei::types::{Message, Role};
use cersei::{Agent, OpenAi};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

/// A configured provider (built-in defaults merged with the config file).
/// Sampling parameters (`max_tokens`, `temperature`, `top_p`, `extra_body`)
/// live on each [`ConfiguredModel`], not here: they vary per model.
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    /// API key spec: `!command`, `env:VAR`, or a literal key.
    pub api_key: String,
    pub models: Vec<ConfiguredModel>,
}

/// One model on a provider, with optional per-model request parameters.
/// An unset parameter leaves the request untouched (the agent default
/// applies).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredModel {
    pub id: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub extra_body: Option<serde_json::Value>,
}

impl ConfiguredModel {
    /// A bare model id with no per-model parameters.
    pub fn bare(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            extra_body: None,
        }
    }
}

impl From<&str> for ConfiguredModel {
    fn from(id: &str) -> Self {
        Self::bare(id)
    }
}

impl From<String> for ConfiguredModel {
    fn from(id: String) -> Self {
        Self::bare(id)
    }
}

impl From<crate::config::ModelRef> for ConfiguredModel {
    fn from(model_ref: crate::config::ModelRef) -> Self {
        match model_ref {
            crate::config::ModelRef::Simple(id) => Self::bare(id),
            crate::config::ModelRef::Detailed(entry) => Self {
                id: entry.id,
                max_tokens: entry.max_tokens,
                temperature: entry.temperature,
                top_p: entry.top_p,
                extra_body: entry.extra_body,
            },
        }
    }
}

/// The wire-level ids of a provider's models, in order.
pub fn model_ids(provider: &Provider) -> Vec<String> {
    provider.models.iter().map(|m| m.id.clone()).collect()
}

/// Find a model on a provider by wire id (bare or `provider/`-prefixed).
/// Kept for the explorer's id matching (bare, `provider/`-prefixed, or exactly
/// matching ids) — the library's resolver uses the same three forms.
#[allow(dead_code)]
fn find_model<'a>(provider: &'a Provider, model: &str) -> Option<&'a ConfiguredModel> {
    provider.models.iter().find(|m| {
        m.id == model
            || m.id == format!("{}/{}", provider.name, model)
            || format!("{}/{}", provider.name, m.id) == model
    })
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
    fs_reader: crate::subagents::AcpFsSink,
    readonly: bool,
    reasoning: cersei::provider::ReasoningField,
    ask_user_tool: Option<Box<dyn cersei::tools::Tool>>,
    followup_ask_user: Option<AskUserBridge>,
    subagent_providers: Option<crate::subagents::SubAgentProviders>,
) -> Vec<Box<dyn cersei::tools::Tool>> {
    let mut tools = cersei::tools::coding();
    // Replace the built-in Grep with our ripgrep version (raw `rg` flag
    // passthrough, per-file and global result caps — freebuff-style).
    tools.retain(|t| t.name() != "Grep");
    // Replace the built-in WebSearch (reads the legacy CERSEI_SEARCH_API_KEY
    // env var) with our Exa-first / Parallel / TinyFish / LangSearch version.
    tools.retain(|t| t.name() != "WebSearch");
    tools.push(Box::new(crate::tools::WebSearchTool));
    // Drop cersei's built-in ExaSearch tool: Exa is reached through the
    // WebSearch backend chain above, so the model must not see a second,
    // standalone search tool that reads the same EXA_API_KEY.
    tools.retain(|t| t.name() != "ExaSearch");
    // The ACP client-aware Read/Write/Edit overrides (which consult the
    // editor's unsaved buffers and mirror edits back via `fs/write_text_file`)
    // are only wired when an ACP fs bridge is present — i.e. when the binary
    // runs as an ACP server. In TUI and `-p` mode (`fs_reader` is None) the
    // built-in Read/Write/Edit tools are left in place.
    if fs_reader.is_some() {
        tools.retain(|t| t.name() != "Read");
        tools.push(Box::new(crate::tools::ClientReadTool::new(
            fs_reader.clone(),
        )));
        tools.retain(|t| t.name() != "Write" && t.name() != "Edit");
        tools.push(Box::new(crate::tools::ClientWriteTool::new(
            fs_reader.clone(),
        )));
        tools.push(Box::new(crate::tools::ClientEditTool::new(fs_reader)));
    }
    tools.push(Box::new(crate::tools::RgSearchTool));
    tools.push(Box::new(crate::tools::ReadDocsTool));
    if let Some(tool) = ask_user_tool {
        tools.push(tool);
    } else {
        tools.push(Box::new(crate::tools::AskUserTool::new()));
    }
    tools.push(Box::new(
        cersei::tools::synthetic_output::SyntheticOutputTool,
    ));
    // write_todos tracking, used by the phase workflow in the system prompt.
    tools.push(Box::new(cersei::tools::todo_write::TodoWriteTool));
    tools.push(Box::new(crate::subagents::SuggestFollowupsTool::new(
        followups,
        followup_ask_user,
    )));
    // Read-only sessions (ACP readonly mode) can't spawn sub-agents: the
    // sub-agents run with AllowAll and could modify files.
    if !readonly {
        tools.push(Box::new(crate::subagents::SpawnAgentsTool::new(
            resolved.clone(),
            parent,
            events,
            reasoning,
            subagent_providers,
        )));
    }
    tools
}

/// A factory for the providers spawned sub-agents run on. Each call builds a
/// paced provider on `(provider, model)` with a *fresh* attempt scope: the same
/// session, the spawning turn as `parent_turn_id`, and its own turn slot, so
/// concurrent sub-agents can't overwrite each other's attribution.
fn subagent_provider_factory(
    catalog: Arc<Catalog>,
    scope: Arc<AttemptScope>,
    provider: &str,
    model: &str,
    reasoning: cersei::provider::ReasoningField,
) -> crate::subagents::SubAgentProviders {
    let provider = provider.to_string();
    let model = model.to_string();
    Arc::new(move || {
        let child =
            Arc::new(AttemptScope::new(scope.session_id.clone()).with_parent_turn(scope.turn()));
        catalog.provider_impl(&provider, &model, child, reasoning)
    })
}

/// Build the OpenAI-compatible provider for a resolved selection, carrying the
/// model family's reasoning field so the SSE reader captures `delta.reasoning`
/// (see `response_format::reasoning_field_for`), and wrapping it so the
/// model's configured request parameters (`max_tokens`, `temperature`,
/// `top_p`, `extra_body`) are applied to every completion request.
pub fn openai_provider(
    resolved: &Resolved,
    reasoning: cersei::provider::ReasoningField,
) -> anyhow::Result<ConfiguredProvider> {
    let inner = OpenAi::builder()
        .base_url(&resolved.base_url)
        .api_key(&resolved.api_key)
        .model(&resolved.model)
        .reasoning_field(reasoning)
        .build()
        .context("failed to build provider")?;
    Ok(ConfiguredProvider {
        inner,
        max_tokens: resolved.max_tokens,
        temperature: resolved.temperature,
        top_p: resolved.top_p,
        extra_body: resolved.extra_body.clone(),
    })
}

/// An OpenAI-compatible provider with per-model request parameters applied
/// on top of every completion request. The parameters come from the model's
/// own `models` entry (`max_tokens`, `temperature`, `top_p`, `extra_body`);
/// unset parameters leave the request untouched.
///
/// `top_p` and `extra_body` ride in the request's `ProviderOptions` and are
/// emitted by cersei's OpenAi provider on the wire (`top_p` as a top-level
/// body field, `extra_body` merged as an object of extra body fields).
pub struct ConfiguredProvider {
    inner: OpenAi,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    extra_body: Option<serde_json::Value>,
}

impl ConfiguredProvider {
    /// The configured reasoning field (for tests / diagnostics).
    pub fn reasoning_field(&self) -> cersei::provider::ReasoningField {
        self.inner.reasoning_field()
    }
}

#[async_trait::async_trait]
impl cersei::provider::Provider for ConfiguredProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn context_window(&self, model: &str) -> u64 {
        self.inner.context_window(model)
    }

    async fn complete(
        &self,
        mut request: cersei::provider::CompletionRequest,
    ) -> cersei::types::Result<cersei::provider::CompletionStream> {
        if let Some(max_tokens) = self.max_tokens {
            request.max_tokens = max_tokens;
        }
        if let Some(temperature) = self.temperature {
            request.temperature = Some(temperature);
        }
        if let Some(top_p) = self.top_p {
            request.options.set("top_p", top_p);
        }
        if let Some(extra_body) = &self.extra_body {
            request.options.set("extra_body", extra_body.clone());
        }
        self.inner.complete(request).await
    }
}

/// A provider/model selection with the API key already resolved, plus the
/// model's configured request parameters (None = use the agent defaults).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    /// Per-model max output tokens; `None` falls back to the agent default.
    pub max_tokens: Option<u32>,
    /// Per-model sampling temperature; `None` leaves the agent default.
    pub temperature: Option<f32>,
    /// Per-model nucleus sampling cutoff; `None` leaves the server default.
    pub top_p: Option<f32>,
    /// Extra JSON body fields merged into every request for this model.
    pub extra_body: Option<serde_json::Value>,
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
    /// Optional ACP client-filesystem bridge; when present the wrapping file
    /// tools consult/mirror the client's editor buffers. None in TUI/`-p`.
    pub fs_reader: crate::subagents::AcpFsSink,
    /// Which delta field the provider reads thinking from (resolved from
    /// `config.model_families`; see `response_format::reasoning_field_for`).
    /// Lets the OpenAI-compatible SSE reader capture `delta.reasoning` for
    /// reasoning models before it would be dropped.
    pub reasoning: cersei::provider::ReasoningField,
    /// The configured `ask_user` tool. When `Some`, the tool uses the TUI
    /// channel to pause for user answers. When `None`, a default non-interactive
    /// tool is used.
    pub ask_user_tool: Option<Box<dyn cersei::tools::Tool>>,
    /// Optional ACP bridge that makes `suggest_followups` wait for a clickable
    /// multi-select response instead of only storing suggestions locally.
    pub followup_ask_user: Option<AskUserBridge>,
    /// A pre-built provider — the library's paced transport, carrying cooldowns
    /// and per-attempt telemetry. `None` builds the plain provider from
    /// `resolved`.
    pub provider: Option<Box<dyn cersei::provider::Provider>>,
    /// Factory for the providers that spawned sub-agents run on, so a sub-agent
    /// request is paced, cooldown-aware and attributed to its own attempt scope
    /// (parent session, spawning turn as `parent_turn_id`). `None` leaves
    /// sub-agents on the plain provider.
    pub subagent_providers: Option<crate::subagents::SubAgentProviders>,
}

// ─── Provider registry ──────────────────────────────────────────────────────

fn builtin_providers() -> Vec<Provider> {
    vec![
        Provider {
            name: "poolside".into(),
            base_url: "https://inference.poolside.ai/v1".into(),
            api_key: "env:POOLSIDE_API_KEY".into(),
            models: vec![
                "poolside/laguna-xs-2.1".into(),
                "poolside/laguna-s-2.1".into(),
            ],
        },
        Provider {
            name: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "env:OPENROUTER_API_KEY".into(),
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
            // endpoint).
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
        Provider {
            name: "kiosapi".into(),
            base_url: "https://kiosapi.com/v1".into(),
            api_key: "env:KIOSAPI_API_KEY".into(),
            // KiosAPI is a New API-style OpenAI-compatible gateway; the
            // default model is qwen3.8-flash.
            models: vec!["kiosapi/qwen3.8-flash".into()],
        },
        Provider {
            name: "ollama".into(),
            base_url: "https://ollama.com/v1".into(),
            api_key: "env:OLLAMA_CLOUD_API_KEY".into(),
            // ollama.com's hosted cloud API — the same OpenAI-compatible
            // endpoint the local server exposes, backed by the hosted model
            // catalog. Default is gpt-oss:120b.
            models: vec!["ollama/gpt-oss:120b".into()],
        },
        Provider {
            name: "opencode-zen".into(),
            base_url: "https://opencode.ai/zen/v1".into(),
            api_key: "env:OPENCODE_API_KEY".into(),
            // OpenCode Zen (opencode.ai) — an OpenAI-compatible gateway over
            // frontier model providers (Anthropic, Google, OpenAI, …). The
            // default model is gemini-3.8-flash.
            models: vec!["opencode-zen/gemini-3.8-flash".into()],
        },
        Provider {
            name: "google".into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai/".into(),
            api_key: "env:GEMINI_API_KEY".into(),
            // Google AI Studio (Gemini API). The OpenAI-compatible endpoint
            // maps `reasoning_effort` to Gemini's thinking level; this entry
            // is gemini-3.8-flash at low thinking.
            models: vec!["google/gemini-3.8-flash".into()],
        },
        Provider {
            name: "orcarouter".into(),
            base_url: "https://api.orcarouter.ai/v1".into(),
            api_key: "env:ORCAROUTER_API_KEY".into(),
            // OrcaRouter is a zero-markup meta-router over OpenAI, Anthropic,
            // Google, DeepSeek, xAI, Qwen and others; `orcarouter/auto` picks
            // the cheapest live model per request. The rest is a curated
            // subset of its coding-capable models (free and paid).
            models: vec![
                "orcarouter/auto".into(),
                "orcarouter/free".into(),
                "orcarouter/fusion".into(),
                "anthropic/claude-sonnet-5".into(),
                "openai/gpt-5.4".into(),
                "google/gemini-3.5-flash".into(),
                "deepseek/deepseek-v4-pro-0813".into(),
                "z-ai/glm-5.3-flash-free".into(),
                "tencent/hy3-free".into(),
            ],
        },
    ]
}

/// The built-in providers rendered as config-file entries — used by the
/// default-config template so `providers` shows the real defaults a user
/// would edit. Each entry carries the provider's first model, keeping the
/// template compact.
pub(crate) fn builtin_provider_entries() -> Vec<(String, crate::config::ProviderConfigEntry)> {
    builtin_providers()
        .into_iter()
        .map(|p| {
            let models = p
                .models
                .into_iter()
                .take(1)
                .map(|m| crate::config::ModelRef::Simple(m.id))
                .collect();
            (
                p.name,
                crate::config::ProviderConfigEntry {
                    base_url: Some(p.base_url),
                    api_key: Some(p.api_key),
                    models,
                    pacing: None,
                },
            )
        })
        .collect()
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
                models: combo_names
                    .into_iter()
                    .map(|c| ConfiguredModel::bare(c.name))
                    .collect(),
            },
        );
    }

    for (name, entry) in &config.providers {
        let provider = by_name.entry(name.clone()).or_insert_with(|| Provider {
            name: name.clone(),
            base_url: String::new(),
            api_key: String::new(),
            models: Vec::new(),
        });
        if let Some(base_url) = &entry.base_url {
            provider.base_url = base_url.clone();
        }
        if let Some(api_key) = &entry.api_key {
            provider.api_key = api_key.clone();
        }
        if !entry.models.is_empty() {
            // An explicit `models` override takes full control of the list.
            // Entries are bare ids or detailed objects with per-model params.
            provider.models = entry
                .models
                .clone()
                .into_iter()
                .map(ConfiguredModel::from)
                .collect();
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
        provider = match model
            .split('/')
            .next()
            .filter(|prefix| known.contains(prefix))
        {
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
        .map(|m| m.id.clone())
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
                .map(move |model| (p.name.clone(), model.id))
        })
        .collect()
}

/// The provider set as the library wants it: the same merge (built-ins, the
/// virtual `combos` provider, then config additions) mapped into spec structs,
/// with each model's response-format family attached from `model_families` and
/// each provider's pacing ceilings from its config entry.
pub fn provider_specs(config: &AppConfig) -> Vec<ProviderSpec> {
    providers(config)
        .into_iter()
        .map(|provider| {
            let pacing = config
                .providers
                .get(&provider.name)
                .and_then(|entry| entry.pacing.as_ref())
                .map(|pacing| pacing.to_library())
                .unwrap_or_default();
            let models = provider
                .models
                .into_iter()
                .map(|model| ModelSpec {
                    family: config.model_families.get(&model.id).cloned(),
                    id: model.id,
                    max_tokens: model.max_tokens,
                    temperature: model.temperature,
                    top_p: model.top_p,
                    extra_body: model.extra_body,
                })
                .collect();
            ProviderSpec {
                name: provider.name,
                base_url: provider.base_url,
                api_key: provider.api_key,
                models,
                pacing,
            }
        })
        .collect()
}

/// Build the library catalog for a config. The agent keeps its own
/// config-shaped `Provider` type (tools and combos are built from it) and hands
/// the library this spec view at the boundary.
pub fn catalog(config: &AppConfig, store: StoreHandle) -> Catalog {
    Catalog::new(provider_specs(config), store)
}

/// The catalog with a pre-seeded cooldown registry — the runtime's variant, so
/// the walk, the router and the transport share one "what is cooling" view.
pub fn catalog_with_cooldowns(
    config: &AppConfig,
    store: StoreHandle,
    cooldowns: Arc<CooldownRegistry>,
) -> Catalog {
    Catalog::with_cooldowns(provider_specs(config), store, cooldowns)
}

/// Fetch the full available model list from a provider's OpenAI-compatible
/// `/models` endpoint. Used by the `/provider` explorer to show models the
/// config doesn't list (so the user can discover and switch live). Returns
/// model ids sorted and de-duplicated.
pub async fn fetch_models(base_url: &str, api_key: &str) -> anyhow::Result<Vec<String>> {
    ai_providers::fetch_models(base_url, api_key).await
}

// ─── API key resolution ─────────────────────────────────────────────────────

/// Resolve a value spec — `!command` (run shell, use trimmed stdout),
/// `env:VAR` (read the environment variable), or a literal — into a concrete
/// value. `what` names the setting in error messages (e.g. "api_key" or
/// "env"). Used by provider api keys and the config `env` map.
pub fn resolve_value_spec(spec: &str, what: &str) -> anyhow::Result<String> {
    ai_providers::resolve_value_spec(spec, what)
}

pub fn resolve_api_key(spec: &str) -> anyhow::Result<String> {
    ai_providers::resolve_api_key(spec)
}

/// Resolve a provider + model into concrete base URL and API key. The virtual
/// `combos` provider resolves to the first entry of the named combo.
/// Request parameters come from the model's own `models` entry; a model
/// without its own entry uses the agent defaults.
pub fn resolve(config: &AppConfig, provider_name: &str, model: &str) -> anyhow::Result<Resolved> {
    if provider_name == "combos" {
        let entry = combo_first_entry(config, model)
            .ok_or_else(|| anyhow::anyhow!("unknown combo '{model}'"))?;
        return resolve(config, &entry.provider, &entry.model);
    }
    let library = catalog(config, Arc::new(NullStore)).resolve(provider_name, model)?;
    Ok(Resolved {
        provider: library.provider,
        model: library.model,
        base_url: library.base_url,
        api_key: library.api_key,
        max_tokens: library.max_tokens,
        temperature: library.temperature,
        top_p: library.top_p,
        extra_body: library.extra_body,
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
///
/// Expiries are wall-clock times persisted to a small JSON file (default
/// `~/.abstract/cooldowns.json`), so a rate-limited provider stays cooled down
/// across restarts of the process.
#[derive(Clone)]
pub struct FallbackManager {
    enabled: bool,
    /// Entries in priority order (most preferred first).
    priority: Vec<FallbackEntry>,
    /// The process-wide cooldown mirror, shared with the catalog so the walk,
    /// the router and the transport all read one view of "what is cooling".
    cooldowns: Arc<CooldownRegistry>,
    /// Durable cooldowns and attempt history. `NullStore` until a runtime
    /// attaches the telemetry store, so a manager built outside a run still
    /// works in-process.
    store: StoreHandle,
    /// The routing score. `None` until a store is attached, and permanently
    /// `None` when `routing.enabled` is false — which is exactly the ordered
    /// walk this code did before scoring existed.
    router: Option<Arc<ai_providers::Router>>,
    /// How long a locally recorded failure cools an entry down. This is the
    /// walk's fallback policy; the transport's per-kind policy is richer and
    /// writes the same registry.
    cooldown: Duration,
    last_decision: Arc<Mutex<Option<RoutingDecision>>>,
    decision_sink: Arc<Mutex<Option<Arc<crate::telemetry::AgentStore>>>>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl FallbackManager {
    /// `entries` is the combo's fallback list in order. An empty list (a
    /// non-combo selection) means fallback is disabled.
    pub fn new(config: &AppConfig, entries: Vec<FallbackEntry>) -> Self {
        Self {
            enabled: config.fallback.enabled && !entries.is_empty(),
            priority: entries,
            cooldowns: Arc::new(CooldownRegistry::new()),
            store: Arc::new(NullStore),
            router: None,
            cooldown: Duration::from_secs(config.fallback.cooldown_seconds),
            last_decision: Arc::new(Mutex::new(None)),
            decision_sink: Arc::new(Mutex::new(None)),
            session_id: Arc::new(Mutex::new(None)),
        }
    }

    /// Attach the telemetry store (and the catalog's cooldown registry), which
    /// turns on durable cooldowns and the routing score. Called by the runtime
    /// once, at construction.
    pub fn with_store(
        mut self,
        config: &AppConfig,
        store: StoreHandle,
        cooldowns: Arc<CooldownRegistry>,
    ) -> Self {
        let routing = config.routing.to_library();
        self.router = routing.enabled.then(|| {
            Arc::new(ai_providers::Router::new(
                routing,
                store.clone(),
                cooldowns.clone(),
            ))
        });
        self.store = store;
        self.cooldowns = cooldowns;
        self
    }

    /// Attach the database the routing decisions are written to, plus the
    /// session they belong to.
    pub fn with_decision_sink(
        mut self,
        store: Option<Arc<crate::telemetry::AgentStore>>,
        session_id: Option<String>,
    ) -> Self {
        self.decision_sink = Arc::new(Mutex::new(store));
        self.session_id = Arc::new(Mutex::new(session_id));
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether entries are reordered by the score rather than walked in order.
    pub fn routing_enabled(&self) -> bool {
        self.router.is_some()
    }

    /// Mark an entry as failed; it won't be selected for fallback again until
    /// the cooldown expires. The expiry is mirrored in-process immediately and
    /// written to the store in the background (best-effort: losing a cooldown
    /// is not fatal).
    pub fn record_failure(&self, provider: &str, model: &str) {
        let key = ModelKey::new(provider, model);
        let until = SystemTime::now() + self.cooldown;
        self.cooldowns.set(&key, until, "failed");
        let store = self.store.clone();
        // The call site is synchronous (the TUI/one-shot fallback walk), so the
        // write is detached. Without a runtime (unit tests) it stays in-process.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = store
                    .set_cooldown(
                        &key,
                        until,
                        FailureKind::Unknown {
                            message: "fallback".into(),
                        },
                    )
                    .await
                {
                    eprintln!("warning: failed to persist cooldown for {key}: {error}");
                }
            });
        }
    }

    /// The most preferred entry to fall back to after `(provider, model)`
    /// failed, skipping the current entry and any still cooling down.
    pub fn next_entry(&self, provider: &str, model: &str) -> Option<FallbackEntry> {
        self.priority
            .iter()
            .find(|entry| {
                (entry.provider != provider || entry.model != model)
                    && !self
                        .cooldowns
                        .is_cooling(&ModelKey::new(&entry.provider, &entry.model))
            })
            .cloned()
    }

    /// Score the combo's entries and remember the choice.
    ///
    /// Returns `None` when routing is off or there is nothing to choose from;
    /// the caller then walks `next_entry` in order, exactly as before.
    pub async fn choose(
        &self,
        combo: Option<&str>,
        candidates: Vec<ai_providers::Candidate>,
    ) -> Option<RoutingDecision> {
        let router = self.router.clone()?;
        let decision = router.choose(combo, &candidates).await?;
        *self.last_decision.lock() = Some(decision.clone());
        self.persist_decision(&decision);
        Some(decision)
    }

    /// The most recent decision (for `/why`).
    pub fn last_decision(&self) -> Option<RoutingDecision> {
        self.last_decision.lock().clone()
    }

    fn persist_decision(&self, decision: &RoutingDecision) {
        let store = self.decision_sink.lock().clone();
        let Some(store) = store else {
            return;
        };
        let session_id = self.session_id.lock().clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let decision = decision.clone();
            handle.spawn(async move {
                if let Err(error) = store
                    .record_routing_decision(session_id.as_deref(), &decision)
                    .await
                {
                    eprintln!("warning: failed to record the routing decision: {error}");
                }
            });
        }
    }
}

/// Render a routing decision as the table `/why` shows: every candidate with
/// the terms that produced its penalty, and which one won.
pub fn explain_decision(decision: &RoutingDecision) -> String {
    let mut out = format!(
        "last routing decision at {:?}",
        decision
            .at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default()
    );
    if let Some(combo) = &decision.combo {
        out.push_str(&format!("\ncombo: {combo} → {}\n", decision.chosen));
    } else {
        out.push_str(&format!("\nchosen: {}\n", decision.chosen));
    }
    out.push_str(&format!(
        "\n  {:<34} {:>6} {:>9} {:>10} {:>7} {:>8}\n",
        "candidate", "fail", "attempts", "cooldown", "pacing", "penalty"
    ));
    for candidate in &decision.candidates {
        let marker = if candidate.key == decision.chosen {
            "*"
        } else {
            " "
        };
        let cooldown = match candidate.in_cooldown {
            Some(_) => "cooling".to_string(),
            None => "-".to_string(),
        };
        out.push_str(&format!(
            "{marker} {:<34} {:>6.2} {:>9.1} {:>10} {:>7.2} {:>8.2}\n",
            candidate.key.to_string(),
            candidate.failure_rate,
            candidate.weighted_attempts,
            cooldown,
            candidate.pacing_pressure,
            candidate.penalty,
        ));
    }
    out.push_str("(* = chosen; ties keep the configured order)");
    out
}

/// The candidates a combo's entries make, in configured order, each carrying
/// the load of its provider (the routing score's pacing term).
pub fn candidates_for_combo(config: &AppConfig, combo_name: &str) -> Vec<ai_providers::Candidate> {
    let catalog = catalog(config, Arc::new(NullStore));
    combo(config, combo_name)
        .map(|combo| {
            combo
                .entries
                .into_iter()
                .map(|entry| {
                    let pressure = catalog.limiter(&entry.provider).pressure();
                    ai_providers::Candidate::new(
                        ModelKey::new(&entry.provider, &entry.model),
                        pressure,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
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
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow::anyhow!("provider '{text}' has no models configured"))?;
        return Ok((p.name.clone(), model));
    }

    // "{provider}/{model...}" — match the provider prefix.
    for p in &all {
        if let Some(rest) = text.strip_prefix(&format!("{}/", p.name)) {
            for m in &p.models {
                if m.id == rest || m.id == text {
                    return Ok((p.name.clone(), m.id.clone()));
                }
            }
            return Err(anyhow::anyhow!(
                "unknown model '{rest}' for provider '{}' (available: {})",
                p.name,
                model_ids(p).join(", "),
            ));
        }
    }

    // Bare model name — match against every provider's models.
    for p in &all {
        for m in &p.models {
            if m.id == text
                || m.id
                    .strip_prefix(&format!("{}/", p.name))
                    .is_some_and(|rest| rest == text)
            {
                return Ok((p.name.clone(), m.id.clone()));
            }
        }
    }

    Err(anyhow::anyhow!("unknown provider or model: '{text}'"))
}

// ─── Agent construction ─────────────────────────────────────────────────────

pub fn build_agent(resolved: &Resolved, params: BuildParams) -> anyhow::Result<Arc<Agent>> {
    let provider = match params.provider {
        Some(provider) => provider,
        None => Box::new(openai_provider(resolved, params.reasoning)?),
    };

    let tools = agent_tools(
        resolved,
        params.parent.clone(),
        params.followups.clone(),
        params.subagent_events.clone(),
        params.fs_reader.clone(),
        params.readonly,
        params.reasoning,
        params.ask_user_tool,
        params.followup_ask_user,
        params.subagent_providers.clone(),
    );
    let mut builder = Agent::builder()
        .provider(provider)
        .model(&resolved.model)
        .max_turns(params.max_turns)
        .working_dir(params.working_dir)
        .cancel_token(params.cancel_token)
        .with_messages(params.messages)
        .tools(tools);
    // Model-family quirks: chat-style families (e.g. hy3) answer without
    // tools, so the runner must not force a tool-use round after a complete
    // answer (that is the "agent won't stop" symptom).
    builder = builder.no_tool_nudge(
        crate::model_families::family_for_model(&resolved.model)
            .map(|f| f.no_tool_nudge)
            .unwrap_or(true),
    );
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
/// A pending `ask_user` question set sent from the agent's tool to the TUI.
pub struct AskUserRequest {
    /// Unique id for this question set, so the TUI can match answers to requests.
    pub request_id: u64,
    /// The ACP session the question was asked in, when the tool ran under an
    /// ACP server. The TUI ignores it; the ACP elicitation path uses it to
    /// scope the `elicitation/create` request to the right session.
    pub session_id: Option<String>,
    /// The questions to display.
    pub questions: Vec<serde_json::Value>,
}

/// An answer sent from the TUI back to the agent's `ask_user` tool.
pub struct AskUserAnswer {
    /// Matches the `request_id` of the corresponding `AskUserRequest`.
    pub request_id: u64,
    /// The user's answers. Each entry corresponds to one question in the
    /// original request. `None` means the question was skipped.
    pub answers: Vec<Option<AskUserAnswerValue>>,
}

/// An answer to an ask_user question. Regular questions use freeform text;
/// action-oriented multi-select prompts (such as ACP follow-ups) use indices.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AskUserAnswerValue {
    OtherText(String),
    SelectedIndices(Vec<usize>),
}

/// ACP bridge used by `suggest_followups` to present model suggestions through
/// the same ask-user elicitation channel as regular questions.
#[derive(Clone)]
pub struct AskUserBridge {
    pub ask_user_tx: tokio::sync::mpsc::UnboundedSender<AskUserRequest>,
    pub answer_rx:
        std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AskUserAnswer>>>,
    request_counter: Arc<std::sync::atomic::AtomicU64>,
}

impl AskUserBridge {
    pub fn new(
        ask_user_tx: tokio::sync::mpsc::UnboundedSender<AskUserRequest>,
        answer_rx: std::sync::Arc<
            tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AskUserAnswer>>,
        >,
    ) -> Self {
        Self {
            ask_user_tx,
            answer_rx,
            request_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn next_request_id(&self) -> u64 {
        self.request_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

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
    /// The provider catalog: registry, limiters, discovery cache.
    catalog: Arc<Catalog>,
    /// The database handle for session/turn writes, when telemetry is on.
    telemetry: Option<Arc<crate::telemetry::AgentStore>>,
    /// The session row this runtime writes, when telemetry is on.
    session: Option<crate::telemetry::SessionStart>,
    /// The turn in flight, plus how many turns this session has run.
    turn: Option<String>,
    turn_seq: u32,
    turn_started: Option<std::time::Instant>,
    /// Shared attempt attribution handed to every provider this runtime builds.
    scope: Arc<AttemptScope>,
    /// In-memory cache of each provider's `/models` response, kept for the
    /// whole run so the `/provider` explorer never re-fetches a provider it
    /// already browsed. Successes are cached; failures are retried.
    model_cache: Mutex<HashMap<String, Result<Vec<String>, String>>>,
    /// Channel for `ask_user` questions from the agent's tool to the runtime.
    /// The runtime forwards these to the TUI.
    ask_user_tx: tokio::sync::mpsc::UnboundedSender<AskUserRequest>,
    /// Receiver for `ask_user` questions from the tool.
    ask_user_rx: tokio::sync::mpsc::UnboundedReceiver<AskUserRequest>,
    /// Channel for answers from the runtime back to the tool.
    answer_tx: tokio::sync::mpsc::UnboundedSender<AskUserAnswer>,
}

impl AgentRuntime {
    /// A runtime with no telemetry: in-process cooldowns, no database, the
    /// ordered walk. Used by tests and by anything that runs without a store.
    pub fn new(config: &AppConfig) -> anyhow::Result<Self> {
        Self::build(config, Arc::new(NullStore), None, Arc::new(CooldownRegistry::new()))
    }

    /// A runtime backed by the telemetry store: durable cooldowns, a session
    /// row, shared attempt attribution and scored combo selection. Cooldowns
    /// still in effect from earlier runs are seeded from the database first.
    pub async fn with_telemetry(
        config: &AppConfig,
        store: StoreHandle,
        telemetry: Option<Arc<crate::telemetry::AgentStore>>,
    ) -> anyhow::Result<Self> {
        let cooldowns = Arc::new(CooldownRegistry::new());
        crate::telemetry::seed_registry(&store, &cooldowns).await;
        Self::build(config, store, telemetry, cooldowns)
    }

    fn build(
        config: &AppConfig,
        store: StoreHandle,
        telemetry: Option<Arc<crate::telemetry::AgentStore>>,
        cooldowns: Arc<CooldownRegistry>,
    ) -> anyhow::Result<Self> {
        let (provider, model) = default_selection(config)?;
        let (effective_provider, effective_model) = effective_selection(config, &provider, &model)?;
        let resolved = resolve(config, &effective_provider, &effective_model)?;
        let parent = Arc::new(Mutex::new(None));
        let followups = Arc::new(Mutex::new(Vec::new()));
        let (subagent_tx, _) = tokio::sync::broadcast::channel(1024);
        // Channel from tool -> runtime for ask_user questions.
        let (ask_user_tx, ask_user_rx) = tokio::sync::mpsc::unbounded_channel::<AskUserRequest>();
        // Channel from runtime -> tool for ask_user answers.
        let (answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel::<AskUserAnswer>();
        let ask_user_tool: Option<Box<dyn cersei::tools::Tool>> =
            Some(Box::new(crate::tools::AskUserTool::with_channel(
                ask_user_tx.clone(),
                Arc::new(tokio::sync::Mutex::new(answer_rx)),
            )));

        // One cooldown registry per runtime, shared by the catalog (transport
        // and limiters) and the fallback manager (walk + router).
        let catalog = Arc::new(catalog_with_cooldowns(
            config,
            store.clone(),
            cooldowns.clone(),
        ));
        let session = telemetry.as_ref().map(|_| {
            crate::telemetry::SessionStart::capture(
                config.working_dir.clone(),
                &provider,
                &model,
                &effective_provider,
                &effective_model,
            )
        });
        let scope = Arc::new(match &session {
            Some(session) => AttemptScope::new(Some(session.id.clone())),
            None => AttemptScope::anonymous(),
        });
        let reasoning = crate::response_format::reasoning_field_for(config, &resolved.model);
        let provider_impl = catalog.provider_impl(
            &effective_provider,
            &effective_model,
            scope.clone(),
            reasoning,
        )?;
        let subagent_providers = subagent_provider_factory(
            catalog.clone(),
            scope.clone(),
            &effective_provider,
            &effective_model,
            reasoning,
        );
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
                fs_reader: None,
                reasoning,
                ask_user_tool,
                followup_ask_user: None,
                provider: Some(provider_impl),
                subagent_providers: Some(subagent_providers),
            },
        )?;
        let fallback = fallback_for(config, &provider, &model)
            .with_store(config, store.clone(), cooldowns)
            .with_decision_sink(telemetry.clone(), session.as_ref().map(|s| s.id.clone()));
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
                catalog,
                telemetry,
                session,
                turn: None,
                turn_seq: 0,
                turn_started: None,
                scope,
                model_cache: Mutex::new(HashMap::new()),
                ask_user_tx,
                ask_user_rx,
                answer_tx,
            }),
        })
    }

    pub fn agent(&self) -> Arc<Agent> {
        self.inner.lock().agent.clone()
    }

    // ── Session + turn recording ────────────────────────────────────────

    /// The session row id, when telemetry is on.
    pub fn session_id(&self) -> Option<String> {
        self.inner.lock().session.as_ref().map(|s| s.id.clone())
    }

    /// Write the session row. Callers invoke this once, before the first run;
    /// a no-op when telemetry is off.
    pub async fn start_session(&self) {
        let (store, session) = {
            let inner = self.inner.lock();
            (inner.telemetry.clone(), inner.session.clone())
        };
        let (Some(store), Some(session)) = (store, session) else {
            return;
        };
        if let Err(error) = store.start_session(&session).await {
            eprintln!("warning: could not record the session: {error}");
        }
    }

    /// Open a turn: a new row, a fresh attempt attribution, a start clock.
    /// Returns the turn id when telemetry is on.
    pub async fn begin_turn(&self, provider: &str, model: &str) -> Option<String> {
        let (store, session_id, seq) = {
            let mut inner = self.inner.lock();
            let session_id = inner.session.as_ref().map(|s| s.id.clone())?;
            let seq = inner.turn_seq;
            inner.turn_seq += 1;
            (inner.telemetry.clone(), session_id, seq)
        };
        let turn = crate::telemetry::TurnStart::new(&session_id, seq, provider, model);
        if let Some(store) = &store
            && let Err(error) = store.start_turn(&turn).await
        {
            eprintln!("warning: could not record the turn: {error}");
        }
        let mut inner = self.inner.lock();
        inner.scope.set_turn(Some(turn.id.clone()));
        inner.turn = Some(turn.id.clone());
        inner.turn_started = Some(std::time::Instant::now());
        Some(turn.id)
    }

    /// Record token counts for the turn in flight (and roll them up into the
    /// session, so `sessions list` shows live totals).
    pub async fn note_usage(&self, input_tokens: u64, output_tokens: u64) {
        let (store, turn, session_id) = {
            let inner = self.inner.lock();
            (
                inner.telemetry.clone(),
                inner.turn.clone(),
                inner.session.as_ref().map(|s| s.id.clone()),
            )
        };
        let (Some(store), Some(turn), Some(session_id)) = (store, turn, session_id) else {
            return;
        };
        if let Err(error) = store
            .note_usage(&turn, &session_id, input_tokens, output_tokens)
            .await
        {
            eprintln!("warning: could not record token usage: {error}");
        }
    }

    /// Count a completed tool call against the turn in flight.
    pub async fn note_tool_call(&self) {
        let (store, turn) = {
            let inner = self.inner.lock();
            (inner.telemetry.clone(), inner.turn.clone())
        };
        let (Some(store), Some(turn)) = (store, turn) else {
            return;
        };
        if let Err(error) = store.note_tool_call(&turn).await {
            eprintln!("warning: could not count the tool call: {error}");
        }
    }

    /// Close the turn in flight with its outcome.
    pub async fn end_turn(
        &self,
        outcome: &str,
        error_kind: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let (store, turn, started) = {
            let mut inner = self.inner.lock();
            let turn = inner.turn.take();
            let started = inner.turn_started.take();
            inner.scope.set_turn(None);
            (inner.telemetry.clone(), turn, started)
        };
        let (Some(store), Some(turn)) = (store, turn) else {
            return;
        };
        if let Err(error) = store
            .finish_turn(
                &turn,
                outcome,
                error_kind,
                started.map(|started| started.elapsed()),
                input_tokens,
                output_tokens,
            )
            .await
        {
            eprintln!("warning: could not finish the turn: {error}");
        }
    }

    /// Close the session row (process exit, completion, or cancel).
    pub async fn finish_session(&self, outcome: &str, error_kind: Option<&str>) {
        let (store, session) = {
            let inner = self.inner.lock();
            (inner.telemetry.clone(), inner.session.clone())
        };
        let (Some(store), Some(session)) = (store, session) else {
            return;
        };
        if let Err(error) = store
            .finish_session(&session.id, outcome, error_kind)
            .await
        {
            eprintln!("warning: could not close the session: {error}");
        }
    }

    /// The most recent routing decision, for `/why`.
    pub fn last_decision(&self) -> Option<RoutingDecision> {
        self.inner.lock().fallback.last_decision()
    }

    /// The next entry to fall back to after a failure.
    ///
    /// With routing on and a combo selected, this is the entry the score picks
    /// (which may not be the next one in the list); otherwise it is the
    /// configured walk, skipping entries that are cooling down.
    pub async fn choose_next_entry(
        &self,
        current_provider: &str,
        current_model: &str,
    ) -> Option<FallbackEntry> {
        let (manager, combo_name) = {
            let inner = self.inner.lock();
            let combo_name = (inner.provider == "combos").then(|| inner.model.clone());
            (inner.fallback.clone(), combo_name)
        };
        if !manager.enabled() {
            return None;
        }
        let Some(combo_name) = combo_name.filter(|_| manager.routing_enabled()) else {
            return manager.next_entry(current_provider, current_model);
        };
        let config = { self.inner.lock().config.clone() };
        let candidates = candidates_for_combo(&config, &combo_name);
        let Some(decision) = manager.choose(Some(&combo_name), candidates).await else {
            return manager.next_entry(current_provider, current_model);
        };
        let current = ModelKey::new(current_provider, current_model);
        if decision.chosen != current
            && let Some(entry) = combo(&config, &combo_name).and_then(|combo| {
                combo
                    .entries
                    .into_iter()
                    .find(|entry| {
                        entry.provider == decision.chosen.provider
                            && entry.model == decision.chosen.model
                    })
            })
        {
            return Some(FallbackEntry {
                provider: entry.provider,
                model: entry.model,
            });
        }
        manager.next_entry(current_provider, current_model)
    }

    /// A provider transport for `(provider, model)` with pacing, cooldowns and
    /// telemetry attached, sharing this runtime's limiter and session.
    fn paced_provider(
        &self,
        provider: &str,
        model: &str,
    ) -> anyhow::Result<Box<dyn cersei::provider::Provider>> {
        let (catalog, scope, config) = {
            let inner = self.inner.lock();
            (
                inner.catalog.clone(),
                inner.scope.clone(),
                inner.config.clone(),
            )
        };
        let reasoning = crate::response_format::reasoning_field_for(&config, model);
        catalog.provider_impl(provider, model, scope, reasoning)
    }

    /// Subscribe to the sub-agent activity stream (rendered by the TUI as
    /// nested tool calls under each `spawn_agents` call).
    pub fn subscribe_subagents(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::subagents::SubAgentActivity> {
        self.inner.lock().subagent_tx.subscribe()
    }

    /// The sender for `ask_user` requests. The `AskUserTool` uses this to
    /// send questions to the TUI and await answers.
    pub fn ask_user_tx(&self) -> tokio::sync::mpsc::UnboundedSender<AskUserRequest> {
        let inner = self.inner.lock();
        inner.ask_user_tx.clone()
    }

    /// Drain all pending `ask_user` requests from the tool into `buf`. Called
    /// by the TUI tick loop to surface pending questions.
    pub fn drain_ask_user(&self, buf: &mut Vec<AskUserRequest>) {
        let mut inner = self.inner.lock();
        while let Ok(req) = inner.ask_user_rx.try_recv() {
            buf.push(req);
        }
    }

    /// Send an answer back to the agent's pending `ask_user` tool call.
    pub fn send_ask_user_answer(&self, answer: AskUserAnswer) {
        let inner = self.inner.lock();
        let _ = inner.answer_tx.send(answer);
    }

    /// Resolve a provider's base URL and API key for an out-of-band API call
    /// (e.g. the `/provider` model-list fetch). `provider` is the user-facing
    /// selection; combos are mapped to their concrete first entry.
    pub fn resolve_provider_endpoint(&self, provider: &str) -> anyhow::Result<(String, String)> {
        let config = &self.inner.lock().config;
        // Map a combos selection to its concrete first entry; other
        // selections resolve directly.
        let (eff_provider, _eff_model) = if provider == "combos" {
            let combos = combos(config);
            anyhow::ensure!(!combos.is_empty(), "no combos configured");
            let first = &combos[0].entries[0];
            (first.provider.clone(), first.model.clone())
        } else {
            (provider.to_string(), String::new())
        };
        // Pick any model for the provider so `resolve` produces base_url/key.
        let prov = providers(config)
            .into_iter()
            .find(|p| p.name == eff_provider)
            .with_context(|| format!("unknown provider '{eff_provider}'"))?;
        let model = prov
            .models
            .first()
            .map(|m| m.id.clone())
            .unwrap_or_default();
        let resolved = resolve(config, &eff_provider, &model)?;
        Ok((resolved.base_url, resolved.api_key))
    }

    /// The names of all providers available to switch to in the explorer
    /// (built-ins + virtual combos + config additions).
    pub fn provider_names(&self) -> Vec<String> {
        let config = &self.inner.lock().config;
        providers(config).into_iter().map(|p| p.name).collect()
    }

    /// All concrete providers for the `/provider` explorer (built-ins +
    /// config additions, in display order). The virtual `combos` provider is
    /// excluded — it has no `/models` endpoint of its own.
    pub fn explorer_providers(&self) -> Vec<Provider> {
        providers(&self.inner.lock().config)
            .into_iter()
            .filter(|p| p.name != "combos")
            .collect()
    }

    /// The cached `/models` response for `provider`, if it was already
    /// fetched this run (None when not fetched yet or the fetch failed).
    pub fn cached_models(&self, provider: &str) -> Option<Result<Vec<String>, String>> {
        self.inner.lock().model_cache.lock().get(provider).cloned()
    }

    /// Fetch a provider's full model list, caching the response in memory
    /// for the whole process run — navigating between providers in the
    /// `/provider` explorer never re-calls `/models` for a provider already
    /// browsed. Only successes are cached, so a transient failure is retried.
    pub async fn fetch_models_cached(&self, provider: &str) -> Result<Vec<String>, String> {
        if let Some(cached) = self.cached_models(provider) {
            return cached;
        }
        let (base_url, api_key) = self
            .resolve_provider_endpoint(provider)
            .map_err(|e| e.to_string())?;
        let result = crate::providers::fetch_models(&base_url, &api_key)
            .await
            .map_err(|e| e.to_string());
        if let Ok(models) = &result {
            self.inner
                .lock()
                .model_cache
                .lock()
                .insert(provider.to_string(), Ok(models.clone()));
        }
        result
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
        let (config, working_dir, max_turns, parent, followups, subagent_tx, catalog, scope) = {
            let g = self.inner.lock();
            (
                g.config.clone(),
                g.config.working_dir.clone(),
                g.config.max_turns,
                g.parent.clone(),
                g.followups.clone(),
                g.subagent_tx.clone(),
                g.catalog.clone(),
                g.scope.clone(),
            )
        };
        let resolved = resolve(&config, provider, model)?;
        let reasoning = crate::response_format::reasoning_field_for(&config, &resolved.model);
        let provider_impl = self.paced_provider(provider, model)?;
        let subagent_providers =
            subagent_provider_factory(catalog, scope, provider, model, reasoning);
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
                fs_reader: None,
                reasoning,
                ask_user_tool: None,
                followup_ask_user: None,
                provider: Some(provider_impl),
                subagent_providers: Some(subagent_providers),
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
        let (config, working_dir, max_turns, parent, followups, subagent_tx, catalog, scope) = {
            let g = self.inner.lock();
            (
                g.config.clone(),
                g.config.working_dir.clone(),
                g.config.max_turns,
                g.parent.clone(),
                g.followups.clone(),
                g.subagent_tx.clone(),
                g.catalog.clone(),
                g.scope.clone(),
            )
        };
        let (effective_provider, effective_model) = effective_selection(&config, provider, model)?;
        let resolved = resolve(&config, &effective_provider, &effective_model)?;
        let reasoning = crate::response_format::reasoning_field_for(&config, &resolved.model);
        let provider_impl = self.paced_provider(&effective_provider, &effective_model)?;
        let subagent_providers = subagent_provider_factory(
            catalog,
            scope,
            &effective_provider,
            &effective_model,
            reasoning,
        );
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
                fs_reader: None,
                reasoning,
                ask_user_tool: None,
                followup_ask_user: None,
                provider: Some(provider_impl),
                subagent_providers: Some(subagent_providers),
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
        let config = AppConfig {
            model: "groq/compound".into(),
            ..Default::default()
        };
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
            (
                "openrouter".to_string(),
                "openai/gpt-oss-20b:free".to_string()
            )
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
                models: vec![
                    crate::config::ModelRef::Simple("acme/big".into()),
                    crate::config::ModelRef::Simple("acme/small".into()),
                ],
                pacing: None,
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
    fn per_model_params_apply_to_their_model_only() {
        use crate::config::{ModelConfigEntry, ModelRef};
        let mut config = AppConfig::default();
        config.providers.insert(
            "nvidia".to_string(),
            ProviderConfigEntry {
                base_url: None,
                api_key: None,
                models: vec![
                    ModelRef::Detailed(ModelConfigEntry {
                        id: "deepseek-ai/deepseek-v4-flash-0731".into(),
                        max_tokens: Some(16384),
                        temperature: Some(1.0),
                        top_p: Some(0.95),
                        extra_body: Some(serde_json::json!({
                            "chat_template_kwargs": {
                                "thinking": true,
                                "reasoning_effort": "high"
                            }
                        })),
                    }),
                    ModelRef::Simple("z-ai/glm4.7".into()),
                ],
                pacing: None,
            },
        );
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("NVIDIA_API_KEY", "nvidia-test-key") };
        // The detailed model carries its own parameters.
        let resolved = resolve(&config, "nvidia", "deepseek-ai/deepseek-v4-flash-0731").unwrap();
        assert_eq!(resolved.max_tokens, Some(16384));
        assert_eq!(resolved.temperature, Some(1.0));
        assert_eq!(resolved.top_p, Some(0.95));
        let extra = resolved.extra_body.unwrap();
        assert_eq!(extra["chat_template_kwargs"]["thinking"], true);
        assert_eq!(extra["chat_template_kwargs"]["reasoning_effort"], "high");
        // The bare model sets no parameters (agent defaults apply).
        let bare = resolve(&config, "nvidia", "z-ai/glm4.7").unwrap();
        assert_eq!(bare.max_tokens, None);
        assert_eq!(bare.temperature, None);
        assert_eq!(bare.top_p, None);
        assert_eq!(bare.extra_body, None);
        // Display-prefixed ids resolve to the same per-model params.
        let prefixed = resolve(
            &config,
            "nvidia",
            "nvidia/deepseek-ai/deepseek-v4-flash-0731",
        )
        .unwrap();
        assert_eq!(prefixed.max_tokens, Some(16384));
    }

    #[test]
    fn tokenrouter_builtin_provider() {
        let config = AppConfig::default();
        let tr = provider(&config, "tokenrouter").unwrap();
        assert_eq!(tr.base_url, "https://api.tokenrouter.com/v1");
        assert_eq!(tr.api_key, "env:TOKENROUTER_API_KEY");
        assert!(model_ids(&tr).contains(&"deepseek/deepseek-v4-pro-0813".to_string()));
        assert!(model_ids(&tr).contains(&"qwen/qwen3-coder-next".to_string()));
        assert!(model_ids(&tr).contains(&"openai/gpt-oss-120b".to_string()));
        // Resolution wires up the gateway base URL.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("TOKENROUTER_API_KEY", "tr-test-key") };
        let resolved = resolve(&config, "tokenrouter", "qwen/qwen3-coder-next").unwrap();
        assert_eq!(resolved.base_url, "https://api.tokenrouter.com/v1");
        assert_eq!(resolved.model, "qwen/qwen3-coder-next");
        assert_eq!(resolved.api_key, "tr-test-key");
    }

    #[test]
    fn kiosapi_builtin_provider() {
        let config = AppConfig::default();
        let ki = provider(&config, "kiosapi").unwrap();
        assert_eq!(ki.base_url, "https://kiosapi.com/v1");
        assert_eq!(ki.api_key, "env:KIOSAPI_API_KEY");
        assert_eq!(model_ids(&ki), vec!["kiosapi/qwen3.8-flash"]);
        // Resolution wires up the gateway base URL and key.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("KIOSAPI_API_KEY", "kiosapi-test-key") };
        let resolved = resolve(&config, "kiosapi", "kiosapi/qwen3.8-flash").unwrap();
        assert_eq!(resolved.base_url, "https://kiosapi.com/v1");
        assert_eq!(resolved.model, "kiosapi/qwen3.8-flash");
        assert_eq!(resolved.api_key, "kiosapi-test-key");
    }

    #[test]
    fn google_builtin_provider() {
        let config = AppConfig::default();
        let g = provider(&config, "google").unwrap();
        assert_eq!(
            g.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai/"
        );
        assert_eq!(g.api_key, "env:GEMINI_API_KEY");
        assert_eq!(model_ids(&g), vec!["google/gemini-3.8-flash"]);
        // Resolution wires up the AI Studio base URL and key.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("GEMINI_API_KEY", "google-test-key") };
        let resolved = resolve(&config, "google", "google/gemini-3.8-flash").unwrap();
        assert_eq!(
            resolved.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai/"
        );
        assert_eq!(resolved.model, "google/gemini-3.8-flash");
        assert_eq!(resolved.api_key, "google-test-key");
    }

    #[test]
    fn ollama_builtin_provider() {
        let config = AppConfig::default();
        let o = provider(&config, "ollama").unwrap();
        assert_eq!(o.base_url, "https://ollama.com/v1");
        assert_eq!(o.api_key, "env:OLLAMA_CLOUD_API_KEY");
        assert_eq!(model_ids(&o), vec!["ollama/gpt-oss:120b"]);
        // Resolution wires up the hosted cloud base URL and key.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("OLLAMA_CLOUD_API_KEY", "ollama-test-key") };
        let resolved = resolve(&config, "ollama", "ollama/gpt-oss:120b").unwrap();
        assert_eq!(resolved.base_url, "https://ollama.com/v1");
        assert_eq!(resolved.model, "ollama/gpt-oss:120b");
        assert_eq!(resolved.api_key, "ollama-test-key");
    }

    #[test]
    fn orcarouter_builtin_provider() {
        let config = AppConfig::default();
        let o = provider(&config, "orcarouter").unwrap();
        assert_eq!(o.base_url, "https://api.orcarouter.ai/v1");
        assert_eq!(o.api_key, "env:ORCAROUTER_API_KEY");
        assert_eq!(model_ids(&o)[0], "orcarouter/auto");
        // Resolution wires up the meta-router base URL and key.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("ORCAROUTER_API_KEY", "orca-test-key") };
        let resolved = resolve(&config, "orcarouter", "orcarouter/auto").unwrap();
        assert_eq!(resolved.base_url, "https://api.orcarouter.ai/v1");
        assert_eq!(resolved.model, "orcarouter/auto");
        assert_eq!(resolved.api_key, "orca-test-key");
    }

    #[test]
    fn opencode_zen_builtin_provider() {
        let config = AppConfig::default();
        let oz = provider(&config, "opencode-zen").unwrap();
        assert_eq!(oz.base_url, "https://opencode.ai/zen/v1");
        assert_eq!(oz.api_key, "env:OPENCODE_API_KEY");
        assert_eq!(model_ids(&oz), vec!["opencode-zen/gemini-3.8-flash"]);
        // Resolution wires up the Zen gateway base URL and key.
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("OPENCODE_API_KEY", "opencode-test-key") };
        let resolved = resolve(&config, "opencode-zen", "opencode-zen/gemini-3.8-flash").unwrap();
        assert_eq!(resolved.base_url, "https://opencode.ai/zen/v1");
        assert_eq!(resolved.model, "opencode-zen/gemini-3.8-flash");
        assert_eq!(resolved.api_key, "opencode-test-key");
    }

    /// A config with telemetry off and routing off, so a test exercises the
    /// walk exactly as it behaved before scoring existed and never touches the
    /// real database.
    fn isolated_config() -> AppConfig {
        let mut config = AppConfig::default();
        config.telemetry.enabled = false;
        config.routing.enabled = false;
        config
    }

    #[test]
    fn fallback_priority_and_cooldown() {
        let config = isolated_config();
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
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("openrouter", "openrouter/free")
        );
        fb.record_failure("openrouter", "openrouter/free");
        let next = fb.next_entry("poolside", "poolside/laguna-xs-2.1").unwrap();
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("groq", "groq/compound")
        );
        fb.record_failure("groq", "groq/compound");
        // All alternates cooling down → nothing left to fall back to.
        assert!(
            fb.next_entry("poolside", "poolside/laguna-xs-2.1")
                .is_none()
        );
    }

    #[test]
    fn fallback_can_be_disabled() {
        let mut config = isolated_config();
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
        let config = isolated_config();
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
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1")
        );
        // After nvidia fails too, groq is tried again (it never cooled down).
        let next = fb
            .next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1")
            .unwrap();
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("groq", "groq/compound")
        );
        // With groq cooling down and nvidia current, nothing is left.
        fb.record_failure("groq", "groq/compound");
        assert!(
            fb.next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1")
                .is_none()
        );
    }

    #[test]
    fn plain_selection_has_no_fallback() {
        let config = isolated_config();
        let fb = fallback_for(&config, "groq", "groq/compound");
        assert!(!fb.enabled());
        assert!(fb.next_entry("groq", "groq/compound").is_none());
    }

    fn combo_entries() -> Vec<FallbackEntry> {
        vec![
            FallbackEntry {
                provider: "groq".into(),
                model: "groq/compound".into(),
            },
            FallbackEntry {
                provider: "nvidia".into(),
                model: "nvidia/llama-3.3-nemotron-super-49b-v1".into(),
            },
            FallbackEntry {
                provider: "poolside".into(),
                model: "poolside/laguna-xs-2.1".into(),
            },
        ]
    }

    #[tokio::test]
    async fn cooldowns_persist_through_the_store() {
        let config = isolated_config();
        let store: StoreHandle = Arc::new(ai_providers::InMemoryStore::new());
        let cooldowns = Arc::new(CooldownRegistry::new());
        let fb = FallbackManager::new(&config, combo_entries())
            .with_store(&config, store.clone(), cooldowns.clone());
        fb.record_failure("groq", "groq/compound");
        // The write is detached (the walk is synchronous), so let it run.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let stored = store
            .cooldown_until(&ModelKey::new("groq", "groq/compound"))
            .await
            .unwrap();
        assert!(stored.is_some(), "the failure must reach the store");

        // A restart: a fresh registry seeded from the store skips groq.
        let seeded = Arc::new(CooldownRegistry::new());
        crate::telemetry::seed_registry(&store, &seeded).await;
        assert!(seeded.is_cooling(&ModelKey::new("groq", "groq/compound")));
        let fb2 = FallbackManager::new(&config, combo_entries())
            .with_store(&config, store.clone(), seeded);
        let next = fb2
            .next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1")
            .unwrap();
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("poolside", "poolside/laguna-xs-2.1"),
            "the cooling entry is skipped after a restart"
        );
    }

    #[test]
    fn expired_cooldowns_are_ignored() {
        let config = isolated_config();
        let cooldowns = Arc::new(CooldownRegistry::new());
        let key = ModelKey::new("groq", "groq/compound");
        cooldowns.set(&key, SystemTime::now() - Duration::from_secs(1), "failed");
        let fb = FallbackManager::new(&config, combo_entries())
            .with_store(&config, Arc::new(ai_providers::InMemoryStore::new()), cooldowns);
        let next = fb
            .next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1")
            .unwrap();
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("groq", "groq/compound"),
            "an expired cooldown does not exclude the entry"
        );
    }

    #[test]
    fn without_a_store_cooldowns_still_work_in_process() {
        let config = isolated_config();
        let fb = FallbackManager::new(&config, combo_entries());
        fb.record_failure("groq", "groq/compound");
        // groq is cooling and nvidia is the current entry, so the walk lands on
        // poolside — no store needed for that.
        let next = fb
            .next_entry("nvidia", "nvidia/llama-3.3-nemotron-super-49b-v1")
            .unwrap();
        assert_eq!(
            (next.provider.as_str(), next.model.as_str()),
            ("poolside", "poolside/laguna-xs-2.1")
        );
    }

    #[test]
    fn routing_is_off_unless_a_store_is_attached() {
        let config = AppConfig::default();
        let plain = FallbackManager::new(&config, combo_entries());
        assert!(!plain.routing_enabled(), "no store, no scoring");
        let with_store = FallbackManager::new(&config, combo_entries()).with_store(
            &config,
            Arc::new(ai_providers::InMemoryStore::new()),
            Arc::new(CooldownRegistry::new()),
        );
        assert!(with_store.routing_enabled());

        let mut disabled = AppConfig::default();
        disabled.routing.enabled = false;
        let ordered = FallbackManager::new(&disabled, combo_entries()).with_store(
            &disabled,
            Arc::new(ai_providers::InMemoryStore::new()),
            Arc::new(CooldownRegistry::new()),
        );
        assert!(
            !ordered.routing_enabled(),
            "routing.enabled = false restores the ordered walk"
        );
    }

    /// The score picks the healthy entry, and the decision is explainable.
    #[tokio::test]
    async fn the_router_prefers_the_healthier_combo_entry() {
        use ai_providers::TelemetryStore as _;
        let config = AppConfig {
            combos: HashMap::from([(
                "coding".to_string(),
                vec![
                    crate::config::ComboEntry {
                        provider: "groq".into(),
                        model: "groq/compound".into(),
                    },
                    crate::config::ComboEntry {
                        provider: "poolside".into(),
                        model: "poolside/laguna-xs-2.1".into(),
                    },
                ],
            )]),
            ..Default::default()
        };
        let store = Arc::new(ai_providers::InMemoryStore::new());
        // groq has been failing all window; poolside has no history.
        for _ in 0..10 {
            store
                .record_attempt(&ai_providers::AttemptRecord {
                    provider: "groq".into(),
                    model: "groq/compound".into(),
                    at: SystemTime::now(),
                    outcome: ai_providers::Outcome::Failed(FailureKind::Overloaded),
                    latency: None,
                    session_id: None,
                    turn_id: None,
                })
                .await
                .unwrap();
        }
        let fb = FallbackManager::new(&config, combo_entries()).with_store(
            &config,
            store,
            Arc::new(CooldownRegistry::new()),
        );
        let candidates = candidates_for_combo(&config, "coding");
        assert_eq!(candidates.len(), 2);
        let decision = fb.choose(Some("coding"), candidates).await.unwrap();
        assert_eq!(
            decision.chosen.provider,
            "poolside",
            "the failing entry must lose"
        );
        assert!(!decision.is_configured_order());
        assert_eq!(fb.last_decision().unwrap().chosen, decision.chosen);
        let table = explain_decision(&decision);
        assert!(table.contains("candidate"), "{table}");
        assert!(table.contains("* poolside/poolside/laguna-xs-2.1"), "{table}");
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
        assert_eq!(model_ids(&combos_provider), vec!["coding"]);
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
                models: vec![crate::config::ModelRef::Simple("openrouter/auto".into())],
                pacing: None,
            },
        );
        // A config-file `models` list is shown verbatim, even when the free
        // filter is on.
        let openrouter = provider(&config, "openrouter").unwrap();
        assert_eq!(model_ids(&openrouter), vec!["openrouter/auto"]);
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
                        let args =
                            serde_json::json!({ "file_path": file_path, "content": content });
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
                    let response =
                        format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}");
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
                    let response =
                        format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}");
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

        let mut config = AppConfig {
            provider: "mock".into(),
            model: "mock/test-model".into(),
            working_dir: dir.clone(),
            permissions_mode: "allow_all".into(),
            ..Default::default()
        };
        config.providers.insert(
            "mock".into(),
            ProviderConfigEntry {
                base_url: Some(base_url),
                api_key: Some("test-key".into()),
                models: vec!["mock/test-model".into()],
                pacing: None,
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

        outcome
            .expect("agent run did not complete in time")
            .unwrap();
        assert!(saw_final_text, "agent never produced the final reply text");
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
        let mut config = AppConfig {
            provider: "mock".into(),
            model: "mock/test-model".into(),
            working_dir: std::env::temp_dir(),
            permissions_mode: "allow_all".into(),
            ..Default::default()
        };
        config.providers.insert(
            "mock".into(),
            ProviderConfigEntry {
                base_url: Some(base_url),
                api_key: Some("test-key".into()),
                models: vec!["mock/test-model".into()],
                pacing: None,
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
        while let Ok(Ok(activity)) =
            tokio::time::timeout(Duration::from_millis(500), sub_rx.recv()).await
        {
            match activity {
                crate::subagents::SubAgentActivity::Started { agent_type, .. } => {
                    assert_eq!(agent_type, "researcher-web");
                    saw_started = true;
                }
                crate::subagents::SubAgentActivity::Finished { .. } => saw_finished = true,
                _ => {}
            }
        }
        assert!(saw_started, "no Started event forwarded to subscribers");
        assert!(saw_finished, "no Finished event forwarded to subscribers");
    }

    /// A fake OpenAI-compatible SSE server that streams an ox-alpha-style
    /// reasoning response: thinking in `reasoning` (with `reasoning_details`
    /// alongside, exactly like the live captures) followed by the answer in
    /// `content`. Returns the base URL to point a provider at.
    async fn reasoning_mock_server() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
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

                    let chunk = |delta: serde_json::Value, finish: Option<&str>| {
                        serde_json::json!({
                            "id": "chatcmpl-r",
                            "object": "chat.completion.chunk",
                            "choices": [{
                                "index": 0,
                                "delta": delta,
                                "finish_reason": finish,
                            }]
                        })
                    };
                    let parts = [
                        chunk(
                            serde_json::json!({
                                "role": "assistant",
                                "reasoning": "Let me ",
                                "reasoning_details": [{
                                    "type": "reasoning.text",
                                    "text": "Let me ",
                                    "format": "unknown",
                                    "index": 0,
                                }],
                                "content": "",
                            }),
                            None,
                        ),
                        chunk(
                            serde_json::json!({
                                "reasoning": "think about it.",
                                "reasoning_details": [{
                                    "type": "reasoning.text",
                                    "text": "think about it.",
                                    "format": "unknown",
                                    "index": 0,
                                }],
                            }),
                            None,
                        ),
                        chunk(serde_json::json!({ "content": "The answer is 42." }), None),
                        chunk(serde_json::json!({}), Some("stop")),
                    ];
                    let mut body = String::new();
                    for part in parts {
                        body.push_str(&format!("data: {}\n\n", part));
                    }
                    body.push_str("data: [DONE]\n\n");
                    let response =
                        format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}");
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{addr}")
    }

    /// End-to-end: an ox-alpha-style stream (thinking in `reasoning` +
    /// `reasoning_details`, answer in `content`) must reach the agent event
    /// stream as `ThinkingDelta` events — the exact events the ACP server
    /// maps to `agent_thought_chunk` — with the thinking kept out of the
    /// answer text.
    #[tokio::test]
    async fn ox_alpha_thinking_streams_as_agent_events() {
        use cersei::events::AgentEvent;
        use std::time::Duration;

        let base_url = reasoning_mock_server().await;
        // Build the agent directly with an empty tool set: the runner's
        // no-tool-use nudge (F-08) only fires when tools are available, and a
        // nudge would force a second turn. This keeps the run at exactly one
        // turn so the thinking/answer split is asserted against a single
        // streamed response.
        let provider = OpenAi::builder()
            .base_url(&base_url)
            .api_key("test-key")
            .model("mock/test-model")
            .build()
            .unwrap();
        let agent = Agent::builder()
            .provider(provider)
            .model("mock/test-model")
            .working_dir(std::env::temp_dir())
            .build()
            .unwrap();
        let mut stream = std::sync::Arc::new(agent).run_stream("What is the answer?");

        let mut thinking = String::new();
        let mut text = String::new();
        let outcome = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(event) = stream.next().await {
                match event {
                    AgentEvent::ThinkingDelta(t) => thinking.push_str(&t),
                    AgentEvent::TextDelta(t) => text.push_str(&t),
                    AgentEvent::Complete(_) => return Ok(()),
                    AgentEvent::Error(e) => return Err(anyhow::anyhow!("agent error: {e}")),
                    _ => {}
                }
            }
            Err(anyhow::anyhow!("agent stream ended without completing"))
        })
        .await;

        outcome
            .expect("agent run did not complete in time")
            .unwrap();
        assert_eq!(
            thinking, "Let me think about it.",
            "thinking deltas must stream in order and be kept separate"
        );
        assert_eq!(
            text, "The answer is 42.",
            "answer text must not contain the thinking"
        );
    }

    /// A minimal `Resolved` for tool-wiring tests (fields are otherwise
    /// unused by `agent_tools`, which only inspects tool names).
    fn resolved_stub() -> Resolved {
        Resolved {
            provider: "test".into(),
            model: "test/test-model".into(),
            base_url: "http://127.0.0.1:1".into(),
            api_key: "test-key".into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            extra_body: None,
        }
    }

    /// The reasoning-aware provider path: the field resolved from
    /// `config.model_families` must reach the OpenAi provider (which reads
    /// `delta.reasoning` in its SSE reader instead of letting it drop).
    #[test]
    fn openai_provider_carries_the_reasoning_field() {
        let resolved = resolved_stub();

        // A configured reasoning family is carried into the provider...
        let provider = openai_provider(
            &resolved,
            cersei::provider::ReasoningField::Field("reasoning"),
        )
        .unwrap();
        assert_eq!(
            provider.reasoning_field(),
            cersei::provider::ReasoningField::Field("reasoning")
        );

        // ...an unlisted model auto-detects...
        let provider = openai_provider(&resolved, cersei::provider::ReasoningField::Auto).unwrap();
        assert_eq!(
            provider.reasoning_field(),
            cersei::provider::ReasoningField::Auto
        );

        // ...and `plain` opts out.
        let provider = openai_provider(&resolved, cersei::provider::ReasoningField::Off).unwrap();
        assert_eq!(
            provider.reasoning_field(),
            cersei::provider::ReasoningField::Off
        );
    }

    /// The sink handles `agent_tools` needs (parent/followups/events).
    fn empty_handles() -> (
        crate::subagents::ParentHandle,
        crate::subagents::FollowupSink,
        crate::subagents::SubAgentEventSink,
    ) {
        (
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(Vec::new())),
            None,
        )
    }

    #[test]
    fn agent_tools_tui_mode_keeps_builtin_read_write_edit() {
        // TUI / `-p` mode passes `fs_reader: None` — the built-in
        // Read/Write/Edit tools must remain (not the ACP client wrappers).
        let resolved = resolved_stub();
        let (parent, followups, events) = empty_handles();
        let tools = agent_tools(
            &resolved,
            parent,
            followups,
            events,
            None,
            false,
            cersei::provider::ReasoningField::Auto,
            None,
            None,
            None,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"Read"), "built-in Read should be present");
        assert!(
            names.contains(&"ask_user"),
            "ask_user tool should be present"
        );
        assert!(names.contains(&"Write"), "built-in Write should be present");
        assert!(names.contains(&"Edit"), "built-in Edit should be present");
        // The Client* wrappers must not be registered in TUI mode.
        // (They share the Read/Write/Edit names, so we verify by type via
        // description — the built-ins and wrappers have identical names but
        // the wrappers are distinct structs; here we assert the count is 1
        // for each name, proving no duplicate/wrapper sneaked in.)
        for name in ["Read", "Write", "Edit"] {
            assert_eq!(
                names.iter().filter(|&&n| n == name).count(),
                1,
                "{name} should appear exactly once in TUI mode"
            );
        }
        // Exa is only reachable through the WebSearch backend chain — the
        // standalone ExaSearch tool must never reach the model.
        assert!(
            !names.contains(&"ExaSearch"),
            "standalone ExaSearch tool leaked into TUI mode: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|&&n| n == "WebSearch").count(),
            1,
            "WebSearch should appear exactly once in TUI mode"
        );
    }

    #[test]
    fn agent_tools_acp_mode_replaces_read_write_edit_with_client_wrappers() {
        // ACP server mode passes `fs_reader: Some(...)` — the built-in
        // Read/Write/Edit are replaced by the client-aware wrappers.
        use crate::subagents::{AcpFs, AcpFsSink};
        use async_trait::async_trait;

        // A stub `AcpFs` that advertises both capabilities, so the wrappers
        // will consult it (we don't execute any tools here — we only verify
        // the wiring by tool name count).
        struct CapableFs;
        #[async_trait]
        impl AcpFs for CapableFs {
            fn supports_read_text_file(&self) -> bool {
                true
            }
            fn supports_write_text_file(&self) -> bool {
                true
            }
            async fn read_text_file(
                &self,
                _session_id: &str,
                _path: &str,
                _line: Option<u32>,
                _limit: Option<u32>,
            ) -> anyhow::Result<String> {
                Ok(String::new())
            }
            async fn write_text_file(
                &self,
                _session_id: &str,
                _path: &str,
                _content: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }
        let fs: AcpFsSink = Some(Arc::new(CapableFs) as Arc<dyn AcpFs>);

        let resolved = resolved_stub();
        let (parent, followups, events) = empty_handles();
        let tools = agent_tools(
            &resolved,
            parent,
            followups,
            events,
            fs,
            false,
            cersei::provider::ReasoningField::Auto,
            None,
            None,
            None,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        // The wrappers register under the same Read/Write/Edit names, so the
        // built-ins were removed and exactly one of each name remains.
        for name in ["Read", "Write", "Edit"] {
            assert_eq!(
                names.iter().filter(|&&n| n == name).count(),
                1,
                "{name} should appear exactly once in ACP mode (wrapper replaces built-in)"
            );
        }
        // Grep is always replaced by RgSearchTool (which registers under the
        // "Grep" name) — sanity check the non-conditional replacement still
        // happens in both modes: exactly one Grep entry remains.
        assert_eq!(
            names.iter().filter(|&&n| n == "Grep").count(),
            1,
            "built-in Grep replaced by RgSearchTool (same name) in both modes"
        );
        // Same for search: cersei's built-in ExaSearch is dropped and our
        // WebSearch replacement is the only search tool the model sees.
        assert!(
            !names.contains(&"ExaSearch"),
            "standalone ExaSearch tool leaked into ACP mode: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|&&n| n == "WebSearch").count(),
            1,
            "WebSearch should appear exactly once in ACP mode"
        );
    }

    // `/models` parsing moved to `ai_providers::discovery` with its own tests.

    #[test]
    fn catalog_is_built_from_the_merged_config() {
        let config = isolated_config();
        let merged = catalog(&config, Arc::new(ai_providers::NullStore));
        // The built-ins and the model families from the config reach the
        // library as specs, in the same order the agent displays them.
        let names: Vec<&str> = merged
            .providers()
            .iter()
            .map(|spec| spec.name.as_str())
            .collect();
        assert_eq!(
            names,
            providers(&config)
                .iter()
                .map(|provider| provider.name.as_str())
                .collect::<Vec<_>>()
        );
        let groq = merged.provider("groq").expect("groq is built in");
        assert_eq!(groq.models[0].id, "groq/compound");
        assert_eq!(groq.models[0].family, None);

        let mut with_family = config.clone();
        with_family
            .model_families
            .insert("groq/compound".into(), "plain".into());
        let family_catalog = catalog(&with_family, Arc::new(ai_providers::NullStore));
        assert_eq!(
            family_catalog
                .provider("groq")
                .unwrap()
                .models[0]
                .family
                .as_deref(),
            Some("plain")
        );
    }

    #[test]
    fn provider_pacing_ceilings_reach_the_library() {
        let mut config = isolated_config();
        config.providers.insert(
            "groq".into(),
            ProviderConfigEntry {
                pacing: Some(crate::config::PacingEntryConfig {
                    requests_per_minute: Some(60),
                    max_concurrency: Some(2),
                    min_interval_ms: Some(500),
                    min_cooldown_seconds: None,
                    max_cooldown_seconds: None,
                }),
                ..Default::default()
            },
        );
        let catalog = catalog(&config, Arc::new(ai_providers::NullStore));
        let pacing = &catalog.provider("groq").unwrap().pacing;
        assert_eq!(pacing.requests_per_minute, Some(60));
        assert_eq!(pacing.max_concurrency, Some(2));
        assert_eq!(pacing.min_interval, Some(Duration::from_millis(500)));
        assert_eq!(
            catalog.limiter("groq").effective_interval(),
            Duration::from_secs(1),
            "60rpm is one second per request"
        );
    }
}

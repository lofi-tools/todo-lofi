//! Provider registry: built-in providers, config-file overrides, API key
//! resolution, and shared agent construction.
//!
//! Built-in providers (all OpenAI-compatible): poolside, openrouter, groq and
//! nvidia nim. Their base URL / api key / models can be overridden — or new
//! providers added — via the config file's `[providers.NAME]` section.
//!
//! An `api_key` value in config is either:
//! - `!command` — run the rest as a shell command and use its trimmed stdout,
//! - `env:VAR` — read the environment variable,
//! - a literal key.

use crate::config::AppConfig;
use anyhow::Context as _;
use cersei::tools::permissions::{AllowAll, AllowReadOnly};
use cersei::types::Message;
use cersei::{Agent, OpenAi};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
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
    pub tools: Vec<Box<dyn cersei::tools::Tool>>,
    pub cancel_token: CancellationToken,
    /// Use a read-only permission policy (denies modifying/executing tools).
    pub readonly: bool,
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
    ]
}

/// All providers in deterministic order: built-ins first, then config-file
/// additions (sorted). Config entries override built-in fields by name.
pub fn providers(config: &AppConfig) -> Vec<Provider> {
    let mut by_name: HashMap<String, Provider> = builtin_providers()
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();

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
        let builtin_rank = builtin_names().iter().position(|n| n == &p.name).unwrap_or(usize::MAX);
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

/// Resolve a provider + model into concrete base URL and API key.
pub fn resolve(config: &AppConfig, provider_name: &str, model: &str) -> anyhow::Result<Resolved> {
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

    let mut builder = Agent::builder()
        .provider(provider)
        .model(&resolved.model)
        .max_turns(params.max_turns)
        .working_dir(params.working_dir)
        .cancel_token(params.cancel_token)
        .with_messages(params.messages);
    if let Some(session_id) = params.session_id {
        builder = builder.session_id(session_id);
    }
    if !params.tools.is_empty() {
        builder = builder.tools(params.tools);
    }
    builder = if params.readonly {
        builder.permission_policy(AllowReadOnly)
    } else {
        builder.permission_policy(AllowAll)
    };

    Ok(Arc::new(builder.build().context("failed to build agent")?))
}

// ─── Runtime agent holder (TUI) ─────────────────────────────────────────────

/// Holds the live agent and lets the TUI switch provider/model at runtime.
pub struct AgentRuntime {
    inner: Mutex<AgentRuntimeInner>,
}

struct AgentRuntimeInner {
    agent: Arc<Agent>,
    provider: String,
    model: String,
    config: AppConfig,
}

impl AgentRuntime {
    pub fn new(config: &AppConfig) -> anyhow::Result<Self> {
        let (provider, model) = default_selection(config)?;
        let resolved = resolve(config, &provider, &model)?;
        let agent = build_agent(
            &resolved,
            BuildParams {
                working_dir: config.working_dir.clone(),
                max_turns: config.max_turns,
                session_id: None,
                messages: Vec::new(),
                tools: Vec::new(),
                cancel_token: CancellationToken::new(),
                readonly: false,
            },
        )?;
        Ok(Self {
            inner: Mutex::new(AgentRuntimeInner {
                agent,
                provider,
                model,
                config: config.clone(),
            }),
        })
    }

    pub fn agent(&self) -> Arc<Agent> {
        self.inner.lock().agent.clone()
    }

    pub fn current(&self) -> (String, String) {
        let g = self.inner.lock();
        (g.provider.clone(), g.model.clone())
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

    /// Rebuild the agent with a new provider/model.
    pub fn switch(&self, provider: &str, model: &str) -> anyhow::Result<()> {
        let (config, working_dir, max_turns) = {
            let g = self.inner.lock();
            (g.config.clone(), g.config.working_dir.clone(), g.config.max_turns)
        };
        let resolved = resolve(&config, provider, model)?;
        let agent = build_agent(
            &resolved,
            BuildParams {
                working_dir,
                max_turns,
                session_id: None,
                messages: Vec::new(),
                tools: Vec::new(),
                cancel_token: CancellationToken::new(),
                readonly: false,
            },
        )?;
        let mut g = self.inner.lock();
        g.agent = agent;
        g.provider = provider.to_string();
        g.model = model.to_string();
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
}

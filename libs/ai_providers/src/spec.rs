//! Provider specifications handed to the catalog by the agent.
//!
//! The library never reads a config file: agent-cli parses its config, maps it
//! into these structs, and builds a [`crate::Catalog`] from them.

use std::time::Duration;

/// One provider as the agent defines it.
#[derive(Debug, Clone)]
pub struct ProviderSpec {
    pub name: String,
    pub base_url: String,
    /// `!command`, `env:VAR`, or a literal key.
    pub api_key: String,
    pub models: Vec<ModelSpec>,
    pub pacing: PacingSpec,
}

impl ProviderSpec {
    /// A provider with no models and no pacing limits.
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            models: Vec::new(),
            pacing: PacingSpec::default(),
        }
    }

    pub fn with_models(mut self, models: impl IntoIterator<Item = ModelSpec>) -> Self {
        self.models = models.into_iter().collect();
        self
    }
}

/// One model on a provider, with optional per-model request parameters.
/// An unset parameter leaves the request untouched (the agent default applies).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub extra_body: Option<serde_json::Value>,
    /// Family name from the agent's `model_families` config, if any.
    pub family: Option<String>,
}

impl ModelSpec {
    /// A bare model id with no per-model parameters.
    pub fn bare(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            extra_body: None,
            family: None,
        }
    }
}

impl From<&str> for ModelSpec {
    fn from(id: &str) -> Self {
        Self::bare(id)
    }
}

impl From<String> for ModelSpec {
    fn from(id: String) -> Self {
        Self::bare(id)
    }
}

/// Client-side pacing limits for one provider. All optional: unset means "no
/// limit imposed", and the limiter learns from observed 429s instead.
#[derive(Debug, Clone, Default)]
pub struct PacingSpec {
    /// Ceiling on requests per minute.
    pub requests_per_minute: Option<u32>,
    /// Ceiling on concurrent in-flight requests.
    pub max_concurrency: Option<u32>,
    /// Floor on the gap between two requests.
    pub min_interval: Option<Duration>,
    /// Shortest cooldown applied after a failure.
    pub min_cooldown: Option<Duration>,
    /// Longest cooldown applied after a failure.
    pub max_cooldown: Option<Duration>,
}

/// A provider/model pair, used as the key for cooldowns and telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelKey {
    pub provider: String,
    pub model: String,
}

impl ModelKey {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

impl std::fmt::Display for ModelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// Token pricing for a model, per million tokens. Absent until the agent
/// supplies real numbers; the routing price term reads `0.0` when it is `None`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// A provider/model selection with the api key already resolved, plus the
/// model's configured request parameters (None = use the agent defaults).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub extra_body: Option<serde_json::Value>,
    /// The model's configured family name, if any.
    pub family: Option<String>,
    /// Pricing seam: always `None` while models are assumed free.
    pub price: Option<Price>,
}

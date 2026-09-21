//! The provider catalog: the agent's specs, merged and made usable.
//!
//! The agent parses its config, maps it into [`ProviderSpec`]s and hands them
//! here. The catalog owns everything that has to be shared between agent
//! instances — the per-provider limiters, the `/models` cache, the cooldown
//! registry — so a parent agent and its sub-agents pace against the same
//! provider budget instead of each believing it is alone.

use crate::discovery::fetch_models;
use crate::key::resolve_api_key;
use crate::pacing::ProviderLimiter;
use crate::spec::{ModelKey, ModelSpec, PacingSpec, ProviderSpec, Resolved};
use crate::store::{CooldownRegistry, StoreHandle};
use crate::transport::{ConfiguredProvider, PacedProvider};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// The merged provider set plus the shared process-wide provider state.
pub struct Catalog {
    providers: Vec<ProviderSpec>,
    store: StoreHandle,
    cooldowns: Arc<CooldownRegistry>,
    limiters: Mutex<HashMap<String, Arc<ProviderLimiter>>>,
    model_cache: Mutex<HashMap<String, Result<Vec<String>, String>>>,
}

impl Catalog {
    /// `specs` are the agent's providers in display order (already merged).
    pub fn new(specs: Vec<ProviderSpec>, store: StoreHandle) -> Self {
        Self {
            providers: specs,
            store,
            cooldowns: Arc::new(CooldownRegistry::new()),
            limiters: Mutex::new(HashMap::new()),
            model_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Build a catalog with a pre-seeded cooldown registry (the agent seeds it
    /// from the database's still-active cooldowns at startup).
    pub fn with_cooldowns(
        specs: Vec<ProviderSpec>,
        store: StoreHandle,
        cooldowns: Arc<CooldownRegistry>,
    ) -> Self {
        Self {
            providers: specs,
            store,
            cooldowns,
            limiters: Mutex::new(HashMap::new()),
            model_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn providers(&self) -> &[ProviderSpec] {
        &self.providers
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderSpec> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// The store every component writes through.
    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// The process-wide cooldown registry, shared by the walk and the router.
    pub fn cooldowns(&self) -> &Arc<CooldownRegistry> {
        &self.cooldowns
    }

    /// The limiter for a provider, created on first use from its spec's
    /// pacing. Shared across every agent instance in this process.
    pub fn limiter(&self, provider: &str) -> Arc<ProviderLimiter> {
        let spec = self
            .provider(provider)
            .map(|p| p.pacing.clone())
            .unwrap_or_default();
        self.limiter_with(provider, spec)
    }

    fn limiter_with(&self, provider: &str, spec: PacingSpec) -> Arc<ProviderLimiter> {
        let mut limiters = self.limiters.lock();
        limiters
            .entry(provider.to_string())
            .or_insert_with(|| ProviderLimiter::new(provider, spec))
            .clone()
    }

    /// Every model id on a provider, in configured order.
    pub fn model_ids(&self, provider: &str) -> Vec<String> {
        self.provider(provider)
            .map(|p| p.models.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    }

    /// All `(provider, model)` pairs, in catalog order.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.providers
            .iter()
            .flat_map(|p| {
                p.models
                    .iter()
                    .map(move |m| (p.name.clone(), m.id.clone()))
            })
            .collect()
    }

    /// The first configured model of a provider.
    pub fn default_model(&self, provider: &str) -> anyhow::Result<String> {
        let p = self
            .provider(provider)
            .ok_or_else(|| anyhow::anyhow!("unknown provider: {provider}"))?;
        p.models
            .first()
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow::anyhow!("provider '{provider}' has no models configured"))
    }

    /// User-facing `provider/model` id. Model ids are wire-level ids that may
    /// already carry the provider prefix, so only add it when it is absent.
    pub fn display_model_id(provider: &str, model: &str) -> String {
        if model.starts_with(&format!("{provider}/")) {
            model.to_string()
        } else {
            format!("{provider}/{model}")
        }
    }

    /// Resolve a concrete (non-virtual) provider/model pair into its base URL,
    /// api key and request parameters.
    pub fn resolve(&self, provider: &str, model: &str) -> anyhow::Result<Resolved> {
        let p = self.provider(provider).ok_or_else(|| {
            let known = self
                .providers
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!("unknown provider '{provider}'; known providers: {known}")
        })?;
        let api_key = resolve_api_key(&p.api_key)
            .map_err(|error| error.context(format!("resolving api_key for provider '{provider}'")))?;
        let found = find_model(p, provider, model);
        Ok(Resolved {
            provider: p.name.clone(),
            model: model.to_string(),
            base_url: p.base_url.clone(),
            api_key,
            max_tokens: found.and_then(|m| m.max_tokens),
            temperature: found.and_then(|m| m.temperature),
            top_p: found.and_then(|m| m.top_p),
            extra_body: found.and_then(|m| m.extra_body.clone()),
            family: found.and_then(|m| m.family.clone()),
            // Pricing is a seam: models are assumed free in v1.
            price: None,
        })
    }

    /// A provider transport wrapped with pacing and telemetry.
    pub fn provider_impl(
        &self,
        provider: &str,
        model: &str,
        scope: Arc<crate::store::AttemptScope>,
        reasoning: cersei::provider::ReasoningField,
    ) -> anyhow::Result<Box<dyn cersei::provider::Provider>> {
        let resolved = self.resolve(provider, model)?;
        let spec = self.provider(provider).ok_or_else(|| {
            anyhow::anyhow!("unknown provider '{provider}'")
        })?;
        let inner = ConfiguredProvider::build(&resolved, reasoning)?;
        Ok(Box::new(PacedProvider::new(
            resolved,
            spec.api_key.clone(),
            reasoning,
            inner,
            self.limiter(provider),
            self.store.clone(),
            self.cooldowns.clone(),
            scope,
        )))
    }

    /// The cached `/models` response for a provider, if it was already fetched
    /// this run (None when not fetched yet).
    pub fn cached_models(&self, provider: &str) -> Option<Result<Vec<String>, String>> {
        self.model_cache.lock().get(provider).cloned()
    }

    /// Fetch a provider's full model list, caching the response for the whole
    /// process run. Only successes are cached, so a transient failure is
    /// retried next time.
    pub async fn fetch_models_cached(&self, provider: &str) -> Result<Vec<String>, String> {
        if let Some(cached) = self.cached_models(provider) {
            return cached;
        }
        let resolved = self
            .resolve(provider, "")
            .map_err(|error| error.to_string())?;
        let result = fetch_models(&resolved.base_url, &resolved.api_key)
            .await
            .map_err(|error| error.to_string());
        if let Ok(models) = &result {
            self.model_cache
                .lock()
                .insert(provider.to_string(), Ok(models.clone()));
        }
        result
    }

    /// Look up a model's configured request parameters without resolving a key.
    pub fn request_params(&self, provider: &str, model: &str) -> ModelParams {
        let Some(p) = self.provider(provider) else {
            return ModelParams::default();
        };
        let found = find_model(p, provider, model);
        ModelParams {
            max_tokens: found.and_then(|m| m.max_tokens),
            temperature: found.and_then(|m| m.temperature),
            family: found.and_then(|m| m.family.clone()),
        }
    }
}

/// The subset of a model's config the agent needs before resolving a key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelParams {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub family: Option<String>,
}

/// Find a model by any of the three forms a caller may hold: the bare id, the
/// id already carrying its provider prefix, or the provider-prefixed form of a
/// bare id. This is the invariant the agent's own `find_model` had.
fn find_model<'a>(
    provider: &'a ProviderSpec,
    provider_name: &str,
    model: &str,
) -> Option<&'a ModelSpec> {
    provider.models.iter().find(|candidate| {
        candidate.id == model
            || candidate.id == format!("{provider_name}/{model}")
            || format!("{provider_name}/{}", candidate.id) == model
    })
}

/// The key the [`Catalog`] uses for cooldowns and telemetry.
pub fn model_key(provider: &str, model: &str) -> ModelKey {
    ModelKey::new(provider, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ModelSpec;
    use crate::store::{InMemoryStore, NullStore};

    fn catalog() -> Catalog {
        let specs = vec![
            ProviderSpec::new("groq", "https://api.groq.com/openai/v1", "env:GROQ_KEY").with_models(
                [
                    ModelSpec {
                        family: Some("plain".into()),
                        max_tokens: Some(2048),
                        temperature: Some(0.2),
                        ..ModelSpec::bare("groq/compound")
                    },
                    ModelSpec::bare("openai/gpt-oss-120b"),
                ],
            ),
            ProviderSpec::new("combo-land", "", "")
                .with_models([ModelSpec::bare("a"), ModelSpec::bare("b")]),
        ];
        Catalog::new(specs, Arc::new(NullStore))
    }

    #[test]
    fn lookups_keep_configured_order_and_the_double_prefix_quirk() {
        let catalog = catalog();
        assert_eq!(catalog.providers().len(), 2);
        assert_eq!(catalog.provider("groq").unwrap().base_url, "https://api.groq.com/openai/v1");
        assert!(catalog.provider("nope").is_none());
        assert_eq!(
            catalog.model_ids("groq"),
            vec!["groq/compound", "openai/gpt-oss-120b"]
        );
        assert_eq!(
            catalog.entries(),
            vec![
                ("groq".to_string(), "groq/compound".to_string()),
                ("groq".to_string(), "openai/gpt-oss-120b".to_string()),
                ("combo-land".to_string(), "a".to_string()),
                ("combo-land".to_string(), "b".to_string()),
            ]
        );
        assert_eq!(catalog.default_model("groq").unwrap(), "groq/compound");
        assert!(catalog.default_model("missing").is_err());
        // A wire id that already carries the provider prefix is not prefixed
        // twice; a bare one is.
        assert_eq!(
            Catalog::display_model_id("groq", "groq/compound"),
            "groq/compound"
        );
        assert_eq!(Catalog::display_model_id("groq", "compound"), "groq/compound");
    }

    #[test]
    fn resolve_returns_parameters_and_never_leaks_into_errors() {
        let catalog = catalog();
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("GROQ_KEY", "sk-test") };
        let resolved = catalog.resolve("groq", "groq/compound").unwrap();
        assert_eq!(resolved.api_key, "sk-test");
        assert_eq!(resolved.max_tokens, Some(2048));
        assert_eq!(resolved.temperature, Some(0.2));
        assert_eq!(resolved.family.as_deref(), Some("plain"));
        assert!(resolved.price.is_none(), "pricing is a seam in v1");

        // A model without its own entry uses the agent defaults.
        let resolved = catalog.resolve("groq", "openai/gpt-oss-120b").unwrap();
        assert!(resolved.max_tokens.is_none());
        assert!(resolved.temperature.is_none());

        let err = catalog.resolve("nope", "x").unwrap_err().to_string();
        assert!(err.contains("known providers"), "{err}");
    }

    #[test]
    fn request_params_lookup_handles_prefixed_ids() {
        let catalog = catalog();
        let params = catalog.request_params("groq", "compound");
        assert_eq!(params.max_tokens, Some(2048));
        assert_eq!(
            catalog.request_params("groq", "unknown").max_tokens,
            None
        );
    }

    #[test]
    fn limiters_are_shared_per_provider_name() {
        let catalog = catalog();
        let first = catalog.limiter("groq");
        let second = catalog.limiter("groq");
        assert!(Arc::ptr_eq(&first, &second), "one budget per provider");
        let other = catalog.limiter("combo-land");
        assert!(!Arc::ptr_eq(&first, &other));
        // An unknown provider gets a default (unlimited) limiter rather than
        // an error: pacing must never be a reason a request cannot be tried.
        assert_eq!(catalog.limiter("ghost").effective_interval(), std::time::Duration::ZERO);
    }

    #[tokio::test]
    async fn provider_impl_builds_a_wrapped_transport() {
        // SAFETY: test-only mutation of a dedicated env var.
        unsafe { std::env::set_var("GROQ_KEY", "sk-test") };
        let catalog = Catalog::new(
            vec![ProviderSpec::new("groq", "https://example.invalid/v1", "env:GROQ_KEY")
                .with_models([ModelSpec::bare("m")])],
            Arc::new(InMemoryStore::new()),
        );
        let provider = catalog
            .provider_impl(
                "groq",
                "m",
                Arc::new(crate::store::AttemptScope::anonymous()),
                cersei::provider::ReasoningField::Auto,
            )
            .unwrap();
        assert_eq!(provider.name(), "groq");
        assert!(catalog.provider_impl("nope", "m", Arc::new(crate::store::AttemptScope::anonymous()), cersei::provider::ReasoningField::Auto).is_err());
    }
}

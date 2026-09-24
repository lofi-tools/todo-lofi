//! Per-provider kinds: which discovery shape a gateway speaks, and how a wire
//! model id is normalized into the canonical id the rest of the library uses.
//!
//! One type per provider lives in [`crate::providers`], but nothing about a
//! provider's *endpoint* does: base URLs, api-key specs and the roster itself
//! stay in the agent (see the library spec's D2/D3), and arrive here as
//! arguments. A kind is looked up by the provider's configured name, so a
//! gateway whose name is unknown to this module falls back to
//! [`GenericOpenAi`](crate::providers::generic::GenericOpenAi) — the
//! OpenAI-compatible default — instead of failing.
//!
//! The reason a kind exists at all is that three things vary between gateways
//! that all claim to be "OpenAI-compatible": the `/models` response shape, the
//! model-id form the gateway accepts back (`models/gemini-…`, `provider/model`
//! with a `:free` suffix, and so on), and where in the id the family/version
//! boundary sits.

pub mod generic;
pub mod google;
pub mod groq;
pub mod kiosapi;
pub mod nvidia;
pub mod ollama;
pub mod opencode_zen;
pub mod openrouter;
pub mod orcarouter;
pub mod poolside;
pub mod tokenrouter;

/// The `/models` response shape a provider's discovery endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryShape {
    /// `{"data": [{"id": …}]}` — the OpenAI-compatible default.
    OpenAiModels,
    /// OpenRouter's richer list: `data[]` entries also carry `context_length`,
    /// `created` and a `pricing` object. The plain shape parses it fine; this
    /// variant exists to keep the extra fields.
    OpenRouterModels,
    /// Google AI Studio: `{"models": [{"name": "models/gemini-…"}]}`, where the
    /// id is the *last* path segment of `name`.
    GoogleModels,
    /// Ollama tags: `{"models": [{"name": "gpt-oss:120b"}]}`.
    OllamaTags,
}

/// What the library needs to know about one provider to discover and normalize
/// its models. Endpoints and credentials are never here.
pub trait ProviderKind: Send + Sync {
    /// The provider name, matching the name the agent's roster uses.
    fn name(&self) -> &'static str;

    /// The discovery shape this gateway's model list speaks.
    fn discovery(&self) -> DiscoveryShape;

    /// Normalize a raw wire model id into the canonical id used in the catalog,
    /// telemetry and benchmark joins. The default keeps the raw id, which is
    /// correct for a gateway whose wire ids are already canonical.
    fn canonical_id(&self, raw: &str) -> String {
        raw.to_string()
    }
}

/// Every provider kind this module knows, in lookup order.
pub fn provider_kinds() -> &'static [&'static dyn ProviderKind] {
    &[
        &openrouter::OpenRouter,
        &tokenrouter::TokenRouter,
        &orcarouter::OrcaRouter,
        &kiosapi::KiosApi,
        &groq::Groq,
        &nvidia::Nvidia,
        &poolside::Poolside,
        &ollama::Ollama,
        &opencode_zen::OpenCodeZen,
        &google::Google,
    ]
}

/// The kind registered under `name`, or the OpenAI-compatible default when the
/// name is not one this module specializes. A gateway added to the agent's
/// roster therefore works without a change here.
pub fn provider_kind(name: &str) -> &'static dyn ProviderKind {
    provider_kinds()
        .iter()
        .find(|kind| kind.name() == name)
        .copied()
        .unwrap_or(&generic::GenericOpenAi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_kind_is_findable_by_its_own_name() {
        for kind in provider_kinds() {
            let found = provider_kind(kind.name());
            assert_eq!(found.name(), kind.name(), "lookup must be by name");
        }
    }

    #[test]
    fn an_unknown_gateway_falls_back_to_the_openai_shape() {
        let kind = provider_kind("a-gateway-added-tomorrow");
        assert_eq!(kind.name(), "generic");
        assert_eq!(kind.discovery(), DiscoveryShape::OpenAiModels);
        // The default normalization is the identity, so an unknown gateway's
        // ids are trusted as-is rather than mangled.
        assert_eq!(kind.canonical_id("some/model-1"), "some/model-1");
    }

    #[test]
    fn providers_that_differ_declare_their_own_shape() {
        assert_eq!(
            provider_kind("openrouter").discovery(),
            DiscoveryShape::OpenRouterModels
        );
        assert_eq!(
            provider_kind("google").discovery(),
            DiscoveryShape::GoogleModels
        );
        assert_eq!(
            provider_kind("ollama").discovery(),
            DiscoveryShape::OllamaTags
        );
        // A plain OpenAI-compatible gateway keeps the default shape rather
        // than repeating it.
        assert_eq!(
            provider_kind("groq").discovery(),
            DiscoveryShape::OpenAiModels
        );
    }

    #[test]
    fn google_strips_the_models_path_prefix_from_its_ids() {
        let google = provider_kind("google");
        assert_eq!(
            google.canonical_id("models/gemini-3.8-flash"),
            "gemini-3.8-flash"
        );
        // An id that is already bare is left alone.
        assert_eq!(google.canonical_id("gemini-3.8-flash"), "gemini-3.8-flash");
    }
}

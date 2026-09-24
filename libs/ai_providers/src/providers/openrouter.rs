//! OpenRouter: a meta-router over many vendors.
//!
//! Its `/models` list is the OpenAI shape plus per-model `context_length`,
//! `created` and `pricing`, which is why it gets [`DiscoveryShape::OpenRouterModels`]
//! instead of the plain default. A `:free` suffix is a genuinely different
//! wire model (different pricing and rate limits), so it is kept as part of the
//! canonical id rather than stripped.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct OpenRouter;

impl ProviderKind for OpenRouter {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenRouterModels
    }
}

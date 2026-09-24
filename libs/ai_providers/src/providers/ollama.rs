//! Ollama, whose model list is its own tag shape (`{"models": [{"name": …}]}`)
//! rather than the OpenAI `/models` shape.
//!
//! Ids keep their `name:tag` form (`gpt-oss:120b`); the tag is part of the
//! model's identity, not a variant to strip.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct Ollama;

impl ProviderKind for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OllamaTags
    }
}

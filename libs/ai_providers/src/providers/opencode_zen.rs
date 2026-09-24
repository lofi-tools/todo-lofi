//! OpenCode Zen: an OpenAI-compatible gateway over frontier vendors' models.
//!
//! Ids are `vendor/model` on the wire, which is already the canonical form.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct OpenCodeZen;

impl ProviderKind for OpenCodeZen {
    fn name(&self) -> &'static str {
        "opencode-zen"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

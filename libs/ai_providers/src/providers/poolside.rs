//! Poolside's inference endpoint: OpenAI-compatible, no discovery deviations.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct Poolside;

impl ProviderKind for Poolside {
    fn name(&self) -> &'static str {
        "poolside"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

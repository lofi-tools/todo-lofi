//! NVIDIA's hosted NIM endpoint: OpenAI-compatible, no discovery deviations.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct Nvidia;

impl ProviderKind for Nvidia {
    fn name(&self) -> &'static str {
        "nvidia"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

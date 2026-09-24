//! kiosapi: a New API-style OpenAI-compatible gateway, no discovery deviations.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct KiosApi;

impl ProviderKind for KiosApi {
    fn name(&self) -> &'static str {
        "kiosapi"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

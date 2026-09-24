//! tokenrouter: a unified gateway over many vendors' chat models.
//!
//! Its wire ids carry the vendor prefix (`deepseek/deepseek-v4-pro-0813`) and a
//! `-free` suffix marks the free tier, so both are part of the canonical id.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct TokenRouter;

impl ProviderKind for TokenRouter {
    fn name(&self) -> &'static str {
        "tokenrouter"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

//! The OpenAI-compatible default: any gateway whose model list is the plain
//! `{"data": [{"id": …}]}` shape and whose wire ids are already canonical.
//!
//! Registered under the name `generic` and used as the fallback for a provider
//! name this module does not specialize, so adding a gateway to the agent's
//! roster never requires a change here.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct GenericOpenAi;

impl ProviderKind for GenericOpenAi {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

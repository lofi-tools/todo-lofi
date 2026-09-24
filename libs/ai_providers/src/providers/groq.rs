//! Groq: an OpenAI-compatible inference host with no discovery deviations.
//!
//! It keeps the default shape and default id normalization deliberately — the
//! file exists so a Groq-specific quirk has an obvious home, not because Groq
//! needs one today.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct Groq;

impl ProviderKind for Groq {
    fn name(&self) -> &'static str {
        "groq"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

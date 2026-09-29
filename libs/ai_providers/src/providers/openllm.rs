//! OpenLLM (openllm.sh): an OpenAI-compatible gateway over the models a user's
//! connected Claude/ChatGPT/Grok/Kimi/Cursor accounts (or bring-your-own API
//! keys) already provide.
//!
//! Model ids are either tier aliases (`lite`, `plus`, `ultra`) or a concrete
//! `provider/model` id; both are already the wire form, so the default
//! normalization (keep the raw id) is correct.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct OpenLlm;

impl ProviderKind for OpenLlm {
    fn name(&self) -> &'static str {
        "openllm"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

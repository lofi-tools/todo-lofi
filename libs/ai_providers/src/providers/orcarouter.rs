//! OrcaRouter: a zero-markup meta-router whose `/models` list is the plain
//! OpenAI shape, with its own `auto`/`free`/`fusion` pseudo-models on top.
//!
//! The pseudo-model ids are not real models and carry no family or benchmark
//! data, so [`crate::families::infer_model_version`] returns `None` for them;
//! nothing else treats them specially, because to the wire they are ordinary
//! model ids.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct OrcaRouter;

impl ProviderKind for OrcaRouter {
    fn name(&self) -> &'static str {
        "orcarouter"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::OpenAiModels
    }
}

//! Google AI Studio (the Gemini API's OpenAI-compatible endpoint).
//!
//! Two differences from the plain shape: its model list is `{"models": […]}`,
//! and each entry's id is the *last* segment of a `models/…` resource name, so
//! the wire id cannot be used verbatim as a request id.

use crate::providers::{DiscoveryShape, ProviderKind};

pub struct Google;

impl ProviderKind for Google {
    fn name(&self) -> &'static str {
        "google"
    }

    fn discovery(&self) -> DiscoveryShape {
        DiscoveryShape::GoogleModels
    }

    fn canonical_id(&self, raw: &str) -> String {
        raw.strip_prefix("models/").unwrap_or(raw).to_string()
    }
}

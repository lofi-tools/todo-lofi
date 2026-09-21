//! Provider machinery for OpenAI-compatible gateways.
//!
//! The library owns what is true of *any* OpenAI-compatible gateway: the
//! provider/model registry, model discovery, api-key spec resolution, the
//! cersei-facing transport, model-family quirks, failure classification,
//! client-side pacing and the routing score. The *config* — which providers
//! exist, their base URLs, model lists and combos — stays with the agent, which
//! maps it into [`ProviderSpec`]s and builds a [`Catalog`] from them.
//!
//! Storage is a port, not a dependency: [`TelemetryStore`] is implemented by
//! the agent (agent-cli backs it with SQLite), and [`NullStore`] /
//! [`InMemoryStore`] ship here so the crate works with no database at all.

pub mod catalog;
pub mod discovery;
pub mod failure;
pub mod key;
pub mod pacing;
pub mod quirks;
pub mod routing;
pub mod spec;
pub mod store;
pub mod transport;

pub use catalog::{Catalog, ModelParams};
pub use discovery::fetch_models;
pub use failure::{FailureKind, classify, classify_message, classify_status};
pub use key::{resolve_api_key, resolve_value_spec};
pub use pacing::{ProviderLimiter, RateLimitHint};
pub use quirks::{
    Family, ResponseFormat, Segment, SegmentKind, classify_delta, family_by_name, family_for_model,
    format_for, parse_sse, reasoning_field_for,
};
pub use routing::{Candidate, CandidateScore, Router, RoutingConfig, RoutingDecision};
pub use spec::{ModelKey, ModelSpec, PacingSpec, Price, ProviderSpec, Resolved};
pub use store::{
    AttemptRecord, AttemptScope, AttemptSummary, CooldownEntry, CooldownRegistry, InMemoryStore,
    NullStore, Outcome, StoreHandle, TelemetryStore,
};
pub use transport::{ConfiguredProvider, PacedProvider};

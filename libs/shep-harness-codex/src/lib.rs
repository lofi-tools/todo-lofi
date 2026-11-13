//! Codex app-server adapter.

pub mod codex;
#[cfg(any(test, feature = "live-test-support"))]
pub mod live_codex;

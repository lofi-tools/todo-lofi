//! The harness contract plus the adapters that implement it.
//!
//! Each adapter lives in its own crate, so editing one connector recompiles
//! that connector only. This crate is the single import path the shell and the
//! core use: `agent_harness::codex::CodexHarness` and friends.

pub use shep_harness_core::*;

pub mod acp {
    pub use shep_harness_acp::acp::*;
}

pub mod codex {
    pub use shep_harness_codex::codex::*;
}

pub mod freebuff {
    pub use shep_harness_freebuff::freebuff::*;
}

pub mod freebuff_capture {
    pub use shep_harness_freebuff::freebuff_capture::*;
}

#[cfg(any(test, feature = "live-test-support"))]
pub mod live_codex {
    pub use shep_harness_codex::live_codex::*;
}

pub mod opencode {
    pub use shep_harness_opencode::opencode::*;
}

pub mod pi {
    pub use shep_harness_pi::pi::*;
}

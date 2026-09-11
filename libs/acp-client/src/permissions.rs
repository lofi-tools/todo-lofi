//! Permission policy for `session/request_permission`.
//!
//! The agent offers the options; the client either surfaces them to the user or
//! answers automatically. There is no client-side "remember denial": sticky
//! semantics already exist in the protocol as the agent's own
//! `RejectAlways` / `AllowAlways` option kinds, and duplicating them here would
//! double-remember.

use agent_client_protocol::schema::v1::{PermissionOption, PermissionOptionId, PermissionOptionKind};
use futures::channel::oneshot;

/// How the client answers permission requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Ask the user, showing the agent's options.
    #[default]
    Prompt,
    /// Answer without asking, using the agent's strongest allow option.
    AutoApprove,
}

/// The client's answer to a permission request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Selected(PermissionOptionId),
    Cancelled,
}

/// The channel the UI answers a permission request on, handed over with
/// [`AcpEvent::PermissionRequested`](crate::AcpEvent::PermissionRequested).
///
/// Wrapping the sender keeps the async channel type out of the UI crate's
/// dependencies.
pub struct PermissionReply(oneshot::Sender<PermissionDecision>);

impl PermissionReply {
    pub fn new(sender: oneshot::Sender<PermissionDecision>) -> Self {
        Self(sender)
    }

    /// Answer the agent. Returns the decision back when nobody is listening
    /// any more (the connection or the turn ended first).
    pub fn answer(self, decision: PermissionDecision) -> Result<(), PermissionDecision> {
        self.0.send(decision)
    }
}

impl std::fmt::Debug for PermissionReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PermissionReply")
    }
}

impl ApprovalMode {
    /// The strongest allow option the agent offered.
    ///
    /// Prefers `AllowAlways` over `AllowOnce`. When the agent offers no allow
    /// option at all, returns `None` so the request is surfaced to the user
    /// rather than silently rejected.
    pub fn strongest_allow(options: &[PermissionOption]) -> Option<&PermissionOption> {
        options
            .iter()
            .find(|option| matches!(option.kind, PermissionOptionKind::AllowAlways))
            .or_else(|| {
                options
                    .iter()
                    .find(|option| matches!(option.kind, PermissionOptionKind::AllowOnce))
            })
    }

    /// The decision to make without asking, if any.
    pub fn automatic_decision(options: &[PermissionOption]) -> Option<PermissionDecision> {
        let option = Self::strongest_allow(options)?;
        Some(PermissionDecision::Selected(option.option_id.clone()))
    }
}

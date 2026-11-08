//! Permission policy for `session/request_permission`.
//!
//! Per-project tool approval in Zed's `tool_permissions` shape: a global
//! default plus per-tool rules with `always_allow` / `always_deny` /
//! `always_confirm` regex patterns, e.g.
//!
//! ```json
//! {
//!     "default": "confirm",
//!     "tools": {
//!         "execute": {
//!             "default": "confirm",
//!             "always_allow": [{ "pattern": "^cargo\\s+(build|test|check)" }],
//!             "always_deny": [{ "pattern": "rm\\s+-rf\\s+(/|~)" }]
//!         }
//!     }
//! }
//! ```
//!
//! The agent offers the options; the client either answers automatically from
//! the policy or surfaces the request to the user. Tools are keyed by the ACP
//! tool kind (`read`, `edit`, `execute`, …); patterns match against the tool
//! call's title and input.

use std::collections::BTreeMap;

use agent_client_protocol::schema::v1::{
    ContentBlock, PermissionOption, PermissionOptionId, PermissionOptionKind, ToolCallContent,
    ToolCallUpdate, ToolKind,
};
use futures::channel::oneshot;
use serde::{Deserialize, Serialize};

/// One approval level, mirroring Zed's `tool_permissions` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRule {
    /// Ask the user every time.
    #[default]
    Confirm,
    /// Answer without asking, using the agent's strongest allow option.
    Allow,
    /// Answer without asking, using the agent's strongest reject option.
    Deny,
}

/// A regex matched against the tool call's title and input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternRule {
    pub pattern: String,
}

/// Per-tool overrides, mirroring Zed's `tools.<name>` entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRule {
    #[serde(default)]
    pub default: PermissionRule,
    #[serde(default)]
    pub always_allow: Vec<PatternRule>,
    #[serde(default)]
    pub always_deny: Vec<PatternRule>,
    #[serde(default)]
    pub always_confirm: Vec<PatternRule>,
}

/// Per-project approval policy, mirroring Zed's `tool_permissions` block.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissions {
    #[serde(default)]
    pub default: PermissionRule,
    #[serde(default)]
    pub tools: BTreeMap<String, ToolRule>,
}

/// The policy's answer for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    Confirm,
}

impl ToolPermissions {
    /// Evaluate the policy for a tool call. Priority mirrors Zed:
    /// `always_deny` beats everything, then `always_confirm`, then
    /// `always_allow`; the tool default beats the global default.
    /// Invalid deny or confirmation patterns cannot produce an automatic allow.
    pub fn decide(&self, tool: &str, haystack: &str) -> Verdict {
        if let Some(rule) = self.tools.get(tool) {
            let (deny, invalid_deny) = matches_any(&rule.always_deny, haystack);
            if deny {
                return Verdict::Deny;
            }
            let (confirm, invalid_confirm) = matches_any(&rule.always_confirm, haystack);
            if confirm {
                return Verdict::Confirm;
            }
            if invalid_deny || invalid_confirm {
                return match rule.default {
                    PermissionRule::Deny => Verdict::Deny,
                    PermissionRule::Allow | PermissionRule::Confirm => Verdict::Confirm,
                };
            }
            if matches_any(&rule.always_allow, haystack).0 {
                return Verdict::Allow;
            }
            return resolve(rule.default);
        }
        resolve(self.default)
    }

    /// Set a tool-level default, creating the tool entry when needed. This is
    /// what the permission card's "Always allow / Always deny" buttons write.
    pub fn set_tool_default(&mut self, tool: &str, rule: PermissionRule) {
        self.tools.entry(tool.to_string()).or_default().default = rule;
    }
}

fn resolve(rule: PermissionRule) -> Verdict {
    match rule {
        PermissionRule::Allow => Verdict::Allow,
        PermissionRule::Deny => Verdict::Deny,
        PermissionRule::Confirm => Verdict::Confirm,
    }
}

fn matches_any(rules: &[PatternRule], haystack: &str) -> (bool, bool) {
    let mut matched = false;
    let mut invalid = false;
    for rule in rules {
        match regex::Regex::new(&rule.pattern) {
            Ok(expression) => matched |= expression.is_match(haystack),
            Err(_) => {
                tracing::debug!("invalid permission pattern");
                invalid = true;
            }
        }
    }
    (matched, invalid)
}

/// The rule key for a tool call: the ACP tool kind in snake_case (`read`,
/// `edit`, `execute`, …), `other` when the agent sends none.
pub fn tool_key(call: &ToolCallUpdate) -> String {
    match call.fields.kind {
        Some(ToolKind::Read) => "read",
        Some(ToolKind::Edit) => "edit",
        Some(ToolKind::Delete) => "delete",
        Some(ToolKind::Move) => "move",
        Some(ToolKind::Search) => "search",
        Some(ToolKind::Execute) => "execute",
        Some(ToolKind::Think) => "think",
        Some(ToolKind::Fetch) => "fetch",
        Some(ToolKind::SwitchMode) => "switch_mode",
        Some(ToolKind::Other) | None => "other",
        _ => "other",
    }
    .to_string()
}

/// Text the `always_*` patterns match against: title, raw input and any text
/// content of the tool call.
pub fn haystack(call: &ToolCallUpdate) -> String {
    let mut parts = Vec::new();
    if let Some(title) = call
        .fields
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
    {
        parts.push(title.to_string());
    }
    if let Some(input) = call.fields.raw_input.as_ref() {
        parts.push(input.to_string());
    }
    if let Some(content) = call.fields.content.as_ref() {
        for item in content {
            if let ToolCallContent::Content(block) = item
                && let ContentBlock::Text(text) = &block.content
            {
                parts.push(text.text.clone());
            }
        }
    }
    parts.join("\n")
}

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

/// The strongest reject option the agent offered, mirroring
/// [`strongest_allow`]: `RejectAlways` over `RejectOnce`, `None` when the
/// agent offers no reject option.
pub fn strongest_reject(options: &[PermissionOption]) -> Option<&PermissionOption> {
    options
        .iter()
        .find(|option| matches!(option.kind, PermissionOptionKind::RejectAlways))
        .or_else(|| {
            options
                .iter()
                .find(|option| matches!(option.kind, PermissionOptionKind::RejectOnce))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn allow_deny(allow: &str, deny: &str) -> ToolPermissions {
        ToolPermissions {
            default: PermissionRule::Confirm,
            tools: BTreeMap::from([(
                "execute".to_string(),
                ToolRule {
                    default: PermissionRule::Confirm,
                    always_allow: vec![PatternRule {
                        pattern: allow.to_string(),
                    }],
                    always_deny: vec![PatternRule {
                        pattern: deny.to_string(),
                    }],
                    always_confirm: Vec::new(),
                },
            )]),
        }
    }

    #[test]
    fn deny_beats_allow_and_confirm_beats_allow() {
        let mut policy = allow_deny("^cargo\\s+test", "rm -rf");
        // Deny wins even when an allow pattern also matches.
        assert_eq!(
            policy.decide("execute", "cargo test; rm -rf /tmp/x"),
            Verdict::Deny
        );
        // Allow matches on its own.
        assert_eq!(policy.decide("execute", "cargo test --all"), Verdict::Allow);
        // Nothing matches: the tool default (confirm) applies.
        assert_eq!(policy.decide("execute", "cargo build"), Verdict::Confirm);
        // Other tools fall back to the global default.
        assert_eq!(policy.decide("read", "cargo test --all"), Verdict::Confirm);
        // Confirm patterns beat allow patterns.
        if let Some(rule) = policy.tools.get_mut("execute") {
            rule.always_confirm.push(PatternRule {
                pattern: "sudo".to_string(),
            });
        }
        assert_eq!(
            policy.decide("execute", "sudo cargo test"),
            Verdict::Confirm
        );
    }

    #[test]
    fn tool_default_beats_global_default_and_invalid_patterns_are_ignored() {
        let mut policy = ToolPermissions {
            default: PermissionRule::Deny,
            ..Default::default()
        };
        policy.set_tool_default("read", PermissionRule::Allow);
        assert_eq!(policy.decide("read", "anything"), Verdict::Allow);
        assert_eq!(policy.decide("edit", "anything"), Verdict::Deny);
        // An invalid regex never matches and never panics.
        policy
            .tools
            .get_mut("read")
            .expect("read rule")
            .always_allow
            .push(PatternRule {
                pattern: "([invalid".to_string(),
            });
        assert_eq!(policy.decide("read", "anything"), Verdict::Allow);
    }

    #[test]
    fn tool_confirmation_overrides_global_allow() {
        let mut policy = ToolPermissions {
            default: PermissionRule::Allow,
            ..Default::default()
        };
        policy.set_tool_default("execute", PermissionRule::Confirm);
        assert_eq!(policy.decide("execute", "cargo test"), Verdict::Confirm);
        assert_eq!(policy.decide("read", "anything"), Verdict::Allow);
    }

    #[test]
    fn invalid_restrictive_patterns_do_not_allow_a_request()
    -> Result<(), Box<dyn std::error::Error>> {
        for deny_pattern in [true, false] {
            let mut policy = allow_deny(".*", "never-match");
            let rule = policy
                .tools
                .get_mut("execute")
                .ok_or("missing execute rule")?;
            rule.default = PermissionRule::Allow;
            let invalid = PatternRule {
                pattern: "(invalid".to_owned(),
            };
            if deny_pattern {
                rule.always_deny.push(invalid);
            } else {
                rule.always_confirm.push(invalid);
            }
            assert_eq!(policy.decide("execute", "cargo test"), Verdict::Confirm);
            policy
                .tools
                .get_mut("execute")
                .ok_or("missing execute rule")?
                .default = PermissionRule::Deny;
            assert_eq!(policy.decide("execute", "cargo test"), Verdict::Deny);
            if let Some(rule) = policy.tools.get_mut("execute") {
                rule.default = PermissionRule::Allow;
                rule.always_deny.push(PatternRule {
                    pattern: "cargo".to_owned(),
                });
            }
            assert_eq!(policy.decide("execute", "cargo test"), Verdict::Deny);
        }
        Ok(())
    }

    #[test]
    fn invalid_pattern_does_not_expose_its_contents_in_logs()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(logs.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            assert_eq!(
                matches_any(
                    &[PatternRule {
                        pattern: "(fixture-private-value".to_owned(),
                    }],
                    "anything"
                ),
                (false, true)
            );
        });
        let output = String::from_utf8(logs.lock().map_err(|error| error.to_string())?.clone())?;
        assert!(output.contains("invalid permission pattern"));
        assert!(!output.contains("fixture-private-value"));
        Ok(())
    }
}

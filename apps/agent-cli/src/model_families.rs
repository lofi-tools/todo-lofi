//! Built-in model families: quirks shared by models of the same family no
//! matter which provider serves them.
//!
//! [`config.model_families`](crate::config::AppConfig::model_families) is the
//! user-configurable half of this story: it maps an exact model id to a family
//! name. This module is the built-in half: families defined here apply by
//! model-id pattern (`hy3` matches any wire id containing `hy3`), so a model
//! family is reusable across providers without any config — a hy3 served by
//! b.ai, siliconflow, or tencent all resolve to the same quirks.
//!
//! A quirk exists only when omitting it misbehaves on the wire or in the
//! agent loop:
//!
//! - `reasoning_field` — where the family streams thinking in OpenAI-style
//!   deltas (`reasoning_content` on deepseek/hunyuan-family models). Wrong or
//!   missing means the thinking is glued onto the answer or dropped.
//! - `no_tool_nudge` — whether the agent runner may force a follow-up turn
//!   when the model answers without calling a tool. Off for chat-style
//!   families: a forced tool round after a complete answer is what makes an
//!   adaptive-thinking model like hy3 look like it "won't stop".

/// A built-in model family and its quirks.
pub struct ModelFamily {
    /// The family name, also usable as a `model_families` config value.
    pub name: &'static str,
    /// Substrings that identify a model id as part of this family. The match
    /// is case-insensitive so a provider's casing never hides the quirks.
    pub markers: &'static [&'static str],
    /// The OpenAI-compatible delta field this family streams thinking in
    /// (None = plain text, no thinking field).
    pub reasoning_field: Option<&'static str>,
    /// Whether the agent runner may fire the once-per-session "you answered
    /// without using any tools" nudge for this family.
    pub no_tool_nudge: bool,
}

/// Tencent Hunyuan 3 (hy3): adaptive thinking streamed in
/// `reasoning_content` (deepseek-style), and chat-style answers that should
/// not be forced into a tool-use round.
pub const HY3: ModelFamily = ModelFamily {
    name: "hy3",
    markers: &["hy3"],
    reasoning_field: Some("reasoning_content"),
    no_tool_nudge: false,
};

/// All built-in families, in match order.
pub fn builtin_families() -> &'static [ModelFamily] {
    &[HY3]
}

/// The built-in family whose markers match `model` (case-insensitive), if any.
pub fn family_for_model(model: &str) -> Option<&'static ModelFamily> {
    let lower = model.to_ascii_lowercase();
    builtin_families()
        .iter()
        .find(|f| f.markers.iter().any(|m| lower.contains(m)))
}

/// The built-in family with this name, if any (for `model_families` config
/// values that name a built-in family).
pub fn family_by_name(name: &str) -> Option<&'static ModelFamily> {
    builtin_families().iter().find(|f| f.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hy3_matches_bare_and_provider_qualified_ids() {
        // The same family applies no matter which provider serves it.
        for id in [
            "hy3",
            "b.ai/hy3",
            "siliconflow/hy3",
            "HY3",
            "hunyuan-hy3-pro",
        ] {
            let family = family_for_model(id).expect("hy3 id must resolve");
            assert_eq!(family.name, "hy3");
            assert_eq!(family.reasoning_field, Some("reasoning_content"));
            assert!(!family.no_tool_nudge);
        }
    }

    #[test]
    fn unrelated_models_have_no_builtin_family() {
        for id in [
            "gpt-4o",
            "deepseek/deepseek-chat",
            "stealth/ox-alpha",
            "claude-sonnet-5",
        ] {
            assert!(family_for_model(id).is_none(), "{id} must not match hy3");
        }
    }

    #[test]
    fn builtin_family_name_is_a_valid_config_value() {
        assert_eq!(family_by_name("hy3").map(|f| f.name), Some("hy3"));
        assert!(family_by_name("nope").is_none());
    }
}

//! Model-family quirks: what the *agent loop* must know about a model that the
//! wire shape does not already cover.
//!
//! Two layers of quirks exist in this workspace, deliberately separate:
//!
//! - **Wire-shape quirks** (thinking form, temperature policy, schema dialect,
//!   context window) live in cersei's `ProviderQuirks` — the provider API
//!   forces them and cersei already exposes them.
//! - **Stream/loop quirks** live here: where a family streams its thinking
//!   (so it is delimited from the answer instead of glued on or dropped), and
//!   whether the runner may force a follow-up tool round.
//!
//! Built-in families apply by model-id pattern, so the same model served by
//! any provider resolves to the same quirks. The agent's `model_families`
//! config can name a built-in family or one of the generic ones.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A built-in model family and its quirks.
pub struct Family {
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
pub const HY3: Family = Family {
    name: "hy3",
    markers: &["hy3"],
    reasoning_field: Some("reasoning_content"),
    no_tool_nudge: false,
};

/// All built-in families, in match order.
pub fn builtin_families() -> &'static [Family] {
    &[HY3]
}

/// The built-in family whose markers match `model` (case-insensitive), if any.
pub fn family_for_model(model: &str) -> Option<&'static Family> {
    let lower = model.to_ascii_lowercase();
    builtin_families()
        .iter()
        .find(|f| f.markers.iter().any(|m| lower.contains(m)))
}

/// The built-in family with this name, if any (for `model_families` config
/// values that name a built-in family).
pub fn family_by_name(name: &str) -> Option<&'static Family> {
    builtin_families().iter().find(|f| f.name == name)
}

// ─── Response formats ───────────────────────────────────────────────────────

/// A chunk of model output, classified as thinking or answer text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Thinking,
    Text,
}

/// Where a model family carries its thinking relative to answer text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Thinking is not delimited from text: everything on the wire is answer
    /// text. The default for models with no known reasoning format.
    Plain,
    /// Thinking arrives in a dedicated delta field (e.g. `reasoning` on
    /// openrouter reasoning models, `reasoning_content` on deepseek-style
    /// endpoints); answer text is the usual `content` field.
    ReasoningField { field: String },
}

/// All known response formats, keyed by the family name used in
/// `config.model_families` (e.g. `"stealth/ox-alpha": "reasoning"`). Built-in
/// family names resolve through the registry so a config value can name a
/// built-in family.
pub fn family(name: &str) -> Option<ResponseFormat> {
    match name {
        "plain" => Some(ResponseFormat::Plain),
        "reasoning" => Some(ResponseFormat::ReasoningField {
            field: "reasoning".into(),
        }),
        "reasoning_content" => Some(ResponseFormat::ReasoningField {
            field: "reasoning_content".into(),
        }),
        _ => family_by_name(name)
            .and_then(|f| f.reasoning_field)
            .map(|field| ResponseFormat::ReasoningField {
                field: field.into(),
            }),
    }
}

/// Resolve the response format for a model id: an explicit `model_families`
/// entry (`family_name`) wins, then the built-in family matching the model id.
/// Defaults to [`ResponseFormat::Plain`]; an unknown configured family name
/// logs a warning so config typos are visible instead of silently changing
/// behavior.
pub fn format_for(family_name: Option<&str>, model: &str) -> ResponseFormat {
    if let Some(name) = family_name {
        return match family(name) {
            Some(format) => format,
            None => {
                eprintln!(
                    "warning: unknown response format family '{name}' for model '{model}' — \
                     treating it as plain text"
                );
                ResponseFormat::Plain
            }
        };
    }
    match family_for_model(model) {
        Some(family) => match family.reasoning_field {
            Some(field) => ResponseFormat::ReasoningField {
                field: field.into(),
            },
            None => ResponseFormat::Plain,
        },
        None => ResponseFormat::Plain,
    }
}

/// Resolve which delta field the provider should read thinking from.
///
/// Unlike [`format_for`], an *unlisted* model stays on
/// [`Auto`](cersei::provider::ReasoningField::Auto): reasoning models send
/// reasoning fields only when they think, so auto-detection is the safest
/// default for the live path (plain models never emit those fields). A model
/// explicitly configured `plain` opts out entirely.
pub fn reasoning_field_for(
    family_name: Option<&str>,
    model: &str,
) -> cersei::provider::ReasoningField {
    use cersei::provider::ReasoningField;
    if let Some(name) = family_name {
        return match name {
            "reasoning" => ReasoningField::Field("reasoning"),
            "reasoning_content" => ReasoningField::Field("reasoning_content"),
            "plain" => ReasoningField::Off,
            other => match family_by_name(other) {
                Some(family) => match family.reasoning_field {
                    Some(field) => ReasoningField::Field(field),
                    None => ReasoningField::Off,
                },
                None => {
                    eprintln!(
                        "warning: unknown response format family '{other}' for model '{model}' — \
                         auto-detecting reasoning fields"
                    );
                    ReasoningField::Auto
                }
            },
        };
    }
    match family_for_model(model) {
        Some(family) => match family.reasoning_field {
            Some(field) => ReasoningField::Field(field),
            None => ReasoningField::Off,
        },
        None => ReasoningField::Auto,
    }
}

/// Classify one SSE `delta` object (the `choices[0].delta` of a chat
/// completion chunk) into zero or more [`Segment`]s under the given format.
/// A delta may carry both fields (rare), producing two segments.
pub fn classify_delta(format: &ResponseFormat, delta: &Value) -> Vec<Segment> {
    let mut segments = Vec::new();
    match format {
        ResponseFormat::Plain => {
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
            {
                segments.push(Segment {
                    kind: SegmentKind::Text,
                    text: text.to_string(),
                });
            }
        }
        ResponseFormat::ReasoningField { field } => {
            if let Some(thinking) = delta.get(field).and_then(Value::as_str)
                && !thinking.is_empty()
            {
                segments.push(Segment {
                    kind: SegmentKind::Thinking,
                    text: thinking.to_string(),
                });
            }
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
            {
                segments.push(Segment {
                    kind: SegmentKind::Text,
                    text: text.to_string(),
                });
            }
        }
    }
    segments
}

/// Parse a full SSE body (newline-delimited `data:` JSON lines, as produced
/// by an OpenAI-compatible `/chat/completions` stream) into [`Segment`]s,
/// concatenating same-kind deltas. Lines that aren't `data:` payloads
/// (keep-alive comments, `[DONE]`) are skipped.
pub fn parse_sse(format: &ResponseFormat, body: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let Some(delta) = chunk.pointer("/choices/0/delta") else {
            continue;
        };
        for segment in classify_delta(format, delta) {
            match out.last_mut() {
                Some(prev) if prev.kind == segment.kind => prev.text.push_str(&segment.text),
                _ => out.push(segment),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ox_alpha() -> ResponseFormat {
        ResponseFormat::ReasoningField {
            field: "reasoning".into(),
        }
    }

    #[test]
    fn hy3_matches_bare_and_provider_qualified_ids() {
        for id in ["hy3", "b.ai/hy3", "siliconflow/hy3", "HY3", "hunyuan-hy3-pro"] {
            let family = family_for_model(id).expect("hy3 id must resolve");
            assert_eq!(family.name, "hy3");
            assert_eq!(family.reasoning_field, Some("reasoning_content"));
            assert!(!family.no_tool_nudge);
        }
    }

    #[test]
    fn unrelated_models_have_no_builtin_family() {
        for id in ["gpt-4o", "deepseek/deepseek-chat", "stealth/ox-alpha", "claude-sonnet-5"] {
            assert!(family_for_model(id).is_none(), "{id} must not match hy3");
        }
    }

    #[test]
    fn builtin_family_name_is_a_valid_config_value() {
        assert_eq!(family_by_name("hy3").map(|f| f.name), Some("hy3"));
        assert!(family_by_name("nope").is_none());
    }

    #[test]
    fn plain_format_treats_content_as_text_only() {
        let delta = serde_json::json!({
            "content": "hello",
            "reasoning": "hidden reasoning",
            "role": "assistant"
        });
        let segments = classify_delta(&ResponseFormat::Plain, &delta);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, SegmentKind::Text);
        assert_eq!(segments[0].text, "hello");
    }

    #[test]
    fn reasoning_field_splits_thinking_from_text() {
        let delta = serde_json::json!({
            "content": "the answer",
            "reasoning": "the thinking",
            "role": "assistant"
        });
        let segments = classify_delta(&ox_alpha(), &delta);
        assert_eq!(
            segments,
            vec![
                Segment {
                    kind: SegmentKind::Thinking,
                    text: "the thinking".into()
                },
                Segment {
                    kind: SegmentKind::Text,
                    text: "the answer".into()
                },
            ]
        );
    }

    #[test]
    fn reasoning_field_handles_single_field_deltas() {
        let thinking_only = serde_json::json!({ "reasoning": "think", "content": "" });
        let segments = classify_delta(&ox_alpha(), &thinking_only);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, SegmentKind::Thinking);

        let text_only = serde_json::json!({ "content": "answer" });
        let segments = classify_delta(&ox_alpha(), &text_only);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, SegmentKind::Text);
    }

    #[test]
    fn parse_sse_concatenates_same_kind_deltas() {
        let body = "\ndata: {\"choices\":[{\"delta\":{\"reasoning\":\"The user \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"reasoning\":\"wants a list.\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"1. one\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"\\n2. two\"}}]}\n\ndata: [DONE]\n";
        let segments = parse_sse(&ox_alpha(), body);
        assert_eq!(
            segments,
            vec![
                Segment {
                    kind: SegmentKind::Thinking,
                    text: "The user wants a list.".into()
                },
                Segment {
                    kind: SegmentKind::Text,
                    text: "1. one\n2. two".into()
                },
            ]
        );
    }

    #[test]
    fn parse_sse_skips_non_data_lines() {
        let body =
            ": keep-alive comment\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let segments = parse_sse(&ox_alpha(), body);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "hi");
    }

    #[test]
    fn format_for_resolves_families_from_config() {
        assert_eq!(
            format_for(Some("reasoning"), "stealth/ox-alpha"),
            ResponseFormat::ReasoningField {
                field: "reasoning".into()
            }
        );
        assert_eq!(
            format_for(Some("reasoning_content"), "deepseek/deepseek-chat"),
            ResponseFormat::ReasoningField {
                field: "reasoning_content".into()
            }
        );
        // Unlisted models and unknown family names fall back to Plain.
        assert_eq!(format_for(None, "groq/compound"), ResponseFormat::Plain);
        assert_eq!(format_for(Some("nope"), "unknown-model"), ResponseFormat::Plain);
    }

    #[test]
    fn reasoning_field_for_maps_families_to_provider_fields() {
        use cersei::provider::ReasoningField;
        assert_eq!(
            reasoning_field_for(Some("reasoning"), "stealth/ox-alpha"),
            ReasoningField::Field("reasoning")
        );
        assert_eq!(
            reasoning_field_for(Some("reasoning_content"), "deepseek/deepseek-chat"),
            ReasoningField::Field("reasoning_content")
        );
        assert_eq!(
            reasoning_field_for(Some("plain"), "groq/compound"),
            ReasoningField::Off
        );
        // Unlisted models auto-detect.
        assert_eq!(
            reasoning_field_for(None, "some/other-model"),
            ReasoningField::Auto
        );
    }

    #[test]
    fn builtin_hy3_family_applies_without_config() {
        use cersei::provider::ReasoningField;
        assert_eq!(
            format_for(None, "hy3"),
            ResponseFormat::ReasoningField {
                field: "reasoning_content".into()
            }
        );
        assert_eq!(
            format_for(None, "b.ai/hy3"),
            ResponseFormat::ReasoningField {
                field: "reasoning_content".into()
            }
        );
        assert_eq!(
            reasoning_field_for(None, "hy3"),
            ReasoningField::Field("reasoning_content")
        );
        // An explicit config entry still wins over the built-in default.
        assert_eq!(format_for(Some("plain"), "hy3"), ResponseFormat::Plain);
        assert_eq!(reasoning_field_for(Some("plain"), "hy3"), ReasoningField::Off);
    }

    #[test]
    fn config_can_name_a_builtin_family() {
        use cersei::provider::ReasoningField;
        assert_eq!(
            format_for(Some("hy3"), "b.ai/hunyuan-3-pro"),
            ResponseFormat::ReasoningField {
                field: "reasoning_content".into()
            }
        );
        assert_eq!(
            reasoning_field_for(Some("hy3"), "b.ai/hunyuan-3-pro"),
            ReasoningField::Field("reasoning_content")
        );
    }
}

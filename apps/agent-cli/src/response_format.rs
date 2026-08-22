//! Per-model-family response formats: how a provider's streamed deltas are
//! split into thinking blocks vs. answer text.
//!
//! OpenAI-compatible providers differ in how they delimit reasoning from the
//! final answer. The canonical OpenAI-compatible shape (used by cersei's
//! `OpenAi` provider) reads only `delta.content`; reasoning models like
//! openrouter's `stealth/ox-alpha` stream their thinking in a separate
//! `delta.reasoning` field (and `reasoning_details`), so without per-family
//! knowledge the thinking either vanishes or is glued onto the answer as
//! plain text — the "response blocks are not well delimited" symptom.
//!
//! A [`ResponseFormat`] names where thinking lives on the wire. The
//! [`Segment`]s it produces (thinking vs. text) are what the ACP/TUI layers
//! render as `agent_thought_chunk` vs. `agent_message_chunk`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
/// `config.model_families` (e.g. `"stealth/ox-alpha": "reasoning"`).
pub fn family(name: &str) -> Option<ResponseFormat> {
    match name {
        "plain" => Some(ResponseFormat::Plain),
        "reasoning" => Some(ResponseFormat::ReasoningField {
            field: "reasoning".into(),
        }),
        "reasoning_content" => Some(ResponseFormat::ReasoningField {
            field: "reasoning_content".into(),
        }),
        _ => None,
    }
}

/// Resolve the response format for a model id from `config.model_families`,
/// defaulting to [`ResponseFormat::Plain`] when the model is unlisted or the
/// family name is unknown (an unknown name logs a warning so config typos are
/// visible instead of silently changing behavior).
pub fn format_for(config: &crate::config::AppConfig, model: &str) -> ResponseFormat {
    match config.model_families.get(model) {
        Some(name) => match family(name) {
            Some(format) => format,
            None => {
                eprintln!(
                    "warning: unknown response format family '{name}' for model '{model}' — \
                     treating it as plain text"
                );
                ResponseFormat::Plain
            }
        },
        None => ResponseFormat::Plain,
    }
}

/// Resolve which delta field the provider should read thinking from, for a
/// model id from `config.model_families`. This is the config-driven half of
/// the reasoning-aware provider path: it drives the OpenAI-compatible SSE
/// reader (via `OpenAiBuilder::reasoning_field`) so `delta.reasoning` is
/// captured before cersei's reader would drop it.
///
/// Unlike [`format_for`], an *unlisted* model stays on [`Auto`]
/// (cersei::provider::ReasoningField::Auto): reasoning models send reasoning
/// fields only when they think, so auto-detection is the safest default for
/// the live path (plain models never emit those fields). A model explicitly
/// configured `plain` opts out entirely.
pub fn reasoning_field_for(
    config: &crate::config::AppConfig,
    model: &str,
) -> cersei::provider::ReasoningField {
    use cersei::provider::ReasoningField;
    match config.model_families.get(model) {
        Some(name) => match name.as_str() {
            "reasoning" => ReasoningField::Field("reasoning"),
            "reasoning_content" => ReasoningField::Field("reasoning_content"),
            "plain" => ReasoningField::Off,
            other => {
                eprintln!(
                    "warning: unknown response format family '{other}' for model '{model}' — \
                     auto-detecting reasoning fields"
                );
                ReasoningField::Auto
            }
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
        let Some(data) = line.strip_prefix("data:") else { continue };
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
    use crate::config::AppConfig;

    fn ox_alpha() -> ResponseFormat {
        ResponseFormat::ReasoningField {
            field: "reasoning".into(),
        }
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
                Segment { kind: SegmentKind::Thinking, text: "the thinking".into() },
                Segment { kind: SegmentKind::Text, text: "the answer".into() },
            ]
        );
    }

    #[test]
    fn reasoning_field_handles_single_field_deltas() {
        // A delta that only carries reasoning (typical mid-thinking chunk).
        let thinking_only = serde_json::json!({ "reasoning": "think", "content": "" });
        let segments = classify_delta(&ox_alpha(), &thinking_only);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, SegmentKind::Thinking);

        // A delta that only carries content (the model switched to answering).
        let text_only = serde_json::json!({ "content": "answer" });
        let segments = classify_delta(&ox_alpha(), &text_only);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, SegmentKind::Text);
    }

    #[test]
    fn parse_sse_concatenates_same_kind_deltas() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"The user \"}}]}\n\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"wants a list.\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"1. one\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"\\n2. two\"}}]}\n\
data: [DONE]\n";
        let segments = parse_sse(&ox_alpha(), body);
        assert_eq!(
            segments,
            vec![
                Segment {
                    kind: SegmentKind::Thinking,
                    text: "The user wants a list.".into()
                },
                Segment { kind: SegmentKind::Text, text: "1. one\n2. two".into() },
            ]
        );
    }

    #[test]
    fn parse_sse_skips_non_data_lines() {
        let body = ": keep-alive comment\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let segments = parse_sse(&ox_alpha(), body);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "hi");
    }

    #[test]
    fn format_for_resolves_families_from_config() {
        let mut config = AppConfig::default();
        config.model_families.insert("stealth/ox-alpha".into(), "reasoning".into());
        config.model_families.insert("deepseek/deepseek-chat".into(), "reasoning_content".into());

        assert_eq!(
            format_for(&config, "stealth/ox-alpha"),
            ResponseFormat::ReasoningField { field: "reasoning".into() }
        );
        assert_eq!(
            format_for(&config, "deepseek/deepseek-chat"),
            ResponseFormat::ReasoningField { field: "reasoning_content".into() }
        );
        // Unlisted models and unknown family names fall back to Plain.
        assert_eq!(format_for(&config, "groq/compound"), ResponseFormat::Plain);
        assert_eq!(format_for(&config, "unknown-model"), ResponseFormat::Plain);
    }

    #[test]
    fn reasoning_field_for_maps_families_to_provider_fields() {
        use cersei::provider::ReasoningField;
        let mut config = AppConfig::default();
        config.model_families.insert("stealth/ox-alpha".into(), "reasoning".into());
        config.model_families.insert("deepseek/deepseek-chat".into(), "reasoning_content".into());
        config.model_families.insert("groq/compound".into(), "plain".into());

        // Configured families drive an explicit field...
        assert_eq!(
            reasoning_field_for(&config, "stealth/ox-alpha"),
            ReasoningField::Field("reasoning")
        );
        assert_eq!(
            reasoning_field_for(&config, "deepseek/deepseek-chat"),
            ReasoningField::Field("reasoning_content")
        );
        // ...`plain` opts out entirely...
        assert_eq!(reasoning_field_for(&config, "groq/compound"), ReasoningField::Off);
        // ...and unlisted models auto-detect (reasoning models emit reasoning
        // fields only when they think, so auto is the safe default for the
        // live path).
        assert_eq!(reasoning_field_for(&config, "some/other-model"), ReasoningField::Auto);
    }
}

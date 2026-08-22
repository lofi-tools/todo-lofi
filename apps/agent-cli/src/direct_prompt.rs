//! Direct prompting of a specific model over its OpenAI-compatible endpoint,
//! bypassing the agent loop. Used by the response-parsing tests (fixtures and
//! `live-tests`) to capture exactly what a model streams for a given prompt —
//! including the thinking deltas that cersei's provider drops on the floor.
//!
//! The provider/model/api key resolution goes through the same
//! [`crate::providers`] machinery as the rest of agent-cli, so the live tests
//! pick up api keys from the global config (`~/.abstract/config.json`,
//! `env:VAR`, or `!command` specs) with zero extra setup.

use crate::config::AppConfig;
use crate::providers;
use crate::response_format::{Segment, format_for};
use anyhow::Context as _;
use std::time::Duration;

/// Prompt `model` (on `provider`) with a single user message and return the
/// raw SSE body of the streamed `/chat/completions` response. No tools, no
/// system prompt: this is the model talking, not the agent.
pub async fn prompt_raw(
    config: &AppConfig,
    provider: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let resolved = providers::resolve(config, provider, model)
        .with_context(|| format!("resolving '{provider}' for direct prompt"))?;
    let url = format!("{}/chat/completions", resolved.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": resolved.model,
        "messages": [{ "role": "user", "content": prompt }],
        "max_tokens": max_tokens,
        "stream": true,
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("failed to build HTTP client for direct prompt")?;
    let response = client
        .post(&url)
        .bearer_auth(&resolved.api_key)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("direct prompt POST {url} failed"))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("direct prompt failed: HTTP {status}: {text}");
    }
    response.text().await.context("failed to read direct prompt response body")
}

/// Prompt a model and split its streamed output into thinking/text segments
/// using the model's configured response format family (see
/// [`crate::response_format`]).
pub async fn prompt_segments(
    config: &AppConfig,
    provider: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> anyhow::Result<Vec<Segment>> {
    let body = prompt_raw(config, provider, model, prompt, max_tokens).await?;
    let format = format_for(config, model);
    Ok(super::response_format::parse_sse(&format, &body))
}

/// Where captured live responses are stored as fixtures for the offline
/// parsing tests. The directory is under `tests/` so it ships with the crate
/// and offline tests read from it without any network access.
pub fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Save a captured raw SSE body under
/// `fixtures_dir()/{format_family}/{model}-{name}.sse`. The directory is the
/// response-format family the body was parsed under (e.g. `reasoning`), so
/// the offline replay tests know which parser to apply; the model prefix
/// keeps which model produced it. Creates parent directories as needed.
pub fn save_fixture(
    format_family: &str,
    model: &str,
    name: &str,
    body: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let dir = fixtures_dir().join(format_family);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create fixtures dir {}", dir.display()))?;
    let path = dir.join(format!("{model}-{name}.sse"));
    std::fs::write(&path, body)
        .with_context(|| format!("failed to write fixture {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_format::{SegmentKind, parse_sse};

    // ─── Offline fixture replay (no network) ──────────────────────────────
    //
    // These tests run in plain `cargo test`. They replay raw SSE bodies
    // captured from real models by the live tests (see `live_tests` below)
    // and assert the family-specific parser splits thinking from text the
    // way the wire format intends. Fixtures are committed so the tests pass
    // on machines with no api keys.

fn replay(family: &str, model: &str, name: &str) -> Vec<Segment> {
        let path = fixtures_dir()
            .join(family)
            .join(format!("{model}-{name}.sse"));
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "missing fixture {}: {e}; run the live tests to (re)capture it",
                path.display()
            )
        });
        let format = crate::response_format::family(family)
            .unwrap_or_else(|| panic!("unknown fixture family '{family}'"));
        parse_sse(&format, &body)
    }

    #[test]
    fn ox_alpha_thinking_precedes_answer_and_is_split() {
        let segments = replay("reasoning", "ox-alpha", "list_of_steps");
        // Thinking comes first, then the answer text.
        assert_eq!(segments[0].kind, SegmentKind::Thinking);
        let thinking = segments[0].text.trim();
        assert!(!thinking.is_empty());
        // The answer block follows and is not glued to the thinking.
        let text_segments: Vec<&str> = segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Text)
            .map(|s| s.text.as_str())
            .collect();
        let answer = text_segments.concat();
        assert!(!answer.is_empty());
        // The answer is a list of three steps (the prompt asked for three).
        assert!(answer.contains("1."), "answer should start a numbered list: {answer:?}");
        // The thinking is distinct content — it contains the model's
        // internal reasoning, not the final answer verbatim.
        assert_ne!(thinking, answer.trim());
    }

    #[test]
    fn ox_alpha_fixture_reasoning_field_never_leaks_into_answer() {
        let segments = replay("reasoning", "ox-alpha", "list_of_steps");
        let answer: String = segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Text)
            .map(|s| s.text.as_str())
            .collect();
        // The thinking (e.g. "Let me think...", "The user wants...") must not
        // appear in the answer block — that's the delimited-block guarantee.
        for marker in ["The user wants", "Let me think", "think about"] {
            assert!(
                !answer.contains(marker),
                "thinking marker {marker:?} leaked into the answer: {answer:?}"
            );
        }
    }

    // ─── nvidia/nemotron-3-ultra (reasoning_content format) ─────────────

    #[test]
    fn nemotron_thinking_precedes_answer_and_is_split() {
        let segments = replay("reasoning_content", "nemotron-3-ultra", "list_of_steps");
        assert_eq!(segments[0].kind, SegmentKind::Thinking);
        let thinking = segments[0].text.trim();
        assert!(!thinking.is_empty());
        let text_segments: Vec<&str> = segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Text)
            .map(|s| s.text.as_str())
            .collect();
        let answer = text_segments.concat();
        assert!(!answer.is_empty());
        assert!(answer.contains("1."), "answer should start a numbered list: {answer:?}");
        assert_ne!(thinking, answer.trim());
    }

    #[test]
    fn nemotron_fixture_reasoning_content_never_leaks_into_answer() {
        let segments = replay("reasoning_content", "nemotron-3-ultra", "list_of_steps");
        let answer: String = segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Text)
            .map(|s| s.text.as_str())
            .collect();
        for marker in ["The user wants", "Let me think", "think about"] {
            assert!(
                !answer.contains(marker),
                "thinking marker {marker:?} leaked into the answer: {answer:?}"
            );
        }
    }

    #[test]
    fn nemotron_all_fixtures_have_thinking_and_text() {
        for name in ["list_of_steps", "code_snippet", "explain_concept"] {
            let segments = replay("reasoning_content", "nemotron-3-ultra", name);
            assert!(
                segments.iter().any(|s| s.kind == SegmentKind::Thinking),
                "{name}: expected thinking segments"
            );
            assert!(
                segments.iter().any(|s| s.kind == SegmentKind::Text),
                "{name}: expected text segments"
            );
        }
    }

    #[cfg(feature = "live-tests")]
    mod live_tests {
        use super::*;

        /// Prompt ox-alpha with several prompts, assert the family parser
        /// splits thinking from answer text, and persist the raw SSE bodies
        /// as fixtures for the offline tests. Run with:
        /// `cargo test -p agent-cli --features live-tests -- --ignored`
        #[tokio::test]
        #[ignore]
        async fn capture_ox_alpha_response_examples() {
            let config = crate::config::load();
            let cases = [
                (
                    "list_of_steps",
                    "List the three main steps to debug a failing Rust test. Keep it short.",
                ),
                (
                    "code_snippet",
                    "Write a short Rust function that returns the sum of a slice of integers. Keep it to a few lines.",
                ),
                (
                    "explain_concept",
                    "In two sentences, explain what a monad is.",
                ),
            ];
            for (name, prompt) in cases {
                let body = prompt_raw(&config, "openrouter", "stealth/ox-alpha", prompt, 1024)
                    .await
                    .unwrap_or_else(|e| panic!("{name}: direct prompt failed: {e}"));
                assert!(
                    body.contains("data:"),
                    "{name}: expected an SSE body, got: {body}"
                );
                let segments = parse_sse(
                    &crate::response_format::family("reasoning").unwrap(),
                    &body,
                );
                assert!(
                    segments.iter().any(|s| s.kind == SegmentKind::Thinking),
                    "{name}: expected thinking segments, got {segments:?}"
                );
                assert!(
                    segments.iter().any(|s| s.kind == SegmentKind::Text),
                    "{name}: expected answer text, got {segments:?}"
                );
                let path = save_fixture("reasoning", "ox-alpha", name, &body).unwrap();
                eprintln!("captured {name} -> {}", path.display());
            }
        }

        /// Prompt nvidia/nemotron-3-ultra (reasoning_content format) and
        /// capture fixtures. Run with:
        /// `cargo test -p agent-cli --features live-tests -- --ignored`
        #[tokio::test]
        #[ignore]
        async fn capture_nvidia_nemotron_response_examples() {
            let config = crate::config::load();
            let cases = [
                (
                    "list_of_steps",
                    "List the three main steps to debug a failing Rust test. Keep it short.",
                ),
                (
                    "code_snippet",
                    "Write a short Rust function that returns the sum of a slice of integers. Keep it to a few lines.",
                ),
                (
                    "explain_concept",
                    "In two sentences, explain what a monad is.",
                ),
            ];
            for (name, prompt) in cases {
                let body = prompt_raw(
                    &config,
                    "nvidia",
                    "nvidia/nemotron-3-ultra-550b-a55b",
                    prompt,
                    1024,
                )
                .await
                .unwrap_or_else(|e| panic!("{name}: direct prompt failed: {e}"));
                assert!(
                    body.contains("data:"),
                    "{name}: expected an SSE body, got: {body}"
                );
                let segments = parse_sse(
                    &crate::response_format::family("reasoning_content").unwrap(),
                    &body,
                );
                assert!(
                    segments.iter().any(|s| s.kind == SegmentKind::Thinking),
                    "{name}: expected thinking segments, got {segments:?}"
                );
                assert!(
                    segments.iter().any(|s| s.kind == SegmentKind::Text),
                    "{name}: expected answer text, got {segments:?}"
                );
                let path = save_fixture("reasoning_content", "nemotron-3-ultra", name, &body)
                    .unwrap();
                eprintln!("captured {name} -> {}", path.display());
            }
        }
    }
}

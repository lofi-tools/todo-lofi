//! cersei-provider: Provider trait and built-in LLM providers.
//!
//! Providers abstract over different LLM backends (Anthropic, OpenAI, local models).
//! Each provider implements streaming completion, token counting, and capability discovery.

pub mod adapt;
pub mod anthropic;
pub mod quirks;
pub mod anthropic_vertex;
pub mod gemini;
pub mod openai;
pub mod registry;
pub mod router;
mod stream;

use async_trait::async_trait;
use cersei_types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc;

// Re-exports
pub use adapt::{adapt_tools, SchemaDialect};
pub use anthropic::Anthropic;
pub use quirks::{ProviderQuirks, TemperaturePolicy, ThinkingQuirk};
pub use anthropic_vertex::AnthropicVertex;
pub use gemini::Gemini;
pub use openai::{OpenAi, ReasoningField};
pub use router::from_model_string;
pub use stream::StreamAccumulator;

/// Seconds from a `Retry-After` header, if the provider sent a usable one.
///
/// Only the delta-seconds form is honoured. The HTTP-date form is legal but no
/// major provider emits it, and guessing wrong here would mean sleeping for
/// hours, so an unparsable value is treated as absent and the caller falls back
/// to its own backoff.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

/// Seconds until the rate limit resets, from whichever rate-limit header the
/// gateway sent.
///
/// The exact same headers are read for every provider; only the shapes differ:
///
/// * `retry-after` — delta seconds (checked first, it is the most specific).
/// * `x-ratelimit-reset` / `ratelimit-reset` — unix epoch seconds when the
///   value is far in the future, otherwise delta seconds (OpenRouter, IETF
///   draft).
/// * `x-ratelimit-reset-requests` / `-tokens` — Go durations or plain seconds
///   (OpenAI, Groq).
///
/// An unrecognised shape yields `None` so the caller falls back to exponential
/// backoff: a missing hint is strictly better than a wrong sleep.
pub fn parse_rate_limit_reset(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    if let Some(delta) = parse_retry_after(headers) {
        return Some(delta);
    }
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_string())
    };
    for name in ["x-ratelimit-reset", "ratelimit-reset"] {
        if let Some(parsed) = value(name).and_then(|value| parse_reset_value(&value)) {
            return Some(parsed);
        }
    }
    for name in ["x-ratelimit-reset-requests", "x-ratelimit-reset-tokens"] {
        if let Some(parsed) = value(name).and_then(|value| parse_reset_value(&value)) {
            return Some(parsed);
        }
    }
    None
}

/// Parse one reset value: unix epoch seconds, delta seconds, or a Go duration.
fn parse_reset_value(value: &str) -> Option<std::time::Duration> {
    if let Ok(number) = value.parse::<f64>() {
        if !number.is_finite() || number <= 0.0 {
            return None;
        }
        if number > 1_000_000_000.0 {
            // Epoch seconds: how long until that instant.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let delta = number - now;
            return (delta > 0.0).then(|| std::time::Duration::from_secs_f64(delta));
        }
        return Some(std::time::Duration::from_secs_f64(number));
    }
    parse_go_duration(value)
}

/// Parse a Go duration string (`250ms`, `1m30s`, `2h`). Returns `None` for
/// anything else, including `Retry-After`'s HTTP-date form (cersei only ever
/// spoke the delta-seconds shape, and guessing at dates is worse than backing
/// off exponentially).
fn parse_go_duration(value: &str) -> Option<std::time::Duration> {
    let mut total = std::time::Duration::ZERO;
    let mut number = String::new();
    let mut matched = false;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
            continue;
        }
        let magnitude: f64 = match ch {
            'n' => {
                chars.next_if_eq(&'s');
                1e-9
            }
            'u' | 'µ' => {
                chars.next_if_eq(&'s');
                1e-6
            }
            'm' => {
                if chars.peek() == Some(&'s') {
                    chars.next();
                    1e-3
                } else {
                    60.0
                }
            }
            's' => 1.0,
            'h' => 3600.0,
            _ => return None,
        };
        let count: f64 = number.parse().ok()?;
        number.clear();
        total += std::time::Duration::from_secs_f64(count * magnitude);
        matched = true;
    }
    if matched && number.is_empty() {
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;
    use reqwest::header::HeaderMap;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn retry_after_wins_and_reads_delta_seconds() {
        let map = headers(&[("retry-after", "12"), ("x-ratelimit-reset", "999")]);
        assert_eq!(parse_rate_limit_reset(&map), Some(std::time::Duration::from_secs(12)));
    }

    #[test]
    fn reads_the_openrouter_epoch_shape() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let map = headers(&[("x-ratelimit-reset", &(now + 30).to_string())]);
        let parsed = parse_rate_limit_reset(&map).expect("epoch seconds must parse");
        assert!(
            parsed >= std::time::Duration::from_secs(25)
                && parsed <= std::time::Duration::from_secs(35),
            "got {parsed:?}"
        );
        // A reset in the past is not a hint to sleep for zero seconds.
        let map = headers(&[("x-ratelimit-reset", &(now - 30).to_string())]);
        assert_eq!(parse_rate_limit_reset(&map), None);
    }

    #[test]
    fn reads_the_go_duration_shape() {
        let map = headers(&[("x-ratelimit-reset-requests", "1m30s")]);
        assert_eq!(parse_rate_limit_reset(&map), Some(std::time::Duration::from_secs(90)));
        let map = headers(&[("x-ratelimit-reset-tokens", "250ms")]);
        assert_eq!(
            parse_rate_limit_reset(&map),
            Some(std::time::Duration::from_millis(250))
        );
    }

    #[test]
    fn unknown_shapes_and_garbage_fall_back_to_none() {
        let map = headers(&[("x-ratelimit-reset", "Wed, 21 Oct 2026 07:28:00 GMT")]);
        assert_eq!(parse_rate_limit_reset(&map), None);
        let map = headers(&[("x-ratelimit-reset", ""), ("x-ratelimit-reset-tokens", "nope")]);
        assert_eq!(parse_rate_limit_reset(&map), None);
        assert_eq!(parse_rate_limit_reset(&HeaderMap::new()), None);
        assert_eq!(parse_go_duration("1m30"), None);
        assert_eq!(parse_go_duration("30"), None, "bare numbers are seconds elsewhere");
    }
}

// ─── Provider trait ──────────────────────────────────────────────────────────

#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable provider name (e.g., "anthropic", "openai").
    fn name(&self) -> &str;

    /// Context window size for the given model.
    fn context_window(&self, model: &str) -> u64;

    /// Send a streaming completion request.
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream>;

    /// Send a blocking (non-streaming) completion request.
    async fn complete_blocking(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        self.complete(request).await?.collect().await
    }

    /// Count tokens for a message list. Returns an estimate if exact counting is unavailable.
    async fn count_tokens(&self, messages: &[Message], _model: &str) -> Result<u64> {
        // Default: rough estimate based on character count
        let chars: usize = messages.iter().map(|m| m.get_all_text().len()).sum();
        Ok((chars as u64) / 4) // ~4 chars per token
    }
}

// Blanket impl: Box<dyn Provider> is itself a Provider.
#[async_trait]
impl Provider for Box<dyn Provider> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn context_window(&self, model: &str) -> u64 {
        (**self).context_window(model)
    }
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        (**self).complete(request).await
    }
    async fn complete_blocking(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        (**self).complete_blocking(request).await
    }
    async fn count_tokens(&self, messages: &[Message], model: &str) -> Result<u64> {
        (**self).count_tokens(messages, model).await
    }
}

// ─── Authentication ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Auth {
    /// API key sent as `x-api-key` header (Anthropic Console) or `Authorization: Bearer` (OpenAI).
    ApiKey(String),
    /// Bearer token sent as `Authorization: Bearer <token>`.
    Bearer(String),
    /// OAuth flow with client ID and token.
    OAuth {
        client_id: String,
        token: OAuthToken,
    },
    /// Custom auth provider for non-standard flows.
    Custom(std::sync::Arc<dyn AuthProvider>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub scopes: Vec<String>,
}

impl OAuthToken {
    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at_ms {
            chrono::Utc::now().timestamp_millis() >= exp
        } else {
            false
        }
    }
}

#[async_trait]
pub trait AuthProvider: Send + Sync + std::fmt::Debug {
    /// Returns (header_name, header_value) for the request.
    async fn get_credentials(&self) -> Result<(String, String)>;

    /// Refresh credentials if they have expired.
    async fn refresh(&self) -> Result<()>;
}

// ─── Completion request/response ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub system: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub stop_sequences: Vec<String>,
    /// Provider-specific options (thinking budget, top_p, etc.)
    pub options: ProviderOptions,
}

impl CompletionRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            system: None,
            tools: Vec::new(),
            max_tokens: 16384,
            temperature: None,
            stop_sequences: Vec::new(),
            options: ProviderOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderOptions {
    entries: HashMap<String, serde_json::Value>,
}

impl ProviderOptions {
    pub fn set(&mut self, key: impl Into<String>, value: impl Serialize) {
        if let Ok(v) = serde_json::to_value(value) {
            self.entries.insert(key.into(), v);
        }
    }

    pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        self.entries
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn has(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }
}

#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub message: Message,
    pub usage: Usage,
    pub stop_reason: StopReason,
}

// F-23: `ProviderCapabilities` is gone. It was written 40+ times in the
// registry and read exactly once — a `Box<dyn Provider>` forwarder to
// nothing (H6). The living per-(provider, model) surface is
// `quirks::ProviderQuirks`, whose fields derive from the live-verified
// gates instead of a hand-maintained table.

// ─── Completion stream ───────────────────────────────────────────────────────

/// A streaming response from a provider. Wraps a channel of StreamEvents.
pub struct CompletionStream {
    rx: mpsc::Receiver<StreamEvent>,
}

impl CompletionStream {
    pub fn new(rx: mpsc::Receiver<StreamEvent>) -> Self {
        Self { rx }
    }

    /// Consume the stream and collect into a complete response.
    pub async fn collect(mut self) -> Result<CompletionResponse> {
        let mut acc = StreamAccumulator::new();
        while let Some(event) = self.rx.recv().await {
            if let StreamEvent::Error { message } = &event {
                return Err(CerseiError::Provider(message.clone()));
            }
            acc.process_event(event);
        }
        acc.into_response()
    }

    /// Access the underlying receiver for real-time event processing.
    pub fn into_receiver(self) -> mpsc::Receiver<StreamEvent> {
        self.rx
    }
}

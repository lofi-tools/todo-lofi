//! Typed classification of provider failures.
//!
//! HTTP-level failures arrive typed (`CerseiError::from_http_status` maps every
//! non-2xx response in cersei), so classification is mostly a matter of reading
//! the variant. Mid-stream failures arrive as `StreamEvent::Error { message }`
//! with no status — the response was 200 — so a string fallback covers those.
//!
//! The classification decides three things: whether a kind counts against a
//! provider's health (and therefore its routing score), how long a cooldown it
//! triggers, and whether the provider is disabled for the rest of the process.

use cersei::types::CerseiError;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// 429, or a locally imposed cooldown. Carries the provider's stated reset
    /// window when it sent one.
    RateLimited { retry_after: Option<Duration> },
    /// 429 whose body means "no credit" — retrying buys the same answer.
    QuotaExhausted,
    /// 401/403: the key is wrong, expired, or lacks access.
    Auth,
    /// 5xx / 529: the provider is up but saturated.
    Overloaded,
    /// The request timed out.
    Timeout,
    /// Connection-level failure (refused, reset, DNS).
    Network,
    /// 404 or the body says the model does not exist.
    ModelNotFound,
    /// The prompt exceeded the model's context window: our request, not the
    /// provider's health.
    ContextOverflow,
    /// Other 4xx: malformed request, content policy, …
    RequestRejected { status: u16 },
    /// The user (or the agent) cancelled.
    Cancelled,
    /// Unclassified. `message` is kept for the log, never for scoring.
    Unknown { message: String },
}

impl FailureKind {
    /// Stable storage tag, used for the `attempts.error_kind` column.
    pub fn tag(&self) -> &'static str {
        match self {
            FailureKind::RateLimited { .. } => "rate_limited",
            FailureKind::QuotaExhausted => "quota_exhausted",
            FailureKind::Auth => "auth",
            FailureKind::Overloaded => "overloaded",
            FailureKind::Timeout => "timeout",
            FailureKind::Network => "network",
            FailureKind::ModelNotFound => "model_not_found",
            FailureKind::ContextOverflow => "context_overflow",
            FailureKind::RequestRejected { .. } => "request_rejected",
            FailureKind::Cancelled => "cancelled",
            FailureKind::Unknown { .. } => "unknown",
        }
    }

    /// Rebuild a kind from its storage tag. Detail fields (retry window,
    /// status, message) are not persisted, so they come back empty.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "rate_limited" => FailureKind::RateLimited { retry_after: None },
            "quota_exhausted" => FailureKind::QuotaExhausted,
            "auth" => FailureKind::Auth,
            "overloaded" => FailureKind::Overloaded,
            "timeout" => FailureKind::Timeout,
            "network" => FailureKind::Network,
            "model_not_found" => FailureKind::ModelNotFound,
            "context_overflow" => FailureKind::ContextOverflow,
            "request_rejected" => FailureKind::RequestRejected { status: 400 },
            "cancelled" => FailureKind::Cancelled,
            "unknown" => FailureKind::Unknown {
                message: String::new(),
            },
            _ => return None,
        })
    }

    /// Whether this failure says something about the provider's health, and so
    /// belongs in the routing score. A request we sent wrong (too long, blocked,
    /// malformed), a missing model, or a user cancel says nothing about it.
    pub fn counts_against_provider(&self) -> bool {
        match self {
            FailureKind::RateLimited { .. }
            | FailureKind::Overloaded
            | FailureKind::Timeout
            | FailureKind::Network
            | FailureKind::Auth
            | FailureKind::Unknown { .. } => true,
            FailureKind::QuotaExhausted => true,
            FailureKind::ModelNotFound
            | FailureKind::ContextOverflow
            | FailureKind::RequestRejected { .. }
            | FailureKind::Cancelled => false,
        }
    }

    /// A provider-level problem that no amount of retrying fixes this process.
    pub fn disables_provider(&self) -> bool {
        matches!(self, FailureKind::QuotaExhausted | FailureKind::Auth)
    }

    /// A model-level problem: long cooldown for that model only.
    pub fn disables_model(&self) -> bool {
        matches!(self, FailureKind::ModelNotFound)
    }

    /// The stated reset window, if the failure carried one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            FailureKind::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureKind::RateLimited {
                retry_after: Some(d),
            } => {
                write!(f, "rate limited (retry in {}s)", d.as_secs())
            }
            FailureKind::RateLimited { retry_after: None } => write!(f, "rate limited"),
            FailureKind::QuotaExhausted => write!(f, "quota exhausted"),
            FailureKind::Auth => write!(f, "authentication failed"),
            FailureKind::Overloaded => write!(f, "provider overloaded"),
            FailureKind::Timeout => write!(f, "timed out"),
            FailureKind::Network => write!(f, "network error"),
            FailureKind::ModelNotFound => write!(f, "model not found"),
            FailureKind::ContextOverflow => write!(f, "context overflow"),
            FailureKind::RequestRejected { status } => write!(f, "request rejected ({status})"),
            FailureKind::Cancelled => write!(f, "cancelled"),
            FailureKind::Unknown { message } => write!(f, "unknown error: {message}"),
        }
    }
}

/// Classify a typed provider error.
pub fn classify(error: &CerseiError) -> FailureKind {
    match error {
        CerseiError::RateLimit {
            retry_after,
            message,
        } => {
            if looks_like_quota(message) {
                FailureKind::QuotaExhausted
            } else {
                FailureKind::RateLimited {
                    retry_after: *retry_after,
                }
            }
        }
        CerseiError::ProviderStatus { status, message } => classify_status(*status, message),
        CerseiError::Auth(message) => {
            if looks_like_quota(message) {
                FailureKind::QuotaExhausted
            } else {
                FailureKind::Auth
            }
        }
        CerseiError::ContextOverflow { .. } => FailureKind::ContextOverflow,
        CerseiError::Cancelled => FailureKind::Cancelled,
        CerseiError::Http(e) => {
            if e.is_timeout() {
                FailureKind::Timeout
            } else if e.is_connect() {
                FailureKind::Network
            } else {
                classify_message(&e.to_string())
            }
        }
        CerseiError::Provider(message) => classify_message(message),
        CerseiError::Other(error) => classify_message(&error.to_string()),
        other => classify_message(&other.to_string()),
    }
}

/// Classify an HTTP status with its response body.
pub fn classify_status(status: u16, message: &str) -> FailureKind {
    if looks_like_quota(message) {
        return FailureKind::QuotaExhausted;
    }
    match status {
        401 | 403 => FailureKind::Auth,
        404 => FailureKind::ModelNotFound,
        429 => FailureKind::RateLimited { retry_after: None },
        500 | 502 | 503 | 504 | 529 => FailureKind::Overloaded,
        400 | 422 => {
            // A 400 is either our request's fault (context, content policy) or
            // a missing model; the body says which.
            match classify_message(message) {
                FailureKind::Unknown { .. } => FailureKind::RequestRejected { status },
                kind => kind,
            }
        }
        _ if (400..500).contains(&status) => FailureKind::RequestRejected { status },
        _ => FailureKind::Unknown {
            message: message.to_string(),
        },
    }
}

/// String fallback for failures that arrive without a status — mid-stream
/// errors, local errors, anything wrapped in a message.
pub fn classify_message(message: &str) -> FailureKind {
    let lower = message.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    if looks_like_quota(&lower) {
        return FailureKind::QuotaExhausted;
    }
    if has(&["rate limit", "ratelimit", "429", "too many requests"]) {
        return FailureKind::RateLimited { retry_after: None };
    }
    if has(&[
        "context length",
        "context_length",
        "maximum context",
        "too many tokens",
        "token limit",
    ]) {
        return FailureKind::ContextOverflow;
    }
    if has(&[
        "model not found",
        "unknown model",
        "no such model",
        "does not exist",
        "model_not_found",
    ]) && has(&["model"])
    {
        return FailureKind::ModelNotFound;
    }
    if has(&["timeout", "timed out", "deadline"]) {
        return FailureKind::Timeout;
    }
    if has(&[
        "network",
        "connection",
        "dns",
        "reset by peer",
        "broken pipe",
    ]) {
        return FailureKind::Network;
    }
    if has(&["overloaded", "unavailable", "capacity", "503", "529"]) {
        return FailureKind::Overloaded;
    }
    if has(&["cancelled", "canceled", "aborted"]) {
        return FailureKind::Cancelled;
    }
    FailureKind::Unknown {
        message: message.to_string(),
    }
}

/// Wording that means "you are out of credit", not "slow down": every retry
/// buys the same answer, so the provider is disabled rather than cooled down.
fn looks_like_quota(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "insufficient_quota",
        "insufficient quota",
        "exceeded your current quota",
        "quota exceeded",
        "no credit",
        "out of credits",
        "insufficient balance",
        "insufficient funds",
        "billing",
        "payment required",
        "402",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_errors_map_to_the_documented_kinds() {
        // 429 with a Retry-After becomes a rate limit carrying the window.
        let e = CerseiError::from_http_status(429, Some(Duration::from_secs(30)), "slow down");
        assert_eq!(
            classify(&e),
            FailureKind::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            }
        );

        // 429 whose body says the account is out of money is not a rate limit.
        let e = CerseiError::from_http_status(429, None, "insufficient_quota: add credits");
        assert_eq!(classify(&e), FailureKind::QuotaExhausted);
        assert!(classify(&e).disables_provider());
        assert!(classify(&e).retry_after().is_none());

        assert_eq!(
            classify(&CerseiError::from_http_status(401, None, "bad key")),
            FailureKind::Auth
        );
        assert_eq!(
            classify(&CerseiError::from_http_status(404, None, "no such model")),
            FailureKind::ModelNotFound
        );
        for status in [500, 502, 503, 504, 529] {
            assert_eq!(
                classify(&CerseiError::from_http_status(status, None, "oops")),
                FailureKind::Overloaded,
                "status {status}"
            );
        }
        // A 400 that says "too long" is our request's fault, not the provider's.
        assert_eq!(
            classify(&CerseiError::from_http_status(
                400,
                None,
                "maximum context length is 8192 tokens"
            )),
            FailureKind::ContextOverflow
        );
        assert_eq!(
            classify(&CerseiError::from_http_status(
                400,
                None,
                "unknown model: x"
            )),
            FailureKind::ModelNotFound
        );
        assert_eq!(
            classify(&CerseiError::from_http_status(
                400,
                None,
                "content policy violation"
            )),
            FailureKind::RequestRejected { status: 400 }
        );

        assert_eq!(
            classify(&CerseiError::ContextOverflow { used: 10, limit: 5 }),
            FailureKind::ContextOverflow
        );
        assert_eq!(classify(&CerseiError::Cancelled), FailureKind::Cancelled);
        assert_eq!(
            classify(&CerseiError::Auth("token expired".into())),
            FailureKind::Auth
        );
        assert_eq!(
            classify(&CerseiError::Provider("provider died mid-stream".into())),
            FailureKind::Unknown {
                message: "provider died mid-stream".into()
            }
        );
    }

    #[test]
    fn message_fallback_classifies_stream_errors() {
        assert_eq!(
            classify_message("HTTP 429: too many requests"),
            FailureKind::RateLimited { retry_after: None }
        );
        assert_eq!(
            classify_message("the request timed out"),
            FailureKind::Timeout
        );
        assert_eq!(
            classify_message("connection reset by peer"),
            FailureKind::Network
        );
        assert_eq!(
            classify_message("provider is overloaded, try later"),
            FailureKind::Overloaded
        );
        assert_eq!(
            classify_message("model not found: stealth/nope"),
            FailureKind::ModelNotFound
        );
        assert_eq!(
            classify_message("operation cancelled"),
            FailureKind::Cancelled
        );
        assert!(matches!(
            classify_message("something odd happened"),
            FailureKind::Unknown { .. }
        ));
    }

    #[test]
    fn tags_round_trip() {
        for kind in [
            FailureKind::RateLimited { retry_after: None },
            FailureKind::QuotaExhausted,
            FailureKind::Auth,
            FailureKind::Overloaded,
            FailureKind::Timeout,
            FailureKind::Network,
            FailureKind::ModelNotFound,
            FailureKind::ContextOverflow,
            FailureKind::RequestRejected { status: 400 },
            FailureKind::Cancelled,
            FailureKind::Unknown {
                message: String::new(),
            },
        ] {
            assert_eq!(
                FailureKind::from_tag(kind.tag()).as_ref(),
                Some(&kind),
                "{kind:?}"
            );
        }
        assert!(FailureKind::from_tag("nope").is_none());
    }

    #[test]
    fn only_provider_health_failures_count() {
        assert!(FailureKind::Timeout.counts_against_provider());
        assert!(FailureKind::Overloaded.counts_against_provider());
        assert!(!FailureKind::ContextOverflow.counts_against_provider());
        assert!(!FailureKind::Cancelled.counts_against_provider());
        assert!(!FailureKind::ModelNotFound.counts_against_provider());
        assert!(!FailureKind::RequestRejected { status: 400 }.counts_against_provider());
        assert!(FailureKind::ModelNotFound.disables_model());
    }
}

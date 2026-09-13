use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

/// Process-wide monotonic clock origin.
///
/// Retry due-times are compared against this clock so that wall-clock adjustments
/// cannot make a scheduled retry fire early or late.
fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Milliseconds elapsed on the process monotonic clock.
pub fn monotonic_ms() -> u64 {
    origin().elapsed().as_millis() as u64
}

/// Convert a monotonic timestamp produced by [`monotonic_ms`] back to a wall-clock time.
///
/// Only meaningful while the process is alive, which is all the snapshot interface needs.
pub fn now_plus_monotonic(due_at_ms: u64) -> SystemTime {
    let now = monotonic_ms();
    let skew = due_at_ms.saturating_sub(now);
    SystemTime::now() + std::time::Duration::from_millis(skew)
}

/// Format a timestamp as RFC 3339, or `null` when absent.
pub fn format_timestamp(timestamp: Option<SystemTime>) -> serde_json::Value {
    match timestamp {
        Some(timestamp) => {
            let offset = time::OffsetDateTime::from(timestamp);
            match offset.format(&time::format_description::well_known::Rfc3339) {
                Ok(formatted) => serde_json::Value::String(formatted),
                Err(_) => serde_json::Value::Null,
            }
        }
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_monotonic_ms_is_monotonic() {
        let first = monotonic_ms();
        std::thread::sleep(Duration::from_millis(5));
        assert!(monotonic_ms() >= first);
    }

    #[test]
    fn test_format_timestamp_is_rfc3339_utc() {
        let formatted = format_timestamp(Some(SystemTime::UNIX_EPOCH));
        assert_eq!(formatted, serde_json::json!("1970-01-01T00:00:00Z"));
        assert_eq!(format_timestamp(None), serde_json::Value::Null);
    }
}

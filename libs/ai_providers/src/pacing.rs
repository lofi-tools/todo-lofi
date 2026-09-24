//! Client-side pacing and cooldown policy for one provider.
//!
//! Two ways to stay under a provider's limit:
//!
//! - **Pacing** — a concurrency cap and a minimum gap between requests. Both
//!   are optional; when unset the limiter starts unthrottled and *learns*
//!   (multiplicative decrease on a 429 or overload, slow recovery after a
//!   success streak) instead of trusting a hand-written requests-per-minute
//!   table for a dozen gateways.
//! - **Cooldowns** — how long a failed `(provider, model)` stays out of the
//!   fallback walk, derived from the failure kind and any reset window the
//!   provider stated.
//!
//! Limiters are shared per provider *name* (owned by the catalog), so a
//! concurrency cap means "across this process for this provider", including
//! sub-agents.

use crate::failure::FailureKind;
use crate::spec::PacingSpec;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Multiplicative decrease applied to the learned interval after a 429.
pub const DECREASE: f64 = 2.0;
/// Additive-ish recovery factor applied after [`RECOVER_AFTER`] successes.
pub const RECOVER: f64 = 0.8;
/// Consecutive successes needed before the interval recovers.
pub const RECOVER_AFTER: u32 = 20;
/// Shortest cooldown a failure can impose.
pub const DEFAULT_MIN_COOLDOWN: Duration = Duration::from_secs(30);
/// Longest cooldown a transient failure can impose.
pub const DEFAULT_MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// Ceiling for the learned inter-request interval.
pub const MAX_LEARNED_INTERVAL: Duration = Duration::from_secs(30);
/// Where learning starts when a provider has no configured floor: the first
/// 429 buys a small pause, and further ones double it.
pub const LEARNED_BASE_INTERVAL: Duration = Duration::from_millis(500);
/// A model the provider says does not exist stays out for a long time.
pub const MODEL_NOT_FOUND_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// What a provider's response headers said about its limits. Every field is
/// optional: most gateways send none of them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RateLimitHint {
    pub remaining_requests: Option<u32>,
    pub limit_requests: Option<u32>,
    /// How long until the request budget resets.
    pub reset: Option<Duration>,
}

impl RateLimitHint {
    pub fn is_empty(&self) -> bool {
        self.remaining_requests.is_none() && self.limit_requests.is_none() && self.reset.is_none()
    }
}

struct LimiterState {
    /// Learned gap between requests (already clamped to the spec's floor).
    interval: Duration,
    /// When the next request may start (slot reservation).
    next_slot: Option<Instant>,
    /// A header-derived hold, e.g. `remaining == 0` until `reset`.
    hold_until: Option<Instant>,
    /// The last cooldown imposed, so the backoff can escalate.
    last_cooldown: Option<Duration>,
}

/// Per-provider limiter. Wrap in `Arc` and share.
pub struct ProviderLimiter {
    provider: String,
    spec: PacingSpec,
    concurrency: Option<Arc<Semaphore>>,
    in_flight: AtomicUsize,
    successes: AtomicU32,
    state: Mutex<LimiterState>,
}

impl ProviderLimiter {
    pub fn new(provider: impl Into<String>, spec: PacingSpec) -> Arc<Self> {
        let concurrency = spec
            .max_concurrency
            .filter(|cap| *cap > 0)
            .map(|cap| Arc::new(Semaphore::new(cap as usize)));
        Arc::new(Self {
            provider: provider.into(),
            spec,
            concurrency,
            in_flight: AtomicUsize::new(0),
            successes: AtomicU32::new(0),
            state: Mutex::new(LimiterState {
                interval: Duration::ZERO,
                next_slot: None,
                hold_until: None,
                last_cooldown: None,
            }),
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// How loaded this provider is, as a 0..1 fraction of its cap. Zero when
    /// no cap is configured.
    pub fn pressure(&self) -> f64 {
        match self.spec.max_concurrency.filter(|cap| *cap > 0) {
            Some(cap) => (self.in_flight.load(Ordering::Relaxed) as f64) / (cap as f64),
            None => 0.0,
        }
    }

    /// Take a concurrency permit (held for the life of the request) and bump
    /// the in-flight count.
    pub async fn acquire(self: &Arc<Self>) -> ConcurrencyPermit {
        let permit = match &self.concurrency {
            Some(semaphore) => semaphore.clone().acquire_owned().await.ok(),
            None => None,
        };
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        ConcurrencyPermit {
            limiter: self.clone(),
            _permit: permit,
        }
    }

    /// Wait until this limiter's own pacing allows the next request to start.
    ///
    /// The slot is *reserved* while the lock is held, so two waiters cannot
    /// pick the same instant.
    pub async fn wait_turn(&self) {
        let now = Instant::now();
        let target = {
            let mut state = self.state.lock();
            let hold = state.hold_until.filter(|until| *until > now);
            if hold.is_none() {
                state.hold_until = None;
            }
            let base = hold.unwrap_or(now);
            let interval = self.pacing_interval(&state);
            let next_ok = state
                .next_slot
                .map(|slot| slot + interval)
                .filter(|slot| *slot > base)
                .unwrap_or(base);
            state.next_slot = Some(next_ok);
            next_ok
        };
        if target > now {
            tokio::time::sleep(target - now).await;
        }
    }

    /// The gap the limiter currently enforces: the spec's explicit floor, the
    /// interval implied by `requests_per_minute`, and the learned interval.
    fn pacing_interval(&self, state: &LimiterState) -> Duration {
        let mut interval = state.interval;
        if let Some(min_interval) = self.spec.min_interval {
            interval = interval.max(min_interval);
        }
        if let Some(rpm) = self.spec.requests_per_minute.filter(|rpm| *rpm > 0) {
            interval = interval.max(Duration::from_secs_f64(60.0 / rpm as f64));
        }
        interval.min(MAX_LEARNED_INTERVAL)
    }

    pub fn effective_interval(&self) -> Duration {
        let state = self.state.lock();
        self.pacing_interval(&state)
    }

    /// Apply a rate-limit hint observed on a response.
    ///
    /// Hints only ever *tighten* pacing: with no requests left we hold until
    /// the stated reset; otherwise the remaining budget sets a floor on the
    /// interval. A gateway that sends nothing changes nothing.
    pub fn observe(&self, hint: &RateLimitHint) {
        if hint.is_empty() {
            return;
        }
        let mut state = self.state.lock();
        if let Some(reset) = hint.reset {
            let hold = Instant::now() + reset_min(reset);
            if hint.remaining_requests == Some(0) {
                state.hold_until = Some(match state.hold_until {
                    Some(existing) => existing.max(hold),
                    None => hold,
                });
            }
        }
        if let (Some(remaining), Some(limit)) = (hint.remaining_requests, hint.limit_requests)
            && limit > 0
            && remaining < limit
        {
            // Spread the remaining budget across the window we know about.
            if let Some(reset) = hint.reset
                && remaining > 0
            {
                let spread = reset_min(reset) / remaining;
                state.interval = state.interval.max(spread).min(MAX_LEARNED_INTERVAL);
            }
        }
    }

    /// The cooldown a failure kind imposes on this `(provider, model)`, or
    /// `None` when the failure should not cool anything down.
    pub fn cooldown_for(&self, kind: &FailureKind) -> Option<Duration> {
        let min = self.spec.min_cooldown.unwrap_or(DEFAULT_MIN_COOLDOWN);
        let max = self.spec.max_cooldown.unwrap_or(DEFAULT_MAX_COOLDOWN);
        match kind {
            FailureKind::RateLimited { retry_after } => Some(match retry_after {
                Some(stated) => (*stated).clamp(min, max),
                None => self.next_backoff(min, max),
            }),
            FailureKind::Overloaded | FailureKind::Timeout | FailureKind::Network => {
                Some(self.next_backoff(min, max))
            }
            FailureKind::Unknown { .. } => Some(self.next_backoff(min, max)),
            FailureKind::ModelNotFound => Some(MODEL_NOT_FOUND_COOLDOWN),
            FailureKind::QuotaExhausted
            | FailureKind::Auth
            | FailureKind::ContextOverflow
            | FailureKind::RequestRejected { .. }
            | FailureKind::Cancelled => None,
        }
    }

    /// Exponential backoff: double the previous cooldown, clamped.
    fn next_backoff(&self, min: Duration, max: Duration) -> Duration {
        let mut state = self.state.lock();
        let next = state
            .last_cooldown
            .map(|last| {
                Duration::from_secs_f64((last.as_secs_f64() * DECREASE).max(min.as_secs_f64()))
            })
            .unwrap_or(min);
        let next = next.min(max);
        state.last_cooldown = Some(next);
        next
    }

    /// Record a successful request; enough in a row and the learned interval
    /// recovers toward the configured floor.
    pub fn note_success(&self) {
        if self.successes.fetch_add(1, Ordering::Relaxed) + 1 < RECOVER_AFTER {
            return;
        }
        self.successes.store(0, Ordering::Relaxed);
        let mut state = self.state.lock();
        if !state.interval.is_zero() {
            state.interval = Duration::from_secs_f64(state.interval.as_secs_f64() * RECOVER);
            state.last_cooldown = None;
        }
    }

    /// A failure that is not a cooldown still slows the limiter down.
    pub fn note_failure(&self, kind: &FailureKind) {
        self.successes.store(0, Ordering::Relaxed);
        if matches!(
            kind,
            FailureKind::RateLimited { .. } | FailureKind::Overloaded
        ) {
            let mut state = self.state.lock();
            let floor = self.effective_interval_floor().max(LEARNED_BASE_INTERVAL);
            let grown = if state.interval.is_zero() {
                floor
            } else {
                Duration::from_secs_f64(state.interval.as_secs_f64() * DECREASE)
            };
            state.interval = grown.max(floor).min(MAX_LEARNED_INTERVAL);
        }
    }

    fn effective_interval_floor(&self) -> Duration {
        let mut floor = Duration::ZERO;
        if let Some(min_interval) = self.spec.min_interval {
            floor = floor.max(min_interval);
        }
        if let Some(rpm) = self.spec.requests_per_minute.filter(|rpm| *rpm > 0) {
            floor = floor.max(Duration::from_secs_f64(60.0 / rpm as f64));
        }
        floor
    }
}

/// Clamp a header-stated reset so a bogus value cannot park us for hours.
fn reset_min(reset: Duration) -> Duration {
    reset.clamp(Duration::from_secs(1), DEFAULT_MAX_COOLDOWN)
}

/// Keeps the in-flight count accurate for as long as the request is alive.
pub struct ConcurrencyPermit {
    limiter: Arc<ProviderLimiter>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl std::fmt::Debug for ConcurrencyPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcurrencyPermit")
            .field("capped", &self._permit.is_some())
            .finish()
    }
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        self.limiter.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(pacing: PacingSpec) -> Arc<ProviderLimiter> {
        ProviderLimiter::new("test", pacing)
    }

    #[test]
    fn explicit_limits_set_a_floor_on_the_interval() {
        let l = limiter(PacingSpec {
            requests_per_minute: Some(60),
            min_interval: Some(Duration::from_secs(3)),
            ..Default::default()
        });
        // min_interval (3s) beats 60rpm (1s).
        assert_eq!(l.effective_interval(), Duration::from_secs(3));
        assert_eq!(l.pressure(), 0.0, "no concurrency cap means no pressure");
    }

    #[tokio::test]
    async fn no_limits_means_no_waiting() {
        let l = limiter(PacingSpec::default());
        let start = Instant::now();
        l.wait_turn().await;
        l.wait_turn().await;
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn concurrency_cap_is_enforced_across_holders() {
        let l = limiter(PacingSpec {
            max_concurrency: Some(1),
            ..Default::default()
        });
        let first = l.acquire().await;
        assert_eq!(l.pressure(), 1.0);
        // A second acquire must wait; release the first and it proceeds.
        let second = tokio::spawn({
            let l = l.clone();
            async move { l.acquire().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !second.is_finished(),
            "cap of 1 must block the second request"
        );
        drop(first);
        let _second = second.await.unwrap();
        assert_eq!(l.pressure(), 1.0);
    }

    #[test]
    fn cooldown_policy_follows_the_failure_kind() {
        let l = limiter(PacingSpec {
            min_cooldown: Some(Duration::from_secs(10)),
            max_cooldown: Some(Duration::from_secs(100)),
            ..Default::default()
        });
        // A stated window wins, clamped to the configured bounds.
        assert_eq!(
            l.cooldown_for(&FailureKind::RateLimited {
                retry_after: Some(Duration::from_secs(5))
            }),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            l.cooldown_for(&FailureKind::RateLimited {
                retry_after: Some(Duration::from_secs(5_000))
            }),
            Some(Duration::from_secs(100))
        );
        // No stated window: escalate 10 → 20 → 40, capped at 100.
        assert_eq!(
            l.cooldown_for(&FailureKind::RateLimited { retry_after: None }),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            l.cooldown_for(&FailureKind::Overloaded),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            l.cooldown_for(&FailureKind::Timeout),
            Some(Duration::from_secs(40))
        );
        assert_eq!(
            l.cooldown_for(&FailureKind::Network),
            Some(Duration::from_secs(80))
        );
        assert_eq!(
            l.cooldown_for(&FailureKind::Network),
            Some(Duration::from_secs(100))
        );
        // A missing model gets a long, non-escalating cooldown.
        assert_eq!(
            l.cooldown_for(&FailureKind::ModelNotFound),
            Some(MODEL_NOT_FOUND_COOLDOWN)
        );
        // Our own request errors and cancels do not cool anything down.
        assert_eq!(l.cooldown_for(&FailureKind::ContextOverflow), None);
        assert_eq!(l.cooldown_for(&FailureKind::Cancelled), None);
        assert_eq!(
            l.cooldown_for(&FailureKind::RequestRejected { status: 400 }),
            None
        );
        assert_eq!(l.cooldown_for(&FailureKind::QuotaExhausted), None);
        assert_eq!(l.cooldown_for(&FailureKind::Auth), None);
    }

    #[test]
    fn learning_grows_then_recovers_the_interval() {
        let l = limiter(PacingSpec::default());
        // Unconfigured means unthrottled until something actually goes wrong.
        assert_eq!(l.effective_interval(), Duration::ZERO);
        l.note_failure(&FailureKind::RateLimited { retry_after: None });
        let first = l.effective_interval();
        assert!(first > Duration::ZERO, "a 429 must slow the limiter down");
        l.note_failure(&FailureKind::RateLimited { retry_after: None });
        let second = l.effective_interval();
        assert!(second > first, "a second 429 must slow it further");
        for _ in 0..RECOVER_AFTER {
            l.note_success();
        }
        assert!(
            l.effective_interval() < second,
            "successes must recover the interval"
        );
        // It never grows past the ceiling.
        for _ in 0..40 {
            l.note_failure(&FailureKind::Overloaded);
        }
        assert_eq!(l.effective_interval(), MAX_LEARNED_INTERVAL);
    }

    #[test]
    fn hints_only_tighten_and_never_park_us_for_hours() {
        let l = limiter(PacingSpec::default());
        l.observe(&RateLimitHint::default());
        assert_eq!(l.effective_interval(), Duration::ZERO);

        // A bogus reset is clamped, and remaining > 0 spreads the budget.
        l.observe(&RateLimitHint {
            remaining_requests: Some(2),
            limit_requests: Some(10),
            reset: Some(Duration::from_secs(100_000)),
        });
        let interval = l.effective_interval();
        assert!(interval > Duration::ZERO && interval <= MAX_LEARNED_INTERVAL);

        // remaining == 0 holds the next request until the reset.
        let l = limiter(PacingSpec::default());
        l.observe(&RateLimitHint {
            remaining_requests: Some(0),
            limit_requests: Some(10),
            reset: Some(Duration::from_millis(120)),
        });
        let start = Instant::now();
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                l.wait_turn().await;
            });
        assert!(start.elapsed() >= Duration::from_millis(100));
    }
}

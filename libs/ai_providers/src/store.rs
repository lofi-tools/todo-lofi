//! The telemetry port: what the library needs from the agent's database.
//!
//! The library defines the trait and ships two implementations — [`NullStore`]
//! (no-op, the default) and [`InMemoryStore`] (tests) — while agent-cli
//! implements it over its own toasty + turso database with its own migrations.
//! Storage errors are never fatal: the caller treats them as warnings.

use crate::failure::FailureKind;
use crate::spec::ModelKey;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok {
        input_tokens: u64,
        output_tokens: u64,
    },
    Failed(FailureKind),
}

impl Outcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok { .. })
    }

    /// The failure kind's storage tag, or `None` for a successful attempt.
    pub fn error_tag(&self) -> Option<&'static str> {
        match self {
            Outcome::Ok { .. } => None,
            Outcome::Failed(kind) => Some(kind.tag()),
        }
    }

    pub fn tokens(&self) -> (u64, u64) {
        match self {
            Outcome::Ok {
                input_tokens,
                output_tokens,
            } => (*input_tokens, *output_tokens),
            Outcome::Failed(_) => (0, 0),
        }
    }
}

/// One HTTP attempt, as the transport sees it.
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub provider: String,
    pub model: String,
    pub at: SystemTime,
    pub outcome: Outcome,
    pub latency: Option<Duration>,
    /// The agent run this attempt belongs to, when the agent supplied a scope.
    pub session_id: Option<String>,
    /// The turn in flight when the attempt ran, when the agent supplied one.
    pub turn_id: Option<String>,
}

impl AttemptRecord {
    pub fn key(&self) -> ModelKey {
        ModelKey::new(&self.provider, &self.model)
    }
}

/// A stored attempt, as read back for scoring (no message text, ever).
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptSummary {
    pub at: SystemTime,
    pub ok: bool,
    /// The failure kind's tag, `None` when the attempt succeeded.
    pub error_tag: Option<String>,
    pub latency: Option<Duration>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl AttemptSummary {
    pub fn counts_against_provider(&self) -> bool {
        if self.ok {
            return false;
        }
        self.error_tag
            .as_deref()
            .and_then(FailureKind::from_tag)
            .map(|kind| kind.counts_against_provider())
            .unwrap_or(false)
    }
}

/// The library's storage port. The agent implements this; nothing here may
/// fail a run, so callers log and continue on `Err`.
#[async_trait]
pub trait TelemetryStore: Send + Sync {
    async fn record_attempt(&self, attempt: &AttemptRecord) -> anyhow::Result<()>;

    async fn set_cooldown(
        &self,
        key: &ModelKey,
        until: SystemTime,
        cause: FailureKind,
    ) -> anyhow::Result<()>;

    async fn cooldown_until(&self, key: &ModelKey) -> anyhow::Result<Option<SystemTime>>;

    /// Attempts at or after `since`, oldest first.
    async fn attempts_since(
        &self,
        key: &ModelKey,
        since: SystemTime,
    ) -> anyhow::Result<Vec<AttemptSummary>>;

    /// Every `(key, until)` with a cooldown still in the future, for seeding
    /// the in-process registry at startup.
    async fn active_cooldowns(&self) -> anyhow::Result<Vec<(ModelKey, SystemTime, String)>>;
}

/// A store that remembers nothing. Used when telemetry is disabled and by
/// lookups that only need the catalog.
pub struct NullStore;

#[async_trait]
impl TelemetryStore for NullStore {
    async fn record_attempt(&self, _attempt: &AttemptRecord) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_cooldown(
        &self,
        _key: &ModelKey,
        _until: SystemTime,
        _cause: FailureKind,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cooldown_until(&self, _key: &ModelKey) -> anyhow::Result<Option<SystemTime>> {
        Ok(None)
    }
    async fn attempts_since(
        &self,
        _key: &ModelKey,
        _since: SystemTime,
    ) -> anyhow::Result<Vec<AttemptSummary>> {
        Ok(Vec::new())
    }
    async fn active_cooldowns(&self) -> anyhow::Result<Vec<(ModelKey, SystemTime, String)>> {
        Ok(Vec::new())
    }
}

/// An in-memory store: the crate's tests and `--no-telemetry` runs.
#[derive(Default)]
pub struct InMemoryStore {
    attempts: Mutex<Vec<AttemptRecord>>,
    cooldowns: Mutex<HashMap<ModelKey, (SystemTime, String)>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attempts(&self) -> Vec<AttemptRecord> {
        self.attempts.lock().clone()
    }

    pub fn cooldowns(&self) -> HashMap<ModelKey, (SystemTime, String)> {
        self.cooldowns.lock().clone()
    }
}

#[async_trait]
impl TelemetryStore for InMemoryStore {
    async fn record_attempt(&self, attempt: &AttemptRecord) -> anyhow::Result<()> {
        self.attempts.lock().push(attempt.clone());
        Ok(())
    }

    async fn set_cooldown(
        &self,
        key: &ModelKey,
        until: SystemTime,
        cause: FailureKind,
    ) -> anyhow::Result<()> {
        self.cooldowns
            .lock()
            .insert(key.clone(), (until, cause.tag().to_string()));
        Ok(())
    }

    async fn cooldown_until(&self, key: &ModelKey) -> anyhow::Result<Option<SystemTime>> {
        let now = SystemTime::now();
        Ok(self
            .cooldowns
            .lock()
            .get(key)
            .map(|(until, _)| *until)
            .filter(|until| *until > now))
    }

    async fn attempts_since(
        &self,
        key: &ModelKey,
        since: SystemTime,
    ) -> anyhow::Result<Vec<AttemptSummary>> {
        let mut out: Vec<AttemptSummary> = self
            .attempts
            .lock()
            .iter()
            .filter(|a| a.key() == *key && a.at >= since)
            .map(|a| {
                let (input_tokens, output_tokens) = a.outcome.tokens();
                AttemptSummary {
                    at: a.at,
                    ok: a.outcome.is_ok(),
                    error_tag: a.outcome.error_tag().map(str::to_string),
                    latency: a.latency,
                    input_tokens,
                    output_tokens,
                }
            })
            .collect();
        out.sort_by_key(|a| a.at);
        Ok(out)
    }

    async fn active_cooldowns(&self) -> anyhow::Result<Vec<(ModelKey, SystemTime, String)>> {
        let now = SystemTime::now();
        Ok(self
            .cooldowns
            .lock()
            .iter()
            .filter(|(_, (until, _))| *until > now)
            .map(|(key, (until, cause))| (key.clone(), *until, cause.clone()))
            .collect())
    }
}

// ─── Attempt scope ──────────────────────────────────────────────────────────

/// Attribution for the attempts written while an agent is running.
///
/// One scope per built agent (parent or sub-agent): the agent creates it, hands
/// it to the library when it builds the provider, and updates the turn at turn
/// boundaries. The library reads it when it writes an attempt and never guesses
/// — a missing turn is recorded as NULL.
#[derive(Debug)]
pub struct AttemptScope {
    pub session_id: Option<String>,
    /// The turn in flight, if any.
    turn: Mutex<Option<String>>,
    /// For a sub-agent: the turn that spawned it.
    pub parent_turn_id: Option<String>,
}

impl AttemptScope {
    pub fn new(session_id: Option<String>) -> Self {
        Self {
            session_id,
            turn: Mutex::new(None),
            parent_turn_id: None,
        }
    }

    /// A scope with no session (single-shot helpers, direct prompts).
    pub fn anonymous() -> Self {
        Self::new(None)
    }

    pub fn with_parent_turn(mut self, parent_turn_id: Option<String>) -> Self {
        self.parent_turn_id = parent_turn_id;
        self
    }

    /// Set (or clear) the turn the next attempts belong to.
    pub fn set_turn(&self, turn_id: Option<String>) {
        *self.turn.lock() = turn_id;
    }

    pub fn turn(&self) -> Option<String> {
        self.turn.lock().clone()
    }
}

// ─── In-process cooldown registry ───────────────────────────────────────────

/// One cooldown: when it lifts, and what caused it.
#[derive(Debug, Clone, PartialEq)]
pub struct CooldownEntry {
    pub until: SystemTime,
    pub cause: String,
}

/// The process-authoritative view of cooldowns.
///
/// The database is the durable record, but the fallback walk happens on the
/// TUI thread with no async context, so the library keeps a synchronous mirror:
/// seeded from the store at startup (`active_cooldowns`), written by the
/// transport on failure, and read by both the walk and the router.
#[derive(Default)]
pub struct CooldownRegistry {
    entries: Mutex<HashMap<ModelKey, CooldownEntry>>,
    /// Providers disabled for the rest of the process (auth/quota).
    disabled: Mutex<HashMap<String, String>>,
}

impl CooldownRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seed(&self, rows: impl IntoIterator<Item = (ModelKey, SystemTime, String)>) {
        let mut entries = self.entries.lock();
        for (key, until, cause) in rows {
            entries.insert(key, CooldownEntry { until, cause });
        }
    }

    pub fn set(&self, key: &ModelKey, until: SystemTime, cause: impl Into<String>) {
        self.entries.lock().insert(
            key.clone(),
            CooldownEntry {
                until,
                cause: cause.into(),
            },
        );
    }

    /// The cooldown's expiry when it is still in the future.
    pub fn until(&self, key: &ModelKey) -> Option<SystemTime> {
        let now = SystemTime::now();
        self.entries
            .lock()
            .get(key)
            .map(|entry| entry.until)
            .filter(|until| *until > now)
    }

    pub fn is_cooling(&self, key: &ModelKey) -> bool {
        self.until(key).is_some()
    }

    pub fn clear(&self, key: &ModelKey) {
        self.entries.lock().remove(key);
    }

    /// Disable a provider for the rest of the process (auth / quota).
    pub fn disable_provider(&self, provider: &str, reason: impl Into<String>) {
        self.disabled
            .lock()
            .insert(provider.to_string(), reason.into());
    }

    pub fn disabled_reason(&self, provider: &str) -> Option<String> {
        self.disabled.lock().get(provider).cloned()
    }

    pub fn is_disabled(&self, provider: &str) -> bool {
        self.disabled.lock().contains_key(provider)
    }

    /// Drop expired entries (called on write so the map cannot grow forever).
    pub fn prune(&self) {
        let now = SystemTime::now();
        self.entries.lock().retain(|_, entry| entry.until > now);
    }

    pub fn snapshot(&self) -> HashMap<ModelKey, CooldownEntry> {
        self.entries.lock().clone()
    }
}

/// Convenience: a store handle shared across the catalog and the agent.
pub type StoreHandle = Arc<dyn TelemetryStore>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_store_round_trips_attempts_and_cooldowns() {
        let store = InMemoryStore::new();
        let key = ModelKey::new("groq", "groq/compound");
        let now = SystemTime::now();
        store
            .record_attempt(&AttemptRecord {
                provider: key.provider.clone(),
                model: key.model.clone(),
                at: now,
                outcome: Outcome::Failed(FailureKind::Timeout),
                latency: Some(Duration::from_millis(1200)),
                session_id: Some("s1".into()),
                turn_id: Some("t1".into()),
            })
            .await
            .unwrap();
        store
            .record_attempt(&AttemptRecord {
                provider: key.provider.clone(),
                model: key.model.clone(),
                at: now + Duration::from_secs(1),
                outcome: Outcome::Ok {
                    input_tokens: 10,
                    output_tokens: 3,
                },
                latency: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .unwrap();

        let attempts = store
            .attempts_since(&key, now - Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].error_tag.as_deref(), Some("timeout"));
        assert!(attempts[0].counts_against_provider());
        assert!(attempts[1].ok);
        assert_eq!(attempts[1].input_tokens, 10);
        // Other keys see nothing.
        assert!(
            store
                .attempts_since(&ModelKey::new("groq", "other"), now)
                .await
                .unwrap()
                .is_empty()
        );

        let until = SystemTime::now() + Duration::from_secs(30);
        store
            .set_cooldown(&key, until, FailureKind::RateLimited { retry_after: None })
            .await
            .unwrap();
        assert!(store.cooldown_until(&key).await.unwrap().is_some());
        assert_eq!(store.active_cooldowns().await.unwrap().len(), 1);
    }

    #[test]
    fn registry_seeds_reads_and_expires() {
        let registry = CooldownRegistry::new();
        let key = ModelKey::new("groq", "groq/compound");
        registry.seed([(
            key.clone(),
            SystemTime::now() + Duration::from_secs(60),
            "rate_limited".into(),
        )]);
        assert!(registry.is_cooling(&key));
        // Setting a past cooldown is not "cooling".
        registry.set(&key, SystemTime::now() - Duration::from_secs(1), "test");
        assert!(!registry.is_cooling(&key));
        assert!(registry.until(&key).is_none());

        assert!(!registry.is_disabled("groq"));
        registry.disable_provider("groq", "no credit");
        assert!(registry.is_disabled("groq"));
        assert_eq!(
            registry.disabled_reason("groq").as_deref(),
            Some("no credit")
        );
    }

    #[test]
    fn scope_carries_session_turn_and_parent() {
        let scope = AttemptScope::new(Some("s1".into())).with_parent_turn(Some("p1".into()));
        assert_eq!(scope.session_id.as_deref(), Some("s1"));
        assert_eq!(scope.parent_turn_id.as_deref(), Some("p1"));
        assert_eq!(scope.turn(), None);
        scope.set_turn(Some("t1".into()));
        assert_eq!(scope.turn().as_deref(), Some("t1"));
        scope.set_turn(None);
        assert_eq!(scope.turn(), None);

        let anon = AttemptScope::anonymous();
        assert!(anon.session_id.is_none());
    }
}

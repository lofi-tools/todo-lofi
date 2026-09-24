//! The cersei-facing transport.
//!
//! [`ConfiguredProvider`] applies a model's configured request parameters on
//! top of every completion request; [`PacedProvider`] wraps it with client-side
//! pacing, cooldown policy and telemetry. Nothing here reimplements the wire
//! format — cersei's OpenAI SSE reader (as patched in this repo) stays the
//! single implementation.

use crate::failure::{FailureKind, classify, classify_message};
use crate::key::resolve_api_key;
use crate::pacing::ProviderLimiter;
use crate::spec::{ModelKey, Resolved};
use crate::store::{AttemptRecord, AttemptScope, CooldownRegistry, Outcome, StoreHandle};
use cersei::types::{CerseiError, Result as CerseiResult, StreamEvent, Usage};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;

/// An OpenAI-compatible provider with per-model request parameters applied on
/// top of every completion request.
///
/// `top_p` and `extra_body` ride in the request's `ProviderOptions` and are
/// emitted by cersei's OpenAi provider on the wire (`top_p` as a top-level body
/// field, `extra_body` merged as an object of extra body fields).
pub struct ConfiguredProvider {
    inner: cersei::OpenAi,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    extra_body: Option<serde_json::Value>,
}

impl ConfiguredProvider {
    /// Build the underlying cersei provider for a resolved selection.
    pub fn build(
        resolved: &Resolved,
        reasoning: cersei::provider::ReasoningField,
    ) -> anyhow::Result<Self> {
        let inner = cersei::OpenAi::builder()
            .base_url(&resolved.base_url)
            .api_key(&resolved.api_key)
            .model(&resolved.model)
            .reasoning_field(reasoning)
            .build()
            .map_err(|error| anyhow::anyhow!("failed to build provider: {error}"))?;
        Ok(Self {
            inner,
            max_tokens: resolved.max_tokens,
            temperature: resolved.temperature,
            top_p: resolved.top_p,
            extra_body: resolved.extra_body.clone(),
        })
    }

    /// The configured reasoning field (for tests / diagnostics).
    pub fn reasoning_field(&self) -> cersei::provider::ReasoningField {
        self.inner.reasoning_field()
    }
}

#[async_trait::async_trait]
impl cersei::provider::Provider for ConfiguredProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn context_window(&self, model: &str) -> u64 {
        self.inner.context_window(model)
    }

    async fn complete(
        &self,
        mut request: cersei::provider::CompletionRequest,
    ) -> CerseiResult<cersei::provider::CompletionStream> {
        if let Some(max_tokens) = self.max_tokens {
            request.max_tokens = max_tokens;
        }
        if let Some(temperature) = self.temperature {
            request.temperature = Some(temperature);
        }
        if let Some(top_p) = self.top_p {
            request.options.set("top_p", top_p);
        }
        if let Some(extra_body) = &self.extra_body {
            request.options.set("extra_body", extra_body.clone());
        }
        self.inner.complete(request).await
    }
}

/// A configured provider wrapped with pacing, cooldowns and telemetry.
///
/// One instance per built agent. Cheap to share: the limiter, cooldown registry
/// and store are process-wide, so sub-agents pace against the same budget.
pub struct PacedProvider {
    resolved: Resolved,
    /// The api-key spec as configured (`!cmd`, `env:VAR`, or a literal), kept
    /// so an auth failure can re-resolve it once before giving up (D13).
    api_key_spec: String,
    reasoning: cersei::provider::ReasoningField,
    inner: parking_lot::Mutex<Arc<ConfiguredProvider>>,
    limiter: Arc<ProviderLimiter>,
    store: StoreHandle,
    cooldowns: Arc<CooldownRegistry>,
    scope: Arc<AttemptScope>,
}

impl PacedProvider {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        resolved: Resolved,
        api_key_spec: String,
        reasoning: cersei::provider::ReasoningField,
        inner: ConfiguredProvider,
        limiter: Arc<ProviderLimiter>,
        store: StoreHandle,
        cooldowns: Arc<CooldownRegistry>,
        scope: Arc<AttemptScope>,
    ) -> Self {
        Self {
            resolved,
            api_key_spec,
            reasoning,
            inner: parking_lot::Mutex::new(Arc::new(inner)),
            limiter,
            store,
            cooldowns,
            scope,
        }
    }

    fn key(&self) -> ModelKey {
        ModelKey::new(&self.resolved.provider, &self.resolved.model)
    }

    /// The inner provider. Cloned out of the lock so no guard is held across an
    /// await point.
    fn snapshot(&self) -> Arc<ConfiguredProvider> {
        self.inner.lock().clone()
    }

    /// Re-resolve the api key and rebuild the provider. Only meaningful for a
    /// recomputable spec (`!command` / `env:VAR`): a literal key would resolve
    /// to the same value and the retry would just repeat the failure.
    fn rebuild(&self) -> anyhow::Result<()> {
        let recomputable =
            self.api_key_spec.starts_with('!') || self.api_key_spec.starts_with("env:");
        if !recomputable {
            anyhow::bail!("api key is a literal; nothing to re-resolve");
        }
        let api_key = resolve_api_key(&self.api_key_spec)?;
        let mut resolved = self.resolved.clone();
        resolved.api_key = api_key;
        let rebuilt = ConfiguredProvider::build(&resolved, self.reasoning)?;
        *self.inner.lock() = Arc::new(rebuilt);
        Ok(())
    }

    /// Record one attempt. Store failures are warnings: a broken database must
    /// not fail a run.
    async fn record(&self, outcome: Outcome, latency: Option<Duration>) {
        let attempts = self.store.clone();
        let record = AttemptRecord {
            provider: self.resolved.provider.clone(),
            model: self.resolved.model.clone(),
            at: SystemTime::now(),
            outcome,
            latency,
            session_id: self.scope.session_id.clone(),
            turn_id: self.scope.turn(),
        };
        if let Err(error) = attempts.record_attempt(&record).await {
            eprintln!("warning: failed to record provider attempt: {error}");
        }
    }

    /// Apply the cooldown policy for a failure: in-process registry (read by
    /// the walk and the router on this very turn) plus the store, so it
    /// survives a restart.
    async fn apply_cooldown(&self, kind: &FailureKind) {
        let key = self.key();
        if let Some(cooldown) = self.limiter.cooldown_for(kind) {
            let until = SystemTime::now() + cooldown;
            self.cooldowns.set(&key, until, kind.tag());
            if let Err(error) = self.store.set_cooldown(&key, until, kind.clone()).await {
                eprintln!("warning: failed to persist cooldown for {key}: {error}");
            }
        }
        if kind.disables_provider() {
            let reason = format!(
                "provider '{}' disabled for this run ({kind}) — fix or rotate its api key, or top up the account",
                self.resolved.provider
            );
            eprintln!("warning: {reason}");
            self.cooldowns
                .disable_provider(&self.resolved.provider, reason);
        }
    }

    /// The fast path: refuse locally while a cooldown holds, so the agent's
    /// fallback fires without a network round-trip.
    fn cooldown_error(&self) -> Option<CerseiError> {
        if let Some(reason) = self.cooldowns.disabled_reason(&self.resolved.provider) {
            return Some(CerseiError::RateLimit {
                retry_after: None,
                message: reason,
            });
        }
        let until = self.cooldowns.until(&self.key())?;
        let remaining = until.duration_since(SystemTime::now()).unwrap_or_default();
        Some(CerseiError::RateLimit {
            retry_after: Some(remaining.max(Duration::from_secs(1))),
            message: format!("{} is cooling down", self.key()),
        })
    }

    /// Wrap the inner stream so usage, a mid-stream failure and the request's
    /// latency are recorded when the stream ends, and the concurrency permit is
    /// held for as long as events may arrive.
    fn record_stream(
        &self,
        stream: cersei::provider::CompletionStream,
        permit: crate::pacing::ConcurrencyPermit,
        started: Instant,
    ) -> cersei::provider::CompletionStream {
        let mut rx = stream.into_receiver();
        let (tx, out_rx) = mpsc::channel(256);
        let store = self.store.clone();
        let limiter = self.limiter.clone();
        let key = self.key();
        let scope = self.scope.clone();
        tokio::spawn(async move {
            // The permit lives here so the provider's concurrency slot stays
            // taken for the whole stream, not just the handshake.
            let _permit = permit;
            let mut usage = Usage::default();
            let mut failure: Option<FailureKind> = None;
            while let Some(event) = rx.recv().await {
                match &event {
                    StreamEvent::MessageDelta {
                        usage: Some(event_usage),
                        ..
                    } => usage.merge(event_usage),
                    StreamEvent::Error { message } if failure.is_none() => {
                        failure = Some(classify_message(message));
                    }
                    _ => {}
                }
                if tx.send(event).await.is_err() {
                    // The consumer dropped the stream; nothing left to report.
                    return;
                }
            }
            let latency = Some(started.elapsed());
            let outcome = match failure {
                Some(kind) => {
                    limiter.note_failure(&kind);
                    Outcome::Failed(kind)
                }
                None => {
                    limiter.note_success();
                    Outcome::Ok {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                    }
                }
            };
            let record = AttemptRecord {
                provider: key.provider.clone(),
                model: key.model.clone(),
                at: SystemTime::now(),
                outcome,
                latency,
                session_id: scope.session_id.clone(),
                turn_id: scope.turn(),
            };
            if let Err(error) = store.record_attempt(&record).await {
                eprintln!("warning: failed to record provider attempt: {error}");
            }
        });
        cersei::provider::CompletionStream::new(out_rx)
    }
}

#[async_trait::async_trait]
impl cersei::provider::Provider for PacedProvider {
    fn name(&self) -> &str {
        &self.resolved.provider
    }

    fn context_window(&self, model: &str) -> u64 {
        self.snapshot().context_window(model)
    }

    async fn complete(
        &self,
        request: cersei::provider::CompletionRequest,
    ) -> CerseiResult<cersei::provider::CompletionStream> {
        if let Some(error) = self.cooldown_error() {
            return Err(error);
        }
        let permit = self.limiter.acquire().await;
        self.limiter.wait_turn().await;
        let started = Instant::now();
        let result = self.snapshot().complete(request.clone()).await;
        match result {
            Ok(stream) => Ok(self.record_stream(stream, permit, started)),
            Err(error) => {
                let kind = classify(&error);
                self.limiter.note_failure(&kind);
                self.record(Outcome::Failed(kind.clone()), Some(started.elapsed()))
                    .await;
                // A rotating key (or a stale `env:` read) recovers from one
                // re-resolve; a genuinely bad key fails again and is then
                // handled by the policy below.
                if matches!(kind, FailureKind::Auth) && self.rebuild().is_ok() {
                    let retry_started = Instant::now();
                    match self.snapshot().complete(request).await {
                        Ok(stream) => {
                            self.limiter.note_success();
                            return Ok(self.record_stream(stream, permit, retry_started));
                        }
                        Err(retry_error) => {
                            let retry_kind = classify(&retry_error);
                            self.limiter.note_failure(&retry_kind);
                            self.record(
                                Outcome::Failed(retry_kind.clone()),
                                Some(retry_started.elapsed()),
                            )
                            .await;
                            self.apply_cooldown(&retry_kind).await;
                            return Err(retry_error);
                        }
                    }
                }
                self.apply_cooldown(&kind).await;
                Err(error)
            }
        }
    }
}

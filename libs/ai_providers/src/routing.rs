//! Deterministic, explainable model routing.
//!
//! Candidates are the entries of the selected combo, in configured order. Each
//! one is scored from decayed failure history, cooldown state and pacing
//! pressure; the lowest penalty wins and ties fall back to the configured
//! order. Every choice produces a [`RoutingDecision`] carrying each candidate's
//! terms, so "why this model" is answerable without re-deriving anything.
//!
//! Price is a seam: models are assumed free, so its weight and term are `0`.

use crate::spec::ModelKey;
use crate::store::{AttemptSummary, CooldownRegistry, StoreHandle};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Smoothing prior for the failure rate of a candidate with no history.
const PRIOR_FAILURE_RATE: f64 = 0.5;
/// Smoothing strength (pseudo-attempts) toward the prior.
const SMOOTHING: f64 = 2.0;
/// Penalty added to a candidate that is currently cooling down.
const COOLDOWN_PENALTY: f64 = 10.0;
/// How many of the most recent latencies feed the (off-by-default) latency term.
const LATENCY_SAMPLES: usize = 5;

/// Routing knobs, from the agent's `routing` config section.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingConfig {
    pub enabled: bool,
    pub window_hours: f64,
    pub half_life_hours: f64,
    pub min_samples: f64,
    pub weight_failure: f64,
    pub weight_pacing: f64,
    pub weight_latency: f64,
    pub weight_price: f64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_hours: 24.0,
            half_life_hours: 6.0,
            min_samples: 3.0,
            weight_failure: 1.0,
            weight_pacing: 0.25,
            weight_latency: 0.0,
            weight_price: 0.0,
        }
    }
}

impl RoutingConfig {
    /// A config that preserves the configured order: cooldowns still skip an
    /// entry (that is the walk's job), but nothing else influences the choice.
    pub fn ordered_only() -> Self {
        Self {
            enabled: false,
            weight_failure: 0.0,
            weight_pacing: 0.0,
            weight_latency: 0.0,
            weight_price: 0.0,
            ..Default::default()
        }
    }
}

/// One entry to be considered, with the pacing pressure the agent measured for
/// its provider (`0.0` when the provider has no concurrency cap).
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub key: ModelKey,
    pub pacing_pressure: f64,
}

impl Candidate {
    pub fn new(key: ModelKey, pacing_pressure: f64) -> Self {
        Self {
            key,
            pacing_pressure,
        }
    }
}

/// The scoring terms for one candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateScore {
    pub key: ModelKey,
    /// Decay-weighted, smoothed failure rate (0..1).
    pub failure_rate: f64,
    /// Decay-weighted attempt count backing `failure_rate`.
    pub weighted_attempts: f64,
    /// When the candidate's cooldown lifts, if it is cooling.
    pub in_cooldown: Option<SystemTime>,
    pub pacing_pressure: f64,
    pub penalty: f64,
}

/// A routing choice and the terms behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    pub at: SystemTime,
    pub combo: Option<String>,
    pub chosen: ModelKey,
    /// Every candidate, in configured order.
    pub candidates: Vec<CandidateScore>,
}

impl RoutingDecision {
    /// True when the choice matches the configured order — in which case the
    /// UI has nothing interesting to say.
    pub fn is_configured_order(&self) -> bool {
        self.candidates
            .first()
            .map(|first| first.key == self.chosen)
            .unwrap_or(true)
    }

    /// A one-line explanation for the UI, or `None` when nothing moved.
    pub fn summary_line(&self) -> Option<String> {
        if self.is_configured_order() {
            return None;
        }
        let chosen = self.candidates.iter().find(|c| c.key == self.chosen)?;
        let mut line = format!(
            "routing: {} → {} (fail {:.2} over {:.1} attempts)",
            self.combo.as_deref().unwrap_or("selection"),
            chosen.key,
            chosen.failure_rate,
            chosen.weighted_attempts
        );
        if let Some(next) = self
            .candidates
            .iter()
            .find(|c| c.key != self.chosen && c.in_cooldown.is_none())
        {
            line.push_str(&format!(
                " — next: {} (fail {:.2} over {:.1})",
                next.key, next.failure_rate, next.weighted_attempts
            ));
        }
        Some(line)
    }
}

/// Scores candidates and chooses one.
pub struct Router {
    config: RoutingConfig,
    store: StoreHandle,
    cooldowns: Arc<CooldownRegistry>,
}

impl Router {
    pub fn new(
        config: RoutingConfig,
        store: StoreHandle,
        cooldowns: Arc<CooldownRegistry>,
    ) -> Self {
        Self {
            config,
            store,
            cooldowns,
        }
    }

    pub fn config(&self) -> &RoutingConfig {
        &self.config
    }

    /// Choose among `candidates`. Returns `None` when there is nothing to
    /// choose from.
    ///
    /// A store read failure is treated as "no history" (every candidate scores
    /// the prior), so a broken database cannot fail a run or reorder the walk.
    pub async fn choose(
        &self,
        combo: Option<&str>,
        candidates: &[Candidate],
    ) -> Option<RoutingDecision> {
        if candidates.is_empty() {
            return None;
        }
        let now = SystemTime::now();
        let since = now
            .checked_sub(Duration::from_secs_f64(self.config.window_hours * 3600.0))
            .unwrap_or(now);

        let mut histories: Vec<Vec<AttemptSummary>> = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let attempts = match self.store.attempts_since(&candidate.key, since).await {
                Ok(attempts) => attempts,
                Err(error) => {
                    eprintln!(
                        "warning: routing history for {} unavailable: {error}",
                        candidate.key
                    );
                    Vec::new()
                }
            };
            histories.push(attempts);
        }
        Some(self.decide(combo, candidates, &histories, now))
    }

    /// The scoring core, split out so it can be tested (and later called) with
    /// histories that were fetched elsewhere.
    pub fn decide(
        &self,
        combo: Option<&str>,
        candidates: &[Candidate],
        histories: &[Vec<AttemptSummary>],
        now: SystemTime,
    ) -> RoutingDecision {
        let mut scores: Vec<CandidateScore> = Vec::with_capacity(candidates.len());
        for (index, candidate) in candidates.iter().enumerate() {
            let empty: Vec<AttemptSummary> = Vec::new();
            let attempts = histories.get(index).unwrap_or(&empty);
            let (failure_rate, weighted_attempts) = self.score_attempts(attempts, now);
            let in_cooldown = self.cooldowns.until(&candidate.key);
            scores.push(CandidateScore {
                key: candidate.key.clone(),
                failure_rate,
                weighted_attempts,
                in_cooldown,
                pacing_pressure: candidate.pacing_pressure,
                penalty: 0.0,
            });
        }

        // Latency is relative to the fastest candidate and only exists when its
        // weight is on.
        let latencies: Vec<Option<f64>> = candidates
            .iter()
            .enumerate()
            .map(|(index, _)| histories.get(index).and_then(|a| mean_latency(a)))
            .collect();
        let best_latency = latencies
            .iter()
            .flatten()
            .cloned()
            .fold(f64::INFINITY, f64::min);

        for (index, score) in scores.iter_mut().enumerate() {
            let mut penalty = self.config.weight_failure * score.failure_rate
                + self.config.weight_pacing * score.pacing_pressure;
            if score.in_cooldown.is_some() {
                penalty += COOLDOWN_PENALTY;
            }
            if self.config.weight_latency > 0.0
                && let Some(latency) = latencies[index]
                && best_latency.is_finite()
                && best_latency > 0.0
            {
                penalty += self.config.weight_latency * (latency / best_latency);
            }
            score.penalty = penalty;
        }

        // Lowest penalty wins. Ties keep the configured order, except that a
        // candidate whose cooldown lifts sooner beats one cooling longer — so
        // when *every* entry is cooling down the least-cooled one is tried.
        let mut chosen_index = 0;
        for index in 1..scores.len() {
            match scores[index]
                .penalty
                .partial_cmp(&scores[chosen_index].penalty)
            {
                Some(std::cmp::Ordering::Less) => chosen_index = index,
                Some(std::cmp::Ordering::Equal) => {
                    if cools_sooner(scores[index].in_cooldown, scores[chosen_index].in_cooldown) {
                        chosen_index = index;
                    }
                }
                _ => {}
            }
        }
        let chosen = scores
            .get(chosen_index)
            .map(|score| score.key.clone())
            .unwrap_or_else(|| candidates[0].key.clone());

        RoutingDecision {
            at: now,
            combo: combo.map(str::to_string),
            chosen,
            candidates: scores,
        }
    }

    /// Decay-weighted, smoothed failure rate over `attempts`.
    ///
    /// Only failures that say something about the provider count (see
    /// [`AttemptSummary::counts_against_provider`]); a request we sent wrong or
    /// a cancel is not the provider's fault. Below `min_samples` the score is
    /// exactly the prior, so a new model is neither favoured nor punished.
    pub fn score_attempts(&self, attempts: &[AttemptSummary], now: SystemTime) -> (f64, f64) {
        let half_life = self.config.half_life_hours * 3600.0;
        let mut weighted_failures = 0.0;
        let mut weighted_attempts = 0.0;
        for attempt in attempts {
            // Clock skew: a timestamp in the future weighs as "now".
            let age = now
                .duration_since(attempt.at)
                .unwrap_or_default()
                .as_secs_f64();
            let decay = if half_life > 0.0 {
                0.5_f64.powf(age / half_life)
            } else {
                1.0
            };
            weighted_attempts += decay;
            if attempt.counts_against_provider() {
                weighted_failures += decay;
            }
        }
        if weighted_attempts < self.config.min_samples {
            return (PRIOR_FAILURE_RATE, weighted_attempts);
        }
        let rate =
            (weighted_failures + SMOOTHING * PRIOR_FAILURE_RATE) / (weighted_attempts + SMOOTHING);
        (rate, weighted_attempts)
    }
}

/// Whether a candidate cooling down until `candidate` should be preferred over
/// one cooling until `chosen`: not cooling beats cooling, and among two
/// cooling candidates the earlier expiry wins.
fn cools_sooner(candidate: Option<SystemTime>, chosen: Option<SystemTime>) -> bool {
    match (candidate, chosen) {
        (None, Some(_)) => true,
        (Some(candidate), Some(chosen)) => candidate < chosen,
        _ => false,
    }
}

/// Mean of the most recent latencies, ignoring attempts with none.
fn mean_latency(attempts: &[AttemptSummary]) -> Option<f64> {
    let values: Vec<f64> = attempts
        .iter()
        .filter_map(|attempt| attempt.latency)
        .map(|latency| latency.as_secs_f64())
        .collect();
    if values.is_empty() {
        return None;
    }
    let start = values.len().saturating_sub(LATENCY_SAMPLES);
    let window = &values[start..];
    Some(window.iter().sum::<f64>() / window.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::FailureKind;
    use crate::store::{AttemptRecord, InMemoryStore, Outcome, TelemetryStore};

    fn summary(age_secs: f64, ok: bool, tag: Option<&str>) -> AttemptSummary {
        AttemptSummary {
            at: SystemTime::now() - Duration::from_secs_f64(age_secs),
            ok,
            error_tag: tag.map(str::to_string),
            latency: Some(Duration::from_millis(500)),
            input_tokens: 10,
            output_tokens: 5,
        }
    }

    fn router(config: RoutingConfig) -> Router {
        Router::new(
            config,
            Arc::new(InMemoryStore::new()),
            Arc::new(CooldownRegistry::new()),
        )
    }

    #[test]
    fn thin_history_scores_the_prior() {
        let router = router(RoutingConfig::default());
        let now = SystemTime::now();
        let (rate, attempts) = router.score_attempts(&[summary(0.0, false, Some("timeout"))], now);
        assert_eq!(rate, PRIOR_FAILURE_RATE, "one sample is not evidence");
        assert_eq!(attempts, 1.0);

        let many: Vec<AttemptSummary> = (0..10)
            .map(|i| summary(i as f64 * 60.0, false, Some("timeout")))
            .collect();
        let (rate, attempts) = router.score_attempts(&many, now);
        assert!(attempts > 8.0);
        assert!(rate > 0.8, "ten failures is evidence: {rate}");
    }

    #[test]
    fn failures_we_caused_do_not_count_against_the_provider() {
        let router = router(RoutingConfig::default());
        let now = SystemTime::now();
        // Below the sample threshold the score is exactly the prior, no matter
        // how many of our own failures we made.
        let ours: Vec<AttemptSummary> = (0..2)
            .map(|i| summary(i as f64, false, Some("context_overflow")))
            .collect();
        assert_eq!(router.score_attempts(&ours, now).0, PRIOR_FAILURE_RATE);

        let cancelled: Vec<AttemptSummary> = (0..2)
            .map(|i| summary(i as f64, false, Some("cancelled")))
            .collect();
        assert_eq!(router.score_attempts(&cancelled, now).0, PRIOR_FAILURE_RATE);

        // With a full window, our own errors must never look *worse* than an
        // unknown model, and must score far below the same number of genuine
        // provider failures.
        let many_ours: Vec<AttemptSummary> = (0..10)
            .map(|i| summary(i as f64, false, Some("context_overflow")))
            .collect();
        let many_theirs: Vec<AttemptSummary> = (0..10)
            .map(|i| summary(i as f64, false, Some("timeout")))
            .collect();
        let ours_rate = router.score_attempts(&many_ours, now).0;
        let theirs_rate = router.score_attempts(&many_theirs, now).0;
        assert!(ours_rate <= PRIOR_FAILURE_RATE, "{ours_rate}");
        assert!(
            theirs_rate > ours_rate + 0.5,
            "{theirs_rate} vs {ours_rate}"
        );
    }

    #[test]
    fn decay_halves_at_the_half_life() {
        let config = RoutingConfig {
            half_life_hours: 1.0,
            min_samples: 0.0,
            ..Default::default()
        };
        let router = router(config);
        let now = SystemTime::now();
        // One failure exactly one half-life old contributes half a unit.
        let (_, attempts) = router.score_attempts(&[summary(3600.0, false, Some("timeout"))], now);
        assert!((attempts - 0.5).abs() < 0.01, "got {attempts}");
    }

    #[tokio::test]
    async fn chooses_the_healthier_entry_and_keeps_order_on_ties() {
        let store = Arc::new(InMemoryStore::new());
        let healthy = ModelKey::new("a", "a/model");
        let flaky = ModelKey::new("b", "b/model");

        // Give `flaky` a bad window and leave `healthy` clean.
        for i in 0..8 {
            store
                .record_attempt(&AttemptRecord {
                    provider: flaky.provider.clone(),
                    model: flaky.model.clone(),
                    at: SystemTime::now() - Duration::from_secs(i * 60),
                    outcome: Outcome::Failed(FailureKind::Overloaded),
                    latency: None,
                    session_id: None,
                    turn_id: None,
                })
                .await
                .unwrap();
        }
        let scored = Router::new(
            RoutingConfig::default(),
            store,
            Arc::new(CooldownRegistry::new()),
        );
        let candidates = [
            Candidate::new(flaky.clone(), 0.0),
            Candidate::new(healthy.clone(), 0.0),
        ];
        let decision = scored.choose(Some("coding"), &candidates).await.unwrap();
        assert_eq!(decision.chosen, healthy, "the flaky entry must lose");
        assert!(!decision.is_configured_order());
        let line = decision.summary_line().unwrap();
        assert!(line.contains("routing: coding → a/a/model"), "{line}");
        // Reported in configured order even though the choice differs.
        assert_eq!(decision.candidates[0].key, flaky);
        assert!(decision.candidates[0].penalty > decision.candidates[1].penalty);

        // With no history at all, the configured order is preserved.
        let fresh = router(RoutingConfig::default());
        let decision = fresh
            .choose(
                None,
                &[
                    Candidate::new(healthy.clone(), 0.0),
                    Candidate::new(flaky.clone(), 0.0),
                ],
            )
            .await
            .unwrap();
        assert_eq!(decision.chosen, healthy);
        assert!(decision.is_configured_order());
        assert!(decision.summary_line().is_none());
    }

    #[tokio::test]
    async fn a_cooling_entry_only_wins_when_everything_is_cooling() {
        let store = Arc::new(InMemoryStore::new());
        let cooldowns = Arc::new(CooldownRegistry::new());
        let hot = ModelKey::new("hot", "hot/model");
        let cool = ModelKey::new("cool", "cool/model");
        cooldowns.set(
            &cool,
            SystemTime::now() + Duration::from_secs(300),
            "rate_limited",
        );
        let router = Router::new(RoutingConfig::default(), store, cooldowns.clone());
        let candidates = [
            Candidate::new(cool.clone(), 0.0),
            Candidate::new(hot.clone(), 0.0),
        ];

        let decision = router.choose(None, &candidates).await.unwrap();
        assert_eq!(decision.chosen, hot);
        let cooling = decision.candidates.iter().find(|c| c.key == cool).unwrap();
        assert!(cooling.in_cooldown.is_some());

        // Nothing left but cooling entries: take the least-bad one anyway
        // rather than failing for lack of a candidate.
        cooldowns.set(
            &hot,
            SystemTime::now() + Duration::from_secs(10),
            "rate_limited",
        );
        let decision = router.choose(None, &candidates).await.unwrap();
        assert_eq!(decision.chosen, hot, "shorter cooldown wins");
    }

    #[tokio::test]
    async fn disabled_routing_reproduces_the_configured_order() {
        let store = Arc::new(InMemoryStore::new());
        let key = ModelKey::new("bad", "bad/model");
        for i in 0..20 {
            store
                .record_attempt(&AttemptRecord {
                    provider: key.provider.clone(),
                    model: key.model.clone(),
                    at: SystemTime::now() - Duration::from_secs(i),
                    outcome: Outcome::Failed(FailureKind::Overloaded),
                    latency: None,
                    session_id: None,
                    turn_id: None,
                })
                .await
                .unwrap();
        }
        let router = Router::new(
            RoutingConfig::ordered_only(),
            store,
            Arc::new(CooldownRegistry::new()),
        );
        let good = ModelKey::new("good", "good/model");
        let decision = router
            .choose(
                None,
                &[Candidate::new(key.clone(), 0.0), Candidate::new(good, 0.0)],
            )
            .await
            .unwrap();
        assert_eq!(
            decision.chosen, key,
            "disabled routing keeps configured order"
        );
        assert!(decision.is_configured_order());
    }

    #[tokio::test]
    async fn pacing_pressure_breaks_a_tie() {
        let router = router(RoutingConfig::default());
        let busy = ModelKey::new("busy", "busy/model");
        let idle = ModelKey::new("idle", "idle/model");
        let decision = router
            .choose(
                None,
                &[
                    Candidate::new(busy.clone(), 1.0),
                    Candidate::new(idle.clone(), 0.0),
                ],
            )
            .await
            .unwrap();
        assert_eq!(decision.chosen, idle);
    }

    #[tokio::test]
    async fn a_store_failure_falls_back_to_the_configured_order() {
        struct FailingStore;
        #[async_trait::async_trait]
        impl crate::store::TelemetryStore for FailingStore {
            async fn record_attempt(&self, _attempt: &AttemptRecord) -> anyhow::Result<()> {
                anyhow::bail!("database is on fire")
            }
            async fn set_cooldown(
                &self,
                _key: &ModelKey,
                _until: SystemTime,
                _cause: FailureKind,
            ) -> anyhow::Result<()> {
                anyhow::bail!("database is on fire")
            }
            async fn cooldown_until(&self, _key: &ModelKey) -> anyhow::Result<Option<SystemTime>> {
                anyhow::bail!("database is on fire")
            }
            async fn attempts_since(
                &self,
                _key: &ModelKey,
                _since: SystemTime,
            ) -> anyhow::Result<Vec<AttemptSummary>> {
                anyhow::bail!("database is on fire")
            }
            async fn active_cooldowns(
                &self,
            ) -> anyhow::Result<Vec<(ModelKey, SystemTime, String)>> {
                anyhow::bail!("database is on fire")
            }
        }

        let router = Router::new(
            RoutingConfig::default(),
            Arc::new(FailingStore),
            Arc::new(CooldownRegistry::new()),
        );
        let first = ModelKey::new("a", "a/model");
        let second = ModelKey::new("b", "b/model");
        let decision = router
            .choose(
                None,
                &[
                    Candidate::new(first.clone(), 0.0),
                    Candidate::new(second, 0.0),
                ],
            )
            .await
            .unwrap();
        assert_eq!(decision.chosen, first);
    }
}

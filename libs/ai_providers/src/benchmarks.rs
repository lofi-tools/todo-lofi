//! Benchmark scores and the coding rank derived from them.
//!
//! Two rules shape this module:
//!
//! * **Raw scores are stored, a rank is derived.** Nothing here invents a
//!   single blended number at ingest time. Each measured score is kept with the
//!   benchmark, model and date it belongs to, so re-weighting is a pure
//!   recomputation with no refetch.
//! * **A rank is explainable, like a routing decision.** [`CodingRank`] carries
//!   every term that produced it — the raw score, its percentile, the weight it
//!   received — so "why is this model ranked here" is answerable without
//!   re-deriving anything.
//!
//! ## Why percentile rather than a weighted average of raw scores
//!
//! Benchmarks do not share a scale or a spread, and their difficulty drifts: a
//! benchmark that saturates makes every model look identical, and one that gets
//! easier inflates them all. Blending raw percentages therefore mostly measures
//! the benchmarks' scales. Ranking each score *within its own benchmark* first
//! removes both problems, and it degrades gracefully when a benchmark is
//! compromised — the ordering a partly-broken benchmark produces is still a
//! weaker version of the truth, not a differently-scaled distortion.
//!
//! ## Why coverage shrinks the score
//!
//! A model measured on one benchmark must not outrank one measured on five.
//! The blend is therefore pulled toward a neutral prior in proportion to how
//! little evidence backs it, using the same smoothed-estimator shape as the
//! routing score: `score = blend·c + prior·(1−c)` with `c = n/(n+k)`.

pub mod artificial_analysis;
pub mod huggingface;

use async_trait::async_trait;
use std::time::{Duration, SystemTime};

/// A coding benchmark a score can belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Benchmark {
    /// Artificial Analysis' own coding composite.
    AaCodingIndex,
    /// Artificial Analysis' agentic composite — closer to a coding *agent*
    /// than a coding model.
    AaAgenticIndex,
    /// SWE-bench Verified: human-validated, 500 instances.
    SweBenchVerified,
    /// SWE-bench Lite: the cheaper 300-instance subset.
    SweBenchLite,
    /// Terminal-Bench: end-to-end terminal tasks.
    TerminalBench,
    /// LiveCodeBench: contamination-free, continuously refreshed problems.
    LiveCodeBench,
    /// Aider's polyglot benchmark.
    AiderPolyglot,
    /// The coding arena's vote-based ratings.
    LmArenaCoding,
}

impl Benchmark {
    /// Every benchmark, for iteration in a UI.
    pub fn all() -> &'static [Benchmark] {
        &[
            Benchmark::AaCodingIndex,
            Benchmark::AaAgenticIndex,
            Benchmark::SweBenchVerified,
            Benchmark::SweBenchLite,
            Benchmark::TerminalBench,
            Benchmark::LiveCodeBench,
            Benchmark::AiderPolyglot,
            Benchmark::LmArenaCoding,
        ]
    }

    /// The stable storage key. Changing one of these invalidates stored rows, so
    /// they are treated as an on-disk format.
    pub fn tag(&self) -> &'static str {
        match self {
            Benchmark::AaCodingIndex => "aa_coding_index",
            Benchmark::AaAgenticIndex => "aa_agentic_index",
            Benchmark::SweBenchVerified => "swe_bench_verified",
            Benchmark::SweBenchLite => "swe_bench_lite",
            Benchmark::TerminalBench => "terminal_bench",
            Benchmark::LiveCodeBench => "livecodebench",
            Benchmark::AiderPolyglot => "aider_polyglot",
            Benchmark::LmArenaCoding => "lmarena_coding",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Benchmark> {
        Benchmark::all()
            .iter()
            .copied()
            .find(|benchmark| benchmark.tag() == tag)
    }

    /// A human-readable name, for a UI that explains a rank.
    pub fn label(&self) -> &'static str {
        match self {
            Benchmark::AaCodingIndex => "Artificial Analysis Coding Index",
            Benchmark::AaAgenticIndex => "Artificial Analysis Agentic Index",
            Benchmark::SweBenchVerified => "SWE-bench Verified",
            Benchmark::SweBenchLite => "SWE-bench Lite",
            Benchmark::TerminalBench => "Terminal-Bench",
            Benchmark::LiveCodeBench => "LiveCodeBench",
            Benchmark::AiderPolyglot => "Aider Polyglot",
            Benchmark::LmArenaCoding => "Coding Arena",
        }
    }

    /// Whether a higher score means a better model. Every benchmark here is
    /// "higher is better", but stating it keeps a future ranking-style source
    /// (a win-rate percentile, say) from silently inverting the rank.
    pub fn higher_is_better(&self) -> bool {
        true
    }
}

/// One measured score: a model, a benchmark, a number and when it was measured.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkScore {
    pub benchmark: Benchmark,
    /// The canonical model id the score is about.
    pub model: String,
    /// The raw score as the source reported it. Percentages stay percentages,
    /// Indices stay indices; only the *ranking* compares them, never an
    /// absolute threshold.
    pub score: f64,
    /// When the measurement was taken (or when the source snapshot was
    /// published), used for decay.
    pub at: SystemTime,
    /// Which source produced it, for attribution and debugging.
    pub source: &'static str,
}

impl BenchmarkScore {
    /// A score whose number is usable. A non-finite value from a source is a
    /// parse failure, not a very good or very bad model.
    pub fn is_usable(&self) -> bool {
        self.score.is_finite()
    }
}

/// A trait for anything that can produce benchmark scores: an HTTP API, a
/// dataset, a vendored file. Each source is fetched independently and cached by
/// the caller, so one source going dark cannot take the rank with it.
#[async_trait]
pub trait BenchmarkSource: Send + Sync {
    /// The source name recorded on every score it produces.
    fn name(&self) -> &'static str;

    /// Whether this source needs an api key to work.
    fn requires_api_key(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<Vec<BenchmarkScore>>;
}

/// How the rank is computed. Every knob is explicit so a differently-weighted
/// rank is a configuration change, not a code change.
#[derive(Debug, Clone, PartialEq)]
pub struct RankConfig {
    /// Per-benchmark weights. A benchmark absent from this list does not
    /// contribute (its scores are still stored, just not ranked on).
    pub weights: Vec<(Benchmark, f64)>,
    /// How quickly an old measurement stops mattering. Half-life, in days.
    pub half_life_days: f64,
    /// Coverage smoothing strength: the number of pseudo-measurements that pull
    /// a thinly-measured model toward the prior. `2.0` means one benchmark
    /// carries `1/3` of the conviction of an unlimited evidence base.
    pub coverage_smoothing: f64,
    /// The score a model with no usable evidence receives, in rank space.
    pub prior: f64,
}

impl Default for RankConfig {
    fn default() -> Self {
        Self {
            // The agentic coding composites come first because they measure
            // what this program actually needs — finishing tasks with tools —
            // and the arena last because its "coding" vote skews to front-end
            // demo generation.
            weights: vec![
                (Benchmark::AaCodingIndex, 1.0),
                (Benchmark::SweBenchVerified, 0.9),
                (Benchmark::TerminalBench, 0.8),
                (Benchmark::AaAgenticIndex, 0.7),
                (Benchmark::LiveCodeBench, 0.6),
                (Benchmark::AiderPolyglot, 0.5),
                (Benchmark::SweBenchLite, 0.4),
                (Benchmark::LmArenaCoding, 0.3),
            ],
            half_life_days: 90.0,
            coverage_smoothing: 2.0,
            prior: 0.5,
        }
    }
}

impl RankConfig {
    pub fn weight(&self, benchmark: Benchmark) -> f64 {
        self.weights
            .iter()
            .find(|(candidate, _)| *candidate == benchmark)
            .map(|(_, weight)| *weight)
            .unwrap_or(0.0)
    }
}

/// One benchmark's contribution to a model's rank.
#[derive(Debug, Clone, PartialEq)]
pub struct RankTerm {
    pub benchmark: Benchmark,
    /// The raw number the source reported.
    pub raw: f64,
    /// Where that raw number sits among every model measured on the same
    /// benchmark, in `0..=1`.
    pub percentile: f64,
    /// The configured weight, already decayed for age.
    pub weight: f64,
    /// How old the measurement was, in days.
    pub age_days: f64,
}

/// A model's coding rank, with every term behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingRank {
    pub model: String,
    /// The final rank-space score, `0..=1`, after coverage shrinkage.
    pub score: f64,
    /// The weighted percentile blend before shrinkage.
    pub blend: f64,
    /// How much evidence backs `blend`, in `0..=1`.
    pub confidence: f64,
    /// How many benchmarks contributed.
    pub coverage: usize,
    /// The terms, in configured weight order.
    pub terms: Vec<RankTerm>,
}

impl CodingRank {
    /// A one-line explanation, or `None` for a model with no benchmark data to
    /// explain.
    pub fn summary_line(&self) -> Option<String> {
        if self.terms.is_empty() {
            return None;
        }
        let best = self.terms.iter().max_by(|left, right| {
            left.weight
                .partial_cmp(&right.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        Some(format!(
            "{} — rank {:.2} from {} benchmark(s); strongest signal {} at percentile {:.2}",
            self.model,
            self.score,
            self.coverage,
            best.benchmark.label(),
            best.percentile
        ))
    }
}

/// Rank every model that has at least one usable, weighted score.
///
/// Models with no evidence are omitted rather than ranked at the prior: a list
/// the user reads as "best coding models" must not contain models that were
/// never measured.
pub fn rank_models(
    scores: &[BenchmarkScore],
    config: &RankConfig,
    now: SystemTime,
) -> Vec<CodingRank> {
    // Group by benchmark, keeping only scores that can actually be ranked.
    let mut by_benchmark: Vec<(Benchmark, Vec<&BenchmarkScore>)> = Vec::new();
    for benchmark in Benchmark::all() {
        let group: Vec<&BenchmarkScore> = scores
            .iter()
            .filter(|score| {
                score.benchmark == *benchmark
                    && score.is_usable()
                    && config.weight(*benchmark) > 0.0
            })
            .collect();
        if !group.is_empty() {
            by_benchmark.push((*benchmark, group));
        }
    }

    // Percentile of each score within its own benchmark.
    let mut percentiles: Vec<(Benchmark, &BenchmarkScore, f64)> = Vec::new();
    for (benchmark, group) in &by_benchmark {
        for score in group {
            percentiles.push((*benchmark, *score, percentile_in(group, score.score)));
        }
    }

    let mut models: Vec<String> = percentiles
        .iter()
        .map(|(_, score, _)| score.model.clone())
        .collect();
    models.sort();
    models.dedup();

    let mut ranks: Vec<CodingRank> = models
        .into_iter()
        .map(|model| {
            let mut terms: Vec<RankTerm> = Vec::new();
            let mut weighted_score = 0.0;
            let mut total_weight = 0.0;
            // Walk the benchmarks in configured weight order so the terms come
            // out strongest-first.
            for benchmark in Benchmark::all() {
                let Some((_, score, percentile)) = percentiles
                    .iter()
                    .copied()
                    .find(|(candidate, score, _)| *candidate == *benchmark && score.model == model)
                else {
                    continue;
                };
                let age_days = now
                    .duration_since(score.at)
                    .unwrap_or_default()
                    .as_secs_f64()
                    / 86_400.0;
                let weight = config.weight(*benchmark) * decay(age_days, config.half_life_days);
                weighted_score += weight * percentile;
                total_weight += weight;
                terms.push(RankTerm {
                    benchmark: *benchmark,
                    raw: score.score,
                    percentile,
                    weight,
                    age_days,
                });
            }
            let blend = if total_weight > 0.0 {
                weighted_score / total_weight
            } else {
                config.prior
            };
            let coverage = terms.len();
            let confidence = coverage_of(coverage, config.coverage_smoothing);
            CodingRank {
                model,
                score: blend * confidence + config.prior * (1.0 - confidence),
                blend,
                confidence,
                coverage,
                terms,
            }
        })
        .collect();

    // Best first; ties break on the model id so the order is deterministic.
    ranks.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.model.cmp(&right.model))
    });
    ranks
}

/// The percentile of `value` among `group`, in `0..=1`, using a midrank so ties
/// are not ordered arbitrarily. A single-element group is exactly `0.5`: with
/// nothing to compare against, it carries no ordering information.
fn percentile_in(group: &[&BenchmarkScore], value: f64) -> f64 {
    let count = group.len();
    if count < 2 {
        return 0.5;
    }
    let less = group.iter().filter(|score| score.score < value).count();
    let equal = group.iter().filter(|score| score.score == value).count();
    let midrank = less as f64 + (equal.saturating_sub(1)) as f64 / 2.0;
    midrank / (count - 1) as f64
}

/// How much conviction `count` measurements buy, in `0..=1`.
fn coverage_of(count: usize, smoothing: f64) -> f64 {
    let count = count as f64;
    if smoothing <= 0.0 {
        return if count > 0.0 { 1.0 } else { 0.0 };
    }
    count / (count + smoothing)
}

/// Recency decay: a measurement `half_life_days` old counts for half.
fn decay(age_days: f64, half_life_days: f64) -> f64 {
    if half_life_days <= 0.0 {
        return 1.0;
    }
    0.5_f64.powf(age_days.max(0.0) / half_life_days)
}

/// GET a JSON document, optionally authenticated, for the source
/// implementations below. Kept here so the sources share one timeout policy.
pub(crate) async fn fetch_json(
    url: &str,
    api_key: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut request = client.get(url);
    if let Some(api_key) = api_key {
        request = request.header("x-api-key", api_key);
    }
    let response = request.send().await?;
    let response = response
        .error_for_status()
        .map_err(|error| anyhow::anyhow!("GET {url} failed: {error}"))?;
    Ok(response.json().await?)
}

/// A score aged out of usefulness: past this many half-lives it is dropped
/// rather than left contributing a fraction of a percentile.
pub const MAX_AGE_HALF_LIVES: f64 = 6.0;

/// Whether a measurement is too old to rank on.
pub fn is_stale(score: &BenchmarkScore, config: &RankConfig, now: SystemTime) -> bool {
    let age_days = now
        .duration_since(score.at)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        / 86_400.0;
    config.half_life_days > 0.0 && age_days / config.half_life_days > MAX_AGE_HALF_LIVES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days_ago(days: f64) -> SystemTime {
        SystemTime::now() - Duration::from_secs_f64(days * 86_400.0)
    }

    fn score(benchmark: Benchmark, model: &str, value: f64) -> BenchmarkScore {
        BenchmarkScore {
            benchmark,
            model: model.to_string(),
            score: value,
            at: SystemTime::now(),
            source: "test",
        }
    }

    #[test]
    fn benchmark_tags_round_trip_and_are_unique() {
        for benchmark in Benchmark::all() {
            assert_eq!(Benchmark::from_tag(benchmark.tag()), Some(*benchmark));
        }
        let mut tags: Vec<&str> = Benchmark::all().iter().map(|b| b.tag()).collect();
        tags.sort_unstable();
        let count = tags.len();
        tags.dedup();
        assert_eq!(tags.len(), count, "tags are an on-disk format");
        assert_eq!(Benchmark::from_tag("nope"), None);
    }

    #[test]
    fn percentiles_rank_within_a_benchmark_not_across_them() {
        let config = RankConfig {
            weights: vec![
                (Benchmark::SweBenchVerified, 1.0),
                (Benchmark::LmArenaCoding, 1.0),
            ],
            ..Default::default()
        };
        // One benchmark is on a 0..1 scale, the other on a ~1400-point rating
        // scale, so a raw average would be the rating scale with extra steps.
        let scores = vec![
            score(Benchmark::SweBenchVerified, "alpha", 0.9),
            score(Benchmark::SweBenchVerified, "beta", 0.1),
            score(Benchmark::SweBenchVerified, "delta", 0.05),
            score(Benchmark::LmArenaCoding, "alpha", 100.0),
            score(Benchmark::LmArenaCoding, "beta", 1400.0),
            score(Benchmark::LmArenaCoding, "gamma", 700.0),
            score(Benchmark::LmArenaCoding, "delta", 50.0),
        ];
        let ranks = rank_models(&scores, &config, SystemTime::now());
        let find = |model: &str| ranks.iter().find(|rank| rank.model == model).unwrap();

        let alpha = find("alpha");
        for term in &alpha.terms {
            assert!(
                (0.0..=1.0).contains(&term.percentile),
                "{}",
                term.percentile
            );
        }
        // The raw numbers are carried through unchanged for display...
        let raw: Vec<f64> = alpha.terms.iter().map(|term| term.raw).collect();
        assert!(raw.contains(&0.9) && raw.contains(&100.0), "{raw:?}");
        // ...but the ordering comes from the percentile, not the raw value: the
        // *smaller* raw number is the better percentile.
        let by_raw = alpha
            .terms
            .iter()
            .min_by(|left, right| left.raw.partial_cmp(&right.raw).unwrap());
        let by_percentile = alpha
            .terms
            .iter()
            .max_by(|left, right| left.percentile.partial_cmp(&right.percentile).unwrap());
        assert_eq!(
            by_raw.map(|term| term.benchmark),
            by_percentile.map(|term| term.benchmark)
        );

        // A model that is worst on every benchmark it appears on ranks last.
        let delta = find("delta");
        let lowest = ranks
            .iter()
            .map(|rank| rank.score)
            .fold(f64::INFINITY, f64::min);
        assert!(
            (delta.score - lowest).abs() < 1e-12,
            "delta is worst on both"
        );
        for other in ["alpha", "beta", "gamma"] {
            assert!(delta.score < find(other).score, "{other} must beat delta");
        }
    }

    #[test]
    fn coverage_shrinks_a_thinly_measured_model_toward_the_prior() {
        let config = RankConfig::default();
        let scores = vec![
            // `wide` has four measurements and a middling one on each; `thin`
            // has one measurement and it is the best in its benchmark.
            score(Benchmark::AaCodingIndex, "wide", 50.0),
            score(Benchmark::SweBenchVerified, "wide", 50.0),
            score(Benchmark::TerminalBench, "wide", 50.0),
            score(Benchmark::LiveCodeBench, "wide", 50.0),
            score(Benchmark::AaCodingIndex, "thin", 50.0),
            score(Benchmark::SweBenchVerified, "thin", 50.0),
            score(Benchmark::AaCodingIndex, "other", 10.0),
            score(Benchmark::SweBenchVerified, "other", 10.0),
            score(Benchmark::TerminalBench, "other", 10.0),
            score(Benchmark::LiveCodeBench, "other", 10.0),
        ];
        let ranks = rank_models(&scores, &config, SystemTime::now());
        let wide = ranks.iter().find(|r| r.model == "wide").unwrap();
        let thin = ranks.iter().find(|r| r.model == "thin").unwrap();
        assert_eq!(wide.coverage, 4);
        assert_eq!(thin.coverage, 2);
        assert!(wide.confidence > thin.confidence);
        // Both sit above the worst model, but four measurements buy more
        // conviction than two.
        let other = ranks.iter().find(|r| r.model == "other").unwrap();
        assert!(wide.score > other.score);
        assert!(thin.score > other.score);
        assert!(wide.score > thin.score);
    }

    #[test]
    fn a_model_with_no_evidence_is_not_ranked() {
        let config = RankConfig::default();
        let scores = vec![score(Benchmark::AaCodingIndex, "measured", 50.0)];
        let ranks = rank_models(&scores, &config, SystemTime::now());
        assert_eq!(ranks.len(), 1);
        assert_eq!(ranks[0].model, "measured");
        // A benchmark with weight 0 contributes nothing, so it cannot create a
        // rank entry on its own.
        let config = RankConfig {
            weights: vec![(Benchmark::AaCodingIndex, 1.0)],
            ..Default::default()
        };
        let scores = vec![score(Benchmark::LmArenaCoding, "unweighted", 1400.0)];
        assert!(rank_models(&scores, &config, SystemTime::now()).is_empty());
    }

    #[test]
    fn a_non_finite_score_is_dropped_rather_than_ranked() {
        let config = RankConfig::default();
        let scores = vec![
            score(Benchmark::AaCodingIndex, "good", 50.0),
            BenchmarkScore {
                score: f64::NAN,
                ..score(Benchmark::AaCodingIndex, "broken", 0.0)
            },
        ];
        let ranks = rank_models(&scores, &config, SystemTime::now());
        assert!(ranks.iter().all(|rank| rank.model != "broken"));
    }

    #[test]
    fn an_old_measurement_counts_for_less() {
        let config = RankConfig {
            weights: vec![(Benchmark::AaCodingIndex, 1.0)],
            half_life_days: 90.0,
            ..Default::default()
        };
        let fresh = BenchmarkScore {
            at: days_ago(0.0),
            ..score(Benchmark::AaCodingIndex, "fresh", 50.0)
        };
        let stale = BenchmarkScore {
            at: days_ago(90.0),
            ..score(Benchmark::AaCodingIndex, "stale", 50.0)
        };
        let ranks = rank_models(&[fresh, stale], &config, SystemTime::now());
        let fresh = ranks.iter().find(|r| r.model == "fresh").unwrap();
        let stale = ranks.iter().find(|r| r.model == "stale").unwrap();
        assert!((fresh.terms[0].weight - 1.0).abs() < 1e-6);
        assert!(
            (stale.terms[0].weight - 0.5).abs() < 1e-3,
            "one half-life halves it"
        );
    }

    #[test]
    fn staleness_is_reported_past_the_useful_window() {
        let config = RankConfig::default();
        let now = SystemTime::now();
        let recent = BenchmarkScore {
            at: days_ago(10.0),
            ..score(Benchmark::AaCodingIndex, "recent", 50.0)
        };
        let ancient = BenchmarkScore {
            at: days_ago(90.0 * (MAX_AGE_HALF_LIVES + 1.0)),
            ..score(Benchmark::AaCodingIndex, "ancient", 50.0)
        };
        assert!(!is_stale(&recent, &config, now));
        assert!(is_stale(&ancient, &config, now));
    }

    #[test]
    fn a_rank_explains_itself() {
        let config = RankConfig::default();
        let scores = vec![
            score(Benchmark::AaCodingIndex, "alpha", 80.0),
            score(Benchmark::AaCodingIndex, "beta", 20.0),
            score(Benchmark::SweBenchVerified, "alpha", 80.0),
            score(Benchmark::SweBenchVerified, "beta", 20.0),
        ];
        let ranks = rank_models(&scores, &config, SystemTime::now());
        let alpha = ranks.iter().find(|r| r.model == "alpha").unwrap();
        assert!(alpha.score > 0.5);
        let line = alpha.summary_line().unwrap();
        assert!(line.contains("alpha"), "{line}");
        assert!(line.contains("2 benchmark"), "{line}");
        // The terms are ordered by configured weight, strongest first.
        assert_eq!(alpha.terms[0].benchmark, Benchmark::AaCodingIndex);
    }

    #[test]
    fn a_disabled_benchmark_still_stores_but_never_ranks() {
        let config = RankConfig {
            weights: vec![(Benchmark::AaCodingIndex, 1.0)],
            ..Default::default()
        };
        assert_eq!(config.weight(Benchmark::AaCodingIndex), 1.0);
        assert_eq!(config.weight(Benchmark::LmArenaCoding), 0.0);
        assert!(Benchmark::all().contains(&Benchmark::LmArenaCoding));
    }
}

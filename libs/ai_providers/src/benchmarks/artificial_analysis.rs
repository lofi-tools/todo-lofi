//! Artificial Analysis' Data API: the most structured single source of coding
//! benchmark data, and the only one here that needs a key.
//!
//! Endpoint: `GET https://artificialanalysis.ai/api/v2/language/models`, with
//! the key in an `x-api-key` header. Each entry carries an `evaluations` object;
//! the coding-relevant fields are the two composites
//! (`artificial_analysis_coding_index`, `artificial_analysis_agentic_index`,
//! both already on a 0..100 scale) and the terminal-bench rates
//! (`terminalbench_v2_1`, `terminalbench_hard`, reported as 0..1 and converted
//! to percentages here so every stored score shares one convention).
//!
//! Attribution prefers the entry's `openrouter_api_id` over its `slug`, because
//! that field is already in the `vendor/model` form this workspace's canonical
//! ids use, so a score joins to the models the roster actually serves without a
//! translation table.

use crate::benchmarks::{Benchmark, BenchmarkScore, BenchmarkSource, fetch_json};
use async_trait::async_trait;
use serde_json::Value;
use std::time::SystemTime;

/// The list endpoint. The documented detail endpoint is
/// `/api/v2/language/models/{slug}`; the list form drops the slug.
pub const DEFAULT_URL: &str = "https://artificialanalysis.ai/api/v2/language/models";

/// Fields taken from `evaluations`, with the factor that normalizes each to a
/// percentage. A rate the source reports as 0..1 is scaled; an index already on
/// 0..100 is not.
const EVALUATION_FIELDS: &[(Benchmark, &str, f64)] = &[
    (
        Benchmark::AaCodingIndex,
        "artificial_analysis_coding_index",
        1.0,
    ),
    (
        Benchmark::AaAgenticIndex,
        "artificial_analysis_agentic_index",
        1.0,
    ),
    (Benchmark::TerminalBench, "terminalbench_v2_1", 100.0),
    (Benchmark::SweBenchVerified, "swe_bench_verified", 1.0),
];

/// The terminal-bench fallback, used only when the v2 field is absent.
const TERMINAL_BENCH_LEGACY: &str = "terminalbench_hard";

pub struct ArtificialAnalysis {
    api_key: String,
    url: String,
}

impl ArtificialAnalysis {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            url: DEFAULT_URL.to_string(),
        }
    }

    /// Point at a different host, for a mirror or a test server.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Parse an already-fetched response. Pure, so the shape is verified by a
    /// fixture instead of a live call.
    pub fn parse(&self, body: &Value) -> Vec<BenchmarkScore> {
        let Some(entries) = body.get("data").and_then(Value::as_array) else {
            return Vec::new();
        };
        let at = SystemTime::now();
        let mut scores = Vec::new();
        for entry in entries {
            let Some(model) = entry
                .get("openrouter_api_id")
                .and_then(Value::as_str)
                .or_else(|| entry.get("slug").and_then(Value::as_str))
            else {
                continue;
            };
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            let Some(evaluations) = entry.get("evaluations") else {
                continue;
            };
            for (benchmark, field, scale) in EVALUATION_FIELDS {
                let Some(value) = evaluations.get(*field).and_then(Value::as_f64) else {
                    continue;
                };
                if !value.is_finite() {
                    continue;
                }
                scores.push(BenchmarkScore {
                    benchmark: *benchmark,
                    model: model.to_string(),
                    score: value * scale,
                    at,
                    source: "artificial_analysis",
                });
            }
            // Older snapshots predate the v2 terminal-bench field. The legacy
            // rate is only consulted when the current one produced no score,
            // so the two never double-count the same benchmark.
            let already_has_terminal = scores
                .iter()
                .any(|score| score.model == model && score.benchmark == Benchmark::TerminalBench);
            if !already_has_terminal
                && let Some(value) = evaluations
                    .get(TERMINAL_BENCH_LEGACY)
                    .and_then(Value::as_f64)
                && value.is_finite()
            {
                scores.push(BenchmarkScore {
                    benchmark: Benchmark::TerminalBench,
                    model: model.to_string(),
                    score: value * 100.0,
                    at,
                    source: "artificial_analysis",
                });
            }
        }
        scores
    }
}

#[async_trait]
impl BenchmarkSource for ArtificialAnalysis {
    fn name(&self) -> &'static str {
        "artificial_analysis"
    }

    fn requires_api_key(&self) -> bool {
        true
    }

    async fn fetch(&self) -> anyhow::Result<Vec<BenchmarkScore>> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("the Artificial Analysis API needs an api key");
        }
        let body = fetch_json(&self.url, Some(&self.api_key)).await?;
        Ok(self.parse(&body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> ArtificialAnalysis {
        ArtificialAnalysis::new("test-key")
    }

    #[test]
    fn reads_the_composites_and_scales_the_terminal_bench_rate() {
        let body = serde_json::json!({
            "tier": "commercial",
            "data": [{
                "slug": "gpt-oss-20b",
                "openrouter_api_id": "openai/gpt-oss-20b",
                "evaluations": {
                    "artificial_analysis_intelligence_index": 24.5,
                    "artificial_analysis_coding_index": 18.5,
                    "artificial_analysis_agentic_index": 27.6,
                    "terminalbench_v2_1": 0.22,
                    "hle": 0.1
                }
            }]
        });
        let scores = source().parse(&body);
        let find = |benchmark: Benchmark| {
            scores
                .iter()
                .find(|score| score.benchmark == benchmark)
                .map(|score| score.score)
        };
        // The composites are already percentages.
        assert_eq!(find(Benchmark::AaCodingIndex), Some(18.5));
        assert_eq!(find(Benchmark::AaAgenticIndex), Some(27.6));
        // A 0..1 rate becomes a percentage.
        assert_eq!(find(Benchmark::TerminalBench), Some(22.0));
        // Fields we do not rank on are not turned into scores.
        assert_eq!(find(Benchmark::AiderPolyglot), None);
        // Attribution prefers the openrouter id, which is already canonical.
        assert!(
            scores
                .iter()
                .all(|score| score.model == "openai/gpt-oss-20b")
        );
    }

    #[test]
    fn falls_back_to_the_slug_when_there_is_no_openrouter_id() {
        let body = serde_json::json!({
            "data": [{
                "slug": "claude-opus-5-5",
                "evaluations": { "artificial_analysis_coding_index": 87.1 }
            }]
        });
        let scores = source().parse(&body);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].model, "claude-opus-5-5");
        assert_eq!(scores[0].score, 87.1);
    }

    #[test]
    fn uses_the_legacy_terminal_bench_field_when_the_v2_one_is_absent() {
        let body = serde_json::json!({
            "data": [{
                "openrouter_api_id": "x-ai/grok-4.5",
                "evaluations": { "terminalbench_hard": 0.11 }
            }]
        });
        let scores = source().parse(&body);
        let terminal: Vec<&BenchmarkScore> = scores
            .iter()
            .filter(|score| score.benchmark == Benchmark::TerminalBench)
            .collect();
        assert_eq!(terminal.len(), 1, "exactly one terminal-bench score");
        assert_eq!(terminal[0].score, 11.0);
    }

    #[test]
    fn a_null_or_missing_evaluation_produces_no_score_not_a_zero() {
        let body = serde_json::json!({
            "data": [{
                "openrouter_api_id": "a/b",
                "evaluations": { "artificial_analysis_coding_index": null, "mmmu_pro": null }
            }]
        });
        assert!(source().parse(&body).is_empty(), "null is not a score");
    }

    #[test]
    fn a_body_without_the_data_array_yields_nothing() {
        assert!(
            source()
                .parse(&serde_json::json!({ "error": "unauthorized" }))
                .is_empty()
        );
        assert!(
            source()
                .parse(&serde_json::json!({ "data": [] }))
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_missing_key_is_an_error_rather_than_an_empty_rank() {
        let empty = ArtificialAnalysis::new("  ");
        assert!(empty.requires_api_key());
        let error = empty.fetch().await.unwrap_err();
        assert!(error.to_string().contains("api key"), "{error}");
    }
}

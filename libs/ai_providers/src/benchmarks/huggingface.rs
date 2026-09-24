//! Hugging Face's dataset-rows API: the open path to vote-based and
//! dataset-published benchmark results.
//!
//! One source covers both arenas because the plumbing is identical — only the
//! dataset, config, split and category differ. `GET
//! https://datasets-server.huggingface.co/rows` answers
//! `{"rows": [{"row": {…}}], "num_rows_total": n, "num_rows_per_page": 100}`;
//! a page holds at most 100 rows, so a full snapshot means paging.
//!
//! **Joining is not free here.** Arena rows name models the way humans do
//! (`claude-fable-5.1-max`), not with a wire id, so a score is stored under the
//! source's own label and resolved to a catalog model later through
//! [`crate::families::infer_model_version`] and the family/version group key.
//! Inventing a canonical id at ingest time would bake one guess into the
//! database; keeping the label keeps the guess revisable.
//!
//! The LiveCodeBench leaderboard dataset is gated, so
//! [`HuggingFaceRows::livecodebench`] needs a Hugging Face token to work.

use crate::benchmarks::{Benchmark, BenchmarkScore, BenchmarkSource, fetch_json};
use async_trait::async_trait;
use serde_json::Value;
use std::time::SystemTime;

pub const DEFAULT_ROWS_URL: &str = "https://datasets-server.huggingface.co/rows";

/// The rows API caps a page at 100 regardless of what a caller asks for.
const MAX_PAGE: usize = 100;

/// The row field holding a model's name, in the order sources use it.
const MODEL_FIELDS: &[&str] = &["model_name", "model", "name", "key"];

/// The row field holding a score, in the order sources use it. `rank` is
/// deliberately absent: it is inverted relative to every score here.
const SCORE_FIELDS: &[&str] = &[
    "rating",
    "score",
    "arena_score",
    "elo",
    "accuracy",
    "pass@1",
];

pub struct HuggingFaceRows {
    url: String,
    dataset: String,
    config: String,
    split: String,
    benchmark: Benchmark,
    token: Option<String>,
    /// Keep only rows whose `category` equals this. `None` keeps every row,
    /// which mixes an arena's subject sub-ranks into one list.
    category: Option<String>,
    max_rows: usize,
}

impl HuggingFaceRows {
    /// A source over an explicit dataset. `config` is the dataset's subset
    /// (Hugging Face's "config"), and `split` is usually a time slice.
    pub fn new(
        benchmark: Benchmark,
        dataset: impl Into<String>,
        config: impl Into<String>,
        split: impl Into<String>,
    ) -> Self {
        Self {
            url: DEFAULT_ROWS_URL.to_string(),
            dataset: dataset.into(),
            config: config.into(),
            split: split.into(),
            benchmark,
            token: None,
            category: None,
            max_rows: 1_000,
        }
    }

    /// The arena's coding ranking. `text` is the arena subset and `coding` the
    /// category the arena itself labels its coding sub-rank with; both are
    /// overridable, because a category the arena renames would otherwise
    /// silently produce no scores.
    pub fn lmarena() -> Self {
        Self::new(
            Benchmark::LmArenaCoding,
            "lmarena-ai/leaderboard-dataset",
            "text",
            "latest",
        )
        .with_category(Some("coding"))
    }

    /// LiveCodeBench's published leaderboard. The dataset is gated, so this
    /// source needs [`HuggingFaceRows::with_token`].
    pub fn livecodebench() -> Self {
        Self::new(
            Benchmark::LiveCodeBench,
            "livecodebench/leaderboard",
            "default",
            "train",
        )
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    pub fn with_category(mut self, category: Option<impl Into<String>>) -> Self {
        self.category = category.map(Into::into);
        self
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = max_rows.max(1);
        self
    }

    /// Whether this dataset is gated, and therefore needs a token.
    pub fn needs_token(&self) -> bool {
        self.dataset.starts_with("livecodebench/")
    }

    /// The page URL for one offset.
    fn page_url(&self, offset: usize, length: usize) -> String {
        format!(
            "{}?dataset={}&config={}&split={}&offset={}&length={}",
            self.url.trim_end_matches('/'),
            encode_query_value(&self.dataset),
            encode_query_value(&self.config),
            encode_query_value(&self.split),
            offset,
            length
        )
    }

    /// Parse a page. Pure, so the verified LMArena shape is protected by a
    /// fixture instead of a live call in the test suite.
    pub fn parse(&self, body: &Value) -> Vec<BenchmarkScore> {
        let Some(rows) = body.get("rows").and_then(Value::as_array) else {
            return Vec::new();
        };
        let at = SystemTime::now();
        let mut scores = Vec::new();
        for entry in rows {
            // The rows API nests the row under `row`; a caller handing a bare
            // row array (a fixture, or a different endpoint) is accepted too.
            let row = entry.get("row").unwrap_or(entry);
            if let Some(category) = &self.category
                && row.get("category").and_then(Value::as_str) != Some(category.as_str())
            {
                continue;
            }
            let Some(model) = first_string(row, MODEL_FIELDS) else {
                continue;
            };
            let Some(score) = first_number(row, SCORE_FIELDS) else {
                continue;
            };
            if !score.is_finite() {
                continue;
            }
            scores.push(BenchmarkScore {
                benchmark: self.benchmark,
                model,
                score,
                at,
                source: "huggingface_rows",
            });
        }
        scores
    }
}

#[async_trait]
impl BenchmarkSource for HuggingFaceRows {
    fn name(&self) -> &'static str {
        "huggingface_rows"
    }

    fn requires_api_key(&self) -> bool {
        self.needs_token()
    }

    async fn fetch(&self) -> anyhow::Result<Vec<BenchmarkScore>> {
        let mut scores = Vec::new();
        let mut offset = 0;
        loop {
            let length = MAX_PAGE.min(self.max_rows.saturating_sub(offset));
            if length == 0 {
                break;
            }
            let body = fetch_json(&self.page_url(offset, length), self.token.as_deref()).await?;
            let page = self.parse(&body);
            let received = body
                .get("rows")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            scores.extend(page);
            offset += received.max(1);
            // A short page is the last page; the row count confirms it for a
            // page that happens to be exactly full.
            let total = body
                .get("num_rows_total")
                .and_then(Value::as_u64)
                .map(|total| total as usize);
            if received < length || offset >= self.max_rows || total.is_some_and(|t| offset >= t) {
                break;
            }
        }
        Ok(scores)
    }
}

/// The first present, non-empty string field from `fields`.
fn first_string(row: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        row.get(*field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

/// The first present numeric field from `fields`. Numbers sent as strings are
/// accepted, because a dataset built from a spreadsheet will store them that
/// way.
fn first_number(row: &Value, fields: &[&str]) -> Option<f64> {
    fields.iter().find_map(|field| {
        let value = row.get(*field)?;
        match value {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.trim().parse::<f64>().ok(),
            _ => None,
        }
    })
}

/// Percent-encode a query value, keeping only the unreserved characters. The
/// dataset id contains a `/`, which must be sent as `%2F`.
fn encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lmarena() -> HuggingFaceRows {
        HuggingFaceRows::lmarena()
    }

    /// The response shape verified against the live rows API.
    fn lmarena_body() -> Value {
        serde_json::json!({
            "features": [
                { "name": "model_name" },
                { "name": "rating" },
                { "name": "category" }
            ],
            "rows": [
                {
                    "row_idx": 0,
                    "row": {
                        "model_name": "claude-fable-5.1-max",
                        "organization": "anthropic",
                        "rating": 1507.5817496991563,
                        "vote_count": 5783,
                        "rank": 1,
                        "category": "overall"
                    }
                },
                {
                    "row_idx": 1,
                    "row": {
                        "model_name": "glm-5.3-flash",
                        "rating": 1290.4,
                        "rank": 2,
                        "category": "coding"
                    }
                }
            ],
            "num_rows_total": 10606,
            "num_rows_per_page": 100
        })
    }

    #[test]
    fn keeps_only_the_configured_category() {
        let scores = lmarena().parse(&lmarena_body());
        assert_eq!(scores.len(), 1, "only the coding row survives");
        assert_eq!(scores[0].model, "glm-5.3-flash");
        assert_eq!(scores[0].score, 1290.4);
        assert_eq!(scores[0].benchmark, Benchmark::LmArenaCoding);

        // Without a category filter the overall row is taken as well, which is
        // why the default filters.
        let all = lmarena()
            .with_category(None::<String>)
            .parse(&lmarena_body());
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].model, "claude-fable-5.1-max");
    }

    #[test]
    fn a_dataset_that_renames_its_category_yields_nothing_rather_than_the_wrong_rank() {
        let body = serde_json::json!({
            "rows": [{ "row": { "model_name": "m", "rating": 1.0, "category": "code" } }]
        });
        assert!(lmarena().parse(&body).is_empty());
    }

    #[test]
    fn accepts_rows_without_the_row_wrapper_and_string_numbers() {
        let source = HuggingFaceRows::new(Benchmark::LiveCodeBench, "d", "c", "s")
            .with_category(None::<String>);
        let body = serde_json::json!({
            "rows": [{ "model": "qwen3-coder", "score": "0.71" }]
        });
        let scores = source.parse(&body);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].model, "qwen3-coder");
        assert!((scores[0].score - 0.71).abs() < 1e-9);
    }

    #[test]
    fn a_row_without_a_usable_score_is_dropped() {
        let source = HuggingFaceRows::new(Benchmark::LiveCodeBench, "d", "c", "s")
            .with_category(None::<String>);
        let body = serde_json::json!({
            "rows": [
                { "row": { "model_name": "no-score" } },
                { "row": { "model_name": "flag-score", "score": true } },
                { "row": { "model_name": "good", "score": 0.5 } }
            ]
        });
        let scores = source.parse(&body);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].model, "good");
    }

    #[test]
    fn a_body_without_rows_yields_nothing() {
        assert!(
            lmarena()
                .parse(&serde_json::json!({ "error": "unauthorized" }))
                .is_empty()
        );
    }

    #[test]
    fn pages_are_addressed_correctly() {
        let url = lmarena().page_url(200, 100);
        assert!(url.contains("offset=200"), "{url}");
        assert!(url.contains("length=100"), "{url}");
        assert!(
            url.contains("dataset=lmarena-ai%2Fleaderboard-dataset"),
            "{url}"
        );
        assert!(url.contains("split=latest"), "{url}");
    }

    #[test]
    fn the_gated_livecodebench_source_requires_a_token() {
        assert!(HuggingFaceRows::livecodebench().requires_api_key());
        assert!(HuggingFaceRows::livecodebench().needs_token());
        assert!(!lmarena().requires_api_key());
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(encode_query_value("a/b"), "a%2Fb");
        assert_eq!(encode_query_value("latest"), "latest");
        assert_eq!(encode_query_value("a b"), "a%20b");
    }
}

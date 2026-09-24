//! The catalog port: what the library needs from the agent's database to keep
//! model versions and benchmark scores across runs.
//!
//! This follows the same rule as [`crate::store`]: the library defines the
//! trait and ships [`NullStore`] / [`InMemoryStore`], while the agent implements
//! it over its own database. Nothing here may fail a run, so callers treat a
//! store error as "no stored catalog" and carry on with what discovery found.
//!
//! ## What is stored, and why only this
//!
//! A [`ModelRecord`] is the *data point per family/version*: which provider
//! serves which canonical model, what family/version it resolves to, its
//! context window and when it was last seen. Families and the inference rules
//! stay in code ([`crate::families`]) because they are logic; which versions
//! exist today is data, because it changes weekly.
//!
//! Disappearance is recorded, never deletion. A model that drops out of a
//! gateway's `/models` list gets a `missing_since` stamp: free and stealth
//! models come and go, and a history is worth more than a tidy table.

use crate::benchmarks::{Benchmark, BenchmarkScore, CodingRank, RankConfig, rank_models};
use crate::discovery::DiscoveredModel;
use crate::families::{ModelVersion, infer_model_version};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

/// One model version as a provider serves it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRecord {
    pub provider: String,
    /// The canonical model id, after the provider's normalization.
    pub model: String,
    /// The inferred family, when the id was recognizable.
    pub family: Option<String>,
    pub vendor: Option<String>,
    pub version: Option<String>,
    pub variant: Option<String>,
    /// A serving tier (`free`, `latest`) that marks a different wire model.
    pub tier: Option<String>,
    pub context_window: Option<u32>,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
    /// Set when a refresh no longer saw this model on its provider.
    pub missing_since: Option<SystemTime>,
}

impl ModelRecord {
    /// Build a record from one discovery result. An id no family matches is
    /// still stored — it exists on the wire, which is what matters — with its
    /// inference fields left empty.
    pub fn from_discovered(provider: &str, discovered: &DiscoveredModel, now: SystemTime) -> Self {
        let inferred: Option<ModelVersion> = infer_model_version(&discovered.id);
        Self {
            provider: provider.to_string(),
            model: discovered.id.clone(),
            family: inferred.as_ref().map(|version| version.family.to_string()),
            vendor: inferred.as_ref().map(|version| version.vendor.to_string()),
            version: inferred
                .as_ref()
                .and_then(|version| version.version.clone()),
            variant: inferred
                .as_ref()
                .and_then(|version| version.variant.clone()),
            tier: inferred.as_ref().and_then(|version| version.tier.clone()),
            context_window: discovered.context_window,
            first_seen: now,
            last_seen: now,
            missing_since: None,
        }
    }

    /// The family/version grouping key, when the id was recognized. This is
    /// what benchmark data can be joined on when the exact variant was never
    /// measured.
    pub fn group_key(&self) -> Option<String> {
        let family = self.family.as_ref()?;
        Some(match &self.version {
            Some(version) => format!("{family} {version}"),
            None => family.clone(),
        })
    }

    /// Whether the last refresh still saw this model.
    pub fn is_present(&self) -> bool {
        self.missing_since.is_none()
    }

    /// Whether two records describe the same model state, ignoring when it was
    /// last seen. A refresh always advances `last_seen`, so counting that as a
    /// change would report every model as updated on every poll; the store uses
    /// this to tell a real change from a seen-again model. Implementations must
    /// agree on it so `UpsertSummary` means the same thing everywhere.
    pub fn same_content(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.last_seen = other.last_seen;
        right.last_seen = other.last_seen;
        left == right
    }
}

/// What an upsert changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpsertSummary {
    pub inserted: usize,
    /// Records whose fields changed. A re-upsert of unchanged data counts as
    /// `unchanged`, so a caller can log a refresh without noise.
    pub updated: usize,
    pub unchanged: usize,
}

impl UpsertSummary {
    pub fn total(&self) -> usize {
        self.inserted + self.updated + self.unchanged
    }
}

/// The library's catalog port. The agent implements this; a store failure
/// degrades to "nothing remembered", never to a failed run.
#[async_trait]
pub trait ModelCatalogStore: Send + Sync {
    /// Insert or update model records, keyed by `(provider, model)`. Records
    /// not already present are inserted; existing ones keep their `first_seen`
    /// and clear any `missing_since`.
    async fn upsert_models(&self, records: &[ModelRecord]) -> anyhow::Result<UpsertSummary>;

    /// Stamp `missing_since` on this provider's models that are not in `seen`,
    /// which is what a refresh found. Returns how many were newly stamped.
    async fn mark_missing(
        &self,
        provider: &str,
        seen: &[String],
        now: SystemTime,
    ) -> anyhow::Result<usize>;

    /// Every stored model record, in a stable order.
    async fn models(&self) -> anyhow::Result<Vec<ModelRecord>>;

    /// Insert or replace benchmark scores, keyed by `(benchmark, model)`: a
    /// newer measurement replaces an older one for the same pair.
    async fn upsert_scores(&self, scores: &[BenchmarkScore]) -> anyhow::Result<usize>;

    /// Every stored score.
    async fn scores(&self) -> anyhow::Result<Vec<BenchmarkScore>>;

    /// The coding rank derived from the stored scores. Derived rather than
    /// stored, so changing the weights needs no refetch.
    async fn ranked(&self, config: &RankConfig) -> anyhow::Result<Vec<CodingRank>> {
        let scores = self.scores().await?;
        Ok(rank_models(&scores, config, SystemTime::now()))
    }
}

/// Fetch a provider's models, store them, and stamp whatever the refresh no
/// longer saw. This is the auto-upsert the agent calls in the background after
/// startup — lazily per provider, and never as a startup gate.
pub async fn refresh_models(
    store: &dyn ModelCatalogStore,
    provider: &str,
    discovered: &[DiscoveredModel],
    now: SystemTime,
) -> anyhow::Result<UpsertSummary> {
    let records: Vec<ModelRecord> = discovered
        .iter()
        .map(|model| ModelRecord::from_discovered(provider, model, now))
        .collect();
    let seen: Vec<String> = records.iter().map(|record| record.model.clone()).collect();
    let summary = store.upsert_models(&records).await?;
    if !discovered.is_empty() {
        // An empty discovery result means the gateway answered with nothing —
        // a failed or versionless response. Treating that as "every model
        // vanished" would stamp a whole provider out of the catalog on one bad
        // call, so it is left alone.
        store.mark_missing(provider, &seen, now).await?;
    }
    Ok(summary)
}

// ─── In-crate stores ────────────────────────────────────────────────────────

/// A catalog that remembers nothing: the default when no database is
/// configured, and what lookups that only need the in-process catalog use.
pub struct NullStore;

#[async_trait]
impl ModelCatalogStore for NullStore {
    async fn upsert_models(&self, _records: &[ModelRecord]) -> anyhow::Result<UpsertSummary> {
        Ok(UpsertSummary::default())
    }

    async fn mark_missing(
        &self,
        _provider: &str,
        _seen: &[String],
        _now: SystemTime,
    ) -> anyhow::Result<usize> {
        Ok(0)
    }

    async fn models(&self) -> anyhow::Result<Vec<ModelRecord>> {
        Ok(Vec::new())
    }

    async fn upsert_scores(&self, _scores: &[BenchmarkScore]) -> anyhow::Result<usize> {
        Ok(0)
    }

    async fn scores(&self) -> anyhow::Result<Vec<BenchmarkScore>> {
        Ok(Vec::new())
    }
}

/// An in-memory catalog: the crate's tests and a `--no-database` run.
#[derive(Default)]
pub struct InMemoryStore {
    models: Mutex<HashMap<(String, String), ModelRecord>>,
    scores: Mutex<HashMap<(Benchmark, String), BenchmarkScore>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ModelCatalogStore for InMemoryStore {
    async fn upsert_models(&self, records: &[ModelRecord]) -> anyhow::Result<UpsertSummary> {
        let mut models = self.models.lock();
        let mut summary = UpsertSummary::default();
        for record in records {
            let key = (record.provider.clone(), record.model.clone());
            match models.get_mut(&key) {
                Some(existing) => {
                    // Identity and history belong to the stored row: a refresh
                    // must not rewrite when a model was first seen, and it does
                    // clear a previous disappearance. `last_seen` always
                    // advances, so `same_content` ignores it.
                    let mut merged = record.clone();
                    merged.first_seen = existing.first_seen;
                    merged.missing_since = None;
                    if merged.same_content(existing) {
                        summary.unchanged += 1;
                    } else {
                        summary.updated += 1;
                    }
                    *existing = merged;
                }
                None => {
                    models.insert(key, record.clone());
                    summary.inserted += 1;
                }
            }
        }
        Ok(summary)
    }

    async fn mark_missing(
        &self,
        provider: &str,
        seen: &[String],
        now: SystemTime,
    ) -> anyhow::Result<usize> {
        let mut models = self.models.lock();
        let mut stamped = 0;
        for ((record_provider, model), record) in models.iter_mut() {
            if record_provider != provider || record.missing_since.is_some() {
                continue;
            }
            if !seen.contains(model) {
                record.missing_since = Some(now);
                stamped += 1;
            }
        }
        Ok(stamped)
    }

    async fn models(&self) -> anyhow::Result<Vec<ModelRecord>> {
        let mut models: Vec<ModelRecord> = self.models.lock().values().cloned().collect();
        models.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then_with(|| left.model.cmp(&right.model))
        });
        Ok(models)
    }

    async fn upsert_scores(&self, scores: &[BenchmarkScore]) -> anyhow::Result<usize> {
        let mut stored = self.scores.lock();
        let mut written = 0;
        for score in scores {
            stored.insert((score.benchmark, score.model.clone()), score.clone());
            written += 1;
        }
        Ok(written)
    }

    async fn scores(&self) -> anyhow::Result<Vec<BenchmarkScore>> {
        let mut scores: Vec<BenchmarkScore> = self.scores.lock().values().cloned().collect();
        scores.sort_by(|left, right| {
            left.benchmark
                .cmp(&right.benchmark)
                .then_with(|| left.model.cmp(&right.model))
        });
        Ok(scores)
    }
}

/// A store handle shared across the catalog and the agent.
pub type CatalogStoreHandle = Arc<dyn ModelCatalogStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn discovered(id: &str, context_window: Option<u32>) -> DiscoveredModel {
        DiscoveredModel {
            id: id.to_string(),
            raw_id: id.to_string(),
            context_window,
            created: None,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
        }
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    #[test]
    fn a_record_carries_the_inferred_family_and_version() {
        let record = ModelRecord::from_discovered(
            "tokenrouter",
            &discovered("deepseek/deepseek-v4-pro-0813", Some(131_072)),
            now(),
        );
        assert_eq!(record.family.as_deref(), Some("deepseek"));
        assert_eq!(record.version.as_deref(), Some("4"));
        assert_eq!(record.variant.as_deref(), Some("pro-0813"));
        assert_eq!(record.group_key().as_deref(), Some("deepseek 4"));
        assert!(record.is_present());
    }

    #[test]
    fn a_router_pseudo_model_is_stored_without_inference_fields() {
        let record =
            ModelRecord::from_discovered("openrouter", &discovered("openrouter/auto", None), now());
        assert!(record.family.is_none());
        assert!(record.group_key().is_none());
        // It is still a model on the wire, so it is still stored.
        assert_eq!(record.model, "openrouter/auto");
    }

    #[tokio::test]
    async fn upserting_the_same_data_twice_reports_it_unchanged_and_keeps_first_seen() {
        let store = InMemoryStore::new();
        let early = now() - Duration::from_secs(3600);
        let first =
            ModelRecord::from_discovered("groq", &discovered("openai/gpt-oss-120b", None), early);
        let summary = store.upsert_models(&[first.clone()]).await.unwrap();
        assert_eq!(summary.inserted, 1);
        assert_eq!(summary.updated, 0);

        // The same model seen again later, with a fresh timestamp.
        let second =
            ModelRecord::from_discovered("groq", &discovered("openai/gpt-oss-120b", None), now());
        let summary = store.upsert_models(&[second]).await.unwrap();
        assert_eq!(summary.unchanged, 1);
        let stored = store.models().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].first_seen, early, "first_seen is history");
        assert!(stored[0].is_present());
    }

    #[tokio::test]
    async fn a_changed_field_counts_as_an_update() {
        let store = InMemoryStore::new();
        store
            .upsert_models(&[ModelRecord::from_discovered(
                "groq",
                &discovered("m", None),
                now(),
            )])
            .await
            .unwrap();
        let summary = store
            .upsert_models(&[ModelRecord::from_discovered(
                "groq",
                &discovered("m", Some(8192)),
                now(),
            )])
            .await
            .unwrap();
        assert_eq!(summary.updated, 1);
        assert_eq!(store.models().await.unwrap()[0].context_window, Some(8192));
    }

    #[tokio::test]
    async fn a_model_that_disappears_is_stamped_not_deleted() {
        let store = InMemoryStore::new();
        let models = vec![discovered("a", None), discovered("b", None)];
        let summary = refresh_models(&store, "groq", &models, now())
            .await
            .unwrap();
        assert_eq!(summary.inserted, 2);

        // The next refresh only sees `a`.
        let summary = refresh_models(&store, "groq", &[discovered("a", None)], now())
            .await
            .unwrap();
        assert_eq!(summary.unchanged, 1);
        let stored = store.models().await.unwrap();
        assert_eq!(stored.len(), 2, "a vanished model keeps its row");
        let b = stored.iter().find(|record| record.model == "b").unwrap();
        assert!(!b.is_present());
        let a = stored.iter().find(|record| record.model == "a").unwrap();
        assert!(a.is_present());

        // Seeing it again clears the stamp.
        refresh_models(&store, "groq", &models, now())
            .await
            .unwrap();
        let b = store
            .models()
            .await
            .unwrap()
            .iter()
            .find(|record| record.model == "b")
            .cloned()
            .unwrap();
        assert!(b.is_present(), "a model that came back is present again");
    }

    #[tokio::test]
    async fn an_empty_discovery_result_does_not_stamp_a_whole_provider_out() {
        let store = InMemoryStore::new();
        refresh_models(&store, "groq", &[discovered("a", None)], now())
            .await
            .unwrap();
        // A gateway that answered with nothing (a failure or a shape change)
        // must not be read as "every model vanished".
        refresh_models(&store, "groq", &[], now()).await.unwrap();
        assert!(store.models().await.unwrap()[0].is_present());
    }

    #[tokio::test]
    async fn one_providers_refresh_does_not_stamp_another_providers_models() {
        let store = InMemoryStore::new();
        refresh_models(&store, "groq", &[discovered("m", None)], now())
            .await
            .unwrap();
        refresh_models(&store, "nvidia", &[discovered("m", None)], now())
            .await
            .unwrap();
        // `groq` no longer sees `m`, but `nvidia` still does and must be
        // untouched.
        refresh_models(&store, "groq", &[discovered("other", None)], now())
            .await
            .unwrap();
        let nvidia = store
            .models()
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.provider == "nvidia")
            .unwrap();
        assert!(nvidia.is_present());
    }

    #[tokio::test]
    async fn a_newer_score_replaces_an_older_one_for_the_same_pair() {
        let store = InMemoryStore::new();
        store
            .upsert_scores(&[BenchmarkScore {
                benchmark: Benchmark::SweBenchVerified,
                model: "a/b".into(),
                score: 10.0,
                at: now() - Duration::from_secs(3600),
                source: "test",
            }])
            .await
            .unwrap();
        store
            .upsert_scores(&[BenchmarkScore {
                benchmark: Benchmark::SweBenchVerified,
                model: "a/b".into(),
                score: 20.0,
                at: now(),
                source: "test",
            }])
            .await
            .unwrap();
        let scores = store.scores().await.unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].score, 20.0);
    }

    #[tokio::test]
    async fn the_rank_is_derived_from_the_stored_scores() {
        let store = InMemoryStore::new();
        let at = now();
        store
            .upsert_scores(&[
                BenchmarkScore {
                    benchmark: Benchmark::AaCodingIndex,
                    model: "good/good".into(),
                    score: 80.0,
                    at,
                    source: "test",
                },
                BenchmarkScore {
                    benchmark: Benchmark::AaCodingIndex,
                    model: "weak/weak".into(),
                    score: 20.0,
                    at,
                    source: "test",
                },
            ])
            .await
            .unwrap();
        let ranks = store.ranked(&RankConfig::default()).await.unwrap();
        assert_eq!(ranks.len(), 2);
        assert_eq!(ranks[0].model, "good/good");
    }

    #[tokio::test]
    async fn the_null_store_remembers_nothing_and_never_fails() {
        let store = NullStore;
        assert_eq!(
            store.upsert_models(&[]).await.unwrap(),
            UpsertSummary::default()
        );
        assert_eq!(store.mark_missing("groq", &[], now()).await.unwrap(), 0);
        assert!(store.models().await.unwrap().is_empty());
        assert!(store.scores().await.unwrap().is_empty());
        assert!(
            store
                .ranked(&RankConfig::default())
                .await
                .unwrap()
                .is_empty()
        );
    }
}

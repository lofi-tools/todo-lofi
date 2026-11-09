use crate::models_dev::{FetchedCatalog, ModelRow, family_id_for, hex_digest, humanize};
use crate::sources::{HarnessSource, ParsedPage};
use anyhow::{Context, Result, bail};
use duckdb::{Connection, params};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub struct ScrapedPage {
    pub source: &'static HarnessSource,
    pub html: String,
    pub fetched_at: String,
    pub parsed: ParsedPage,
}

#[derive(Debug, Default, Serialize)]
pub struct WriteReport {
    pub providers: usize,
    pub models: usize,
    pub canonical_models: usize,
    pub plans: usize,
    pub offers: usize,
    pub sources: usize,
    pub retained_providers: usize,
    pub retained_models: usize,
    pub skipped_plans: Vec<String>,
}

pub struct SnapshotStore {
    connection: Connection,
    path: PathBuf,
}

impl SnapshotStore {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("opening DuckDB snapshot {}", path.display()))?;
        let version: i32 = connection
            .query_row("SELECT schema_version FROM catalog_meta", [], |row| {
                row.get(0)
            })
            .context("reading catalog_meta.schema_version")?;
        if version != 3 {
            bail!("snapshot schema is {version}; this ingest binary writes schema 3");
        }
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    pub fn write(
        &mut self,
        models_dev: Option<&FetchedCatalog>,
        pages: &[ScrapedPage],
    ) -> Result<WriteReport> {
        let mut report = WriteReport::default();
        let transaction = self.connection.transaction()?;

        if let Some(catalog) = models_dev {
            transaction.execute(
                "CREATE OR REPLACE TEMP TABLE ingest_providers (provider_id VARCHAR)",
                [],
            )?;
            transaction.execute(
                "CREATE OR REPLACE TEMP TABLE ingest_models (provider_id VARCHAR, model_id VARCHAR)",
                [],
            )?;
            transaction.execute(
                "UPDATE sources SET fetched_at = CAST(? AS TIMESTAMP), http_status = 200,
                        etag = ?, content_sha256 = ?, status = 'ok', error = NULL
                 WHERE source_id = 'models-dev'",
                params![catalog.fetched_at, catalog.etag, catalog.content_sha256],
            )?;

            for provider in &catalog.providers {
                transaction.execute(
                    "INSERT OR REPLACE INTO providers (
                       provider_id, name, npm, api_url, doc_url, env_keys, logo_path,
                       retained, source_id, source_url, source_record
                     ) VALUES (?, ?, ?, ?, ?, ?, NULL, FALSE, 'models-dev', ?, ?)",
                    params![
                        provider.provider_id,
                        provider.name,
                        provider.npm,
                        provider.api_url,
                        provider.doc_url,
                        serde_json::to_string(&provider.env_keys)?,
                        catalog.source_url,
                        provider.source_record,
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO ingest_providers VALUES (?)",
                    params![provider.provider_id],
                )?;
                report.providers += 1;
            }

            // A provider or model that a later response omits may have been
            // dropped by the source or merely left out of a partial response.
            // Keep the row and flag it instead of silently deleting the fact.
            report.retained_providers = transaction.execute(
                "UPDATE providers SET retained = TRUE
                 WHERE source_id = 'models-dev'
                   AND NOT EXISTS (
                     SELECT 1 FROM ingest_providers i
                     WHERE i.provider_id = providers.provider_id
                   )",
                [],
            )?;

            let mut family_ids = read_family_ids(&transaction)?;
            let mut canonical_models = HashSet::new();
            for model in &catalog.models {
                let family_id = family_id_for(&model.canonical_model_id, &family_ids);
                if !family_ids.iter().any(|existing| existing == &family_id) {
                    transaction.execute(
                        "INSERT OR IGNORE INTO model_families (
                           family_id, name, parent_family_id, vendor
                         ) VALUES (?, ?, NULL, ?)",
                        params![family_id, humanize(&family_id), family_id],
                    )?;
                    family_ids.push(family_id.clone());
                }
                if canonical_models.insert(model.canonical_model_id.clone()) {
                    transaction.execute(
                        "INSERT OR IGNORE INTO canonical_models (
                           canonical_model_id, name, lab, family_id
                         ) VALUES (?, ?, ?, ?)",
                        params![
                            model.canonical_model_id,
                            model
                                .name
                                .clone()
                                .unwrap_or_else(|| humanize(&model.canonical_model_id)),
                            model
                                .canonical_model_id
                                .split('/')
                                .next()
                                .unwrap_or("unknown"),
                            family_id,
                        ],
                    )?;
                    report.canonical_models += 1;
                }
                transaction.execute(
                    "INSERT INTO ingest_models VALUES (?, ?)",
                    params![model.provider_id, model.model_id],
                )?;
            }

            // DuckDB re-checks the primary key for every INSERT OR REPLACE row,
            // which is far too slow for the full model set. Keep the rows the
            // new response omits, replace the source rows, then insert fresh.
            transaction.execute(
                "CREATE OR REPLACE TEMP TABLE retained_models AS
                 SELECT m.* FROM models m
                 ANTI JOIN ingest_models i
                   ON i.provider_id = m.provider_id
                  AND i.model_id = m.model_id
                 WHERE m.source_id = 'models-dev'",
                [],
            )?;
            let retained: i64 =
                transaction
                    .query_row("SELECT COUNT(*) FROM retained_models", [], |row| row.get(0))?;
            report.retained_models = retained as usize;
            transaction.execute("UPDATE retained_models SET retained = TRUE", [])?;
            transaction.execute("DELETE FROM models WHERE source_id = 'models-dev'", [])?;
            transaction.execute("INSERT INTO models SELECT * FROM retained_models", [])?;

            for model in &catalog.models {
                write_model(&transaction, catalog, model)?;
                report.models += 1;
            }
        }

        let mut provider_ids = read_provider_ids(&transaction)?;
        // A source that is no longer configured has no page that could delete
        // its rows, so the run records the active set and prunes the rest.
        transaction.execute(
            "CREATE OR REPLACE TEMP TABLE active_sources (source_id VARCHAR)",
            [],
        )?;
        transaction.execute("INSERT INTO active_sources VALUES ('models-dev')", [])?;
        transaction.execute(
            "CREATE OR REPLACE TEMP TABLE active_harnesses (harness_id VARCHAR)",
            [],
        )?;
        for page in pages {
            write_source(&transaction, page, &mut report)?;
            transaction.execute(
                "INSERT INTO active_sources VALUES (?)",
                params![page.source.source_id],
            )?;
            transaction.execute(
                "INSERT INTO active_harnesses VALUES (?)",
                params![page.source.harness_id],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO harnesses (harness_id, name, vendor, homepage_url)
                 VALUES (?, ?, ?, ?)",
                params![
                    page.source.harness_id,
                    page.source.harness_name,
                    page.source.vendor,
                    page.source.homepage_url,
                ],
            )?;
            if let Some((provider_id, provider_name)) = page.source.vendor_provider
                && provider_ids.insert(provider_id.to_string())
            {
                // A harness vendor that sells its own plans has no models.dev
                // provider entry, so the plan facts need a provider to hang on.
                transaction.execute(
                    "INSERT OR IGNORE INTO providers (
                       provider_id, name, npm, api_url, doc_url, env_keys, logo_path,
                       retained, source_id, source_url, source_record
                     ) VALUES (?, ?, NULL, NULL, ?, '[]', NULL, FALSE, ?, ?, ?)",
                    params![
                        provider_id,
                        provider_name,
                        page.source.homepage_url,
                        page.source.source_id,
                        page.source.url,
                        format!("{}#provider", page.source.url),
                    ],
                )?;
            }
            if let Some(first_plan) = page.parsed.plans.first()
                && provider_ids.contains(&first_plan.provider_id)
            {
                transaction.execute(
                    "INSERT OR IGNORE INTO provider_harnesses (
                       provider_id, harness_id, exclusive, source_url
                     ) VALUES (?, ?, FALSE, ?)",
                    params![
                        first_plan.provider_id,
                        page.source.harness_id,
                        page.source.url,
                    ],
                )?;
            }
            // Each page is authoritative for its own plan facts, so a plan the
            // vendor stopped selling does not linger from an earlier run.
            transaction.execute(
                "DELETE FROM free_offers WHERE source_id = ?",
                params![page.source.source_id],
            )?;
            transaction.execute(
                "DELETE FROM plans WHERE source_id = ?",
                params![page.source.source_id],
            )?;
            for plan in &page.parsed.plans {
                if !provider_ids.contains(&plan.provider_id) {
                    report.skipped_plans.push(format!(
                        "{} (missing provider {})",
                        plan.plan_id, plan.provider_id
                    ));
                    continue;
                }
                transaction.execute(
                    "INSERT OR REPLACE INTO plans (
                       plan_id, provider_id, name, usage_model, billing_period, price, currency,
                       included_allowance, allowance_unit, source_id, source_url, source_record
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        plan.plan_id,
                        plan.provider_id,
                        plan.name,
                        plan.usage_model,
                        plan.billing_period,
                        plan.price,
                        plan.currency,
                        plan.included_allowance,
                        plan.allowance_unit,
                        page.source.source_id,
                        page.source.url,
                        format!("{}#{}", page.source.url, plan.plan_id),
                    ],
                )?;
                report.plans += 1;
            }
            for offer in &page.parsed.offers {
                transaction.execute(
                    "INSERT OR REPLACE INTO free_offers (
                       offer_id, provider_id, plan_id, offer_kind, allowance, allowance_unit,
                       reset_period, eligibility, conditions, source_id, source_url, source_record
                     ) VALUES (?, ?, ?, 'recurring', ?, ?, ?, NULL, ?, ?, ?, ?)",
                    params![
                        offer.offer_id,
                        offer.provider_id,
                        offer.plan_id,
                        offer.allowance,
                        offer.allowance_unit,
                        offer.reset_period,
                        offer.conditions,
                        page.source.source_id,
                        page.source.url,
                        format!("{}#{}", page.source.url, offer.offer_id),
                    ],
                )?;
                report.offers += 1;
            }
        }

        transaction.execute(
            "DELETE FROM free_offers WHERE NOT EXISTS (
               SELECT 1 FROM active_sources a WHERE a.source_id = free_offers.source_id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM plans WHERE NOT EXISTS (
               SELECT 1 FROM active_sources a WHERE a.source_id = plans.source_id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM provider_harnesses WHERE NOT EXISTS (
               SELECT 1 FROM active_harnesses a
               WHERE a.harness_id = provider_harnesses.harness_id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM harnesses WHERE NOT EXISTS (
               SELECT 1 FROM active_harnesses a WHERE a.harness_id = harnesses.harness_id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM providers WHERE source_id <> 'models-dev' AND NOT EXISTS (
               SELECT 1 FROM active_sources a WHERE a.source_id = providers.source_id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM sources WHERE NOT EXISTS (
               SELECT 1 FROM active_sources a WHERE a.source_id = sources.source_id
             )",
            [],
        )?;

        transaction.commit()?;
        self.update_checksum()?;
        Ok(report)
    }

    fn update_checksum(&self) -> Result<()> {
        let bytes = fs::read(&self.path)
            .with_context(|| format!("reading {} for checksum", self.path.display()))?;
        let checksum = hex_digest(&bytes);
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .context("snapshot path has no file name")?;
        let checksum_path = PathBuf::from(format!("{}.sha256", self.path.display()));
        fs::write(&checksum_path, format!("{checksum}  {file_name}\n"))
            .with_context(|| format!("writing {}", checksum_path.display()))?;
        Ok(())
    }
}

fn write_source(
    connection: &Connection,
    page: &ScrapedPage,
    report: &mut WriteReport,
) -> Result<()> {
    connection.execute(
        "INSERT OR REPLACE INTO sources (
           source_id, name, source_url, license, attribution, fetched_at,
           http_status, etag, content_sha256, status, error
         ) VALUES (?, ?, ?, 'Not declared', ?, CAST(? AS TIMESTAMP), 200, NULL, ?, 'ok', NULL)",
        params![
            page.source.source_id,
            page.source.name,
            page.source.url,
            format!("Scraped from {}", page.source.url),
            page.fetched_at,
            hex_digest(page.html.as_bytes()),
        ],
    )?;
    report.sources += 1;
    Ok(())
}

fn write_model(connection: &Connection, catalog: &FetchedCatalog, model: &ModelRow) -> Result<()> {
    connection.execute(
        "INSERT INTO models (
           provider_id, model_id, model_type, canonical_model_id, name, family, description,
           release_date, last_updated, knowledge_date, open_weights, attachment, reasoning,
           tool_call, structured_output, temperature, interleaved, context_limit, input_limit,
           output_limit, cost_input, cost_output, cost_reasoning, cost_cache_read,
           cost_cache_write, cost_input_audio, cost_output_audio, modalities_input,
           modalities_output, status, retained, source_id, source_url, source_record
         ) VALUES (
           ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
           ?, ?, ?, FALSE, 'models-dev', ?, ?
         )",
        params![
            model.provider_id,
            model.model_id,
            model.model_type,
            model.canonical_model_id,
            model.name,
            model.family,
            model.description,
            model.release_date,
            model.last_updated,
            model.knowledge_date,
            model.open_weights,
            model.attachment,
            model.reasoning,
            model.tool_call,
            model.structured_output,
            model.temperature,
            model.interleaved,
            model.context_limit,
            model.input_limit,
            model.output_limit,
            model.cost_input,
            model.cost_output,
            model.cost_reasoning,
            model.cost_cache_read,
            model.cost_cache_write,
            model.cost_input_audio,
            model.cost_output_audio,
            serde_json::to_string(&model.modalities_input)?,
            serde_json::to_string(&model.modalities_output)?,
            model.status,
            catalog.source_url,
            model.source_record,
        ],
    )?;
    Ok(())
}

fn read_provider_ids(connection: &Connection) -> Result<HashSet<String>> {
    let mut statement = connection.prepare("SELECT provider_id FROM providers")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut ids = HashSet::new();
    for row in rows {
        ids.insert(row?);
    }
    Ok(ids)
}

fn read_family_ids(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT family_id FROM model_families")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{HarnessSource, Parser, ScrapedPlan};

    static TEST_SOURCE: HarnessSource = HarnessSource {
        harness_id: "test-harness",
        harness_name: "Test Harness",
        vendor: "Test Vendor",
        homepage_url: "https://example.com",
        source_id: "test-source",
        name: "Test source",
        url: "https://example.com/pricing",
        parser: Parser::Warp,
        vendor_provider: None,
    };

    fn latest_snapshot() -> PathBuf {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../providers-catalog-web/artifacts");
        let mut paths: Vec<PathBuf> = fs::read_dir(&directory)
            .expect("artifacts directory")
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension().and_then(|ext| ext.to_str()) == Some("duckdb")).then_some(path)
            })
            .collect();
        paths.sort();
        paths.pop().expect("a snapshot exists")
    }

    fn page(prices: &[(&str, f64)]) -> ScrapedPage {
        ScrapedPage {
            source: &TEST_SOURCE,
            html: "<html></html>".to_string(),
            fetched_at: "2026-11-09T00:00:00Z".to_string(),
            parsed: ParsedPage {
                plans: prices
                    .iter()
                    .map(|(suffix, price)| ScrapedPlan {
                        plan_id: format!("test-{suffix}"),
                        provider_id: "anthropic".to_string(),
                        name: format!("Test {suffix}"),
                        usage_model: "subscription".to_string(),
                        billing_period: Some("month".to_string()),
                        price: Some(*price),
                        currency: Some("USD".to_string()),
                        included_allowance: None,
                        allowance_unit: None,
                    })
                    .collect(),
                offers: Vec::new(),
            },
        }
    }

    fn write_pages(path: &Path, prices: &[(&str, f64)]) {
        let mut store = SnapshotStore::open(path).expect("snapshot opens");
        store.write(None, &[page(prices)]).expect("page writes");
    }

    fn plan_prices(path: &Path) -> Vec<(String, f64)> {
        let connection = Connection::open(path).expect("snapshot reopens");
        let mut statement = connection
            .prepare(
                "SELECT plan_id, CAST(price AS DOUBLE) FROM plans
                 WHERE source_id = 'test-source' ORDER BY plan_id",
            )
            .expect("statement prepares");
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })
            .expect("plans query runs");
        rows.map(|row| row.expect("plan row reads")).collect()
    }

    #[test]
    fn rewriting_a_source_is_idempotent_and_drops_removed_plans() {
        let temp = std::env::temp_dir().join(format!(
            "providers-catalog-rewrite-{}.duckdb",
            std::process::id()
        ));
        fs::copy(latest_snapshot(), &temp).expect("snapshot copies");

        let original = [("one", 10.0), ("two", 20.0)];
        write_pages(&temp, &original);
        let first_run = plan_prices(&temp);
        assert_eq!(
            first_run,
            vec![
                ("test-one".to_string(), 10.0),
                ("test-two".to_string(), 20.0),
            ]
        );

        write_pages(&temp, &original);
        assert_eq!(
            plan_prices(&temp),
            first_run,
            "a second identical run must not change rows"
        );

        write_pages(&temp, &[("one", 15.0)]);
        assert_eq!(
            plan_prices(&temp),
            vec![("test-one".to_string(), 15.0)],
            "a changed price is replaced and a dropped plan disappears"
        );

        let connection = Connection::open(&temp).expect("snapshot reopens for counts");
        let orphan_offers: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM free_offers WHERE source_id = 'test-source'",
                [],
                |row| row.get(0),
            )
            .expect("offers count runs");
        assert_eq!(orphan_offers, 0, "removed plans leave no offer rows behind");
        drop(connection);

        fs::remove_file(&temp).expect("temp snapshot removed");
        fs::remove_file(format!("{}.sha256", temp.display())).ok();
    }
}

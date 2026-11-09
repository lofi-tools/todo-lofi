mod models_dev;
mod sources;
mod store;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Serialize;
use sources::{HarnessSource, SOURCES};
use spider::traits::PageData;
use spider::website::Website;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use store::{ScrapedPage, SnapshotStore};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Parser, Debug)]
#[command(about = "Fetch models.dev and scrape harness plan/usage pages into a DuckDB snapshot")]
struct Cli {
    /// DuckDB snapshot to upsert. Defaults to the newest snapshot in the catalog artifacts.
    #[arg(long)]
    snapshot: Option<PathBuf>,
    /// Scrape only this harness id. Repeat for several harnesses.
    #[arg(long = "harness")]
    harnesses: Vec<String>,
    /// Skip the models.dev fetch and provider/model upsert.
    #[arg(long)]
    skip_models_dev: bool,
    /// Parse and report without writing to DuckDB.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Serialize)]
struct SourceReport {
    harness_id: String,
    source_url: String,
    plans: usize,
    offers: usize,
}

#[derive(Serialize)]
struct IngestReport {
    snapshot: String,
    models_dev_providers: Option<usize>,
    models_dev_models: Option<usize>,
    sources: Vec<SourceReport>,
    failures: Vec<String>,
    written: Option<store::WriteReport>,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let snapshot = match cli.snapshot.clone() {
        Some(path) => path,
        None => latest_snapshot()?,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let models_dev = if cli.skip_models_dev {
        None
    } else {
        Some(
            models_dev::fetch(&client)
                .await
                .context("fetching models.dev catalog")?,
        )
    };

    let selected: Vec<&HarnessSource> = SOURCES
        .iter()
        .filter(|source| {
            cli.harnesses.is_empty() || cli.harnesses.iter().any(|id| id == source.harness_id)
        })
        .collect();
    if selected.is_empty() {
        bail!("no harness matched the --harness filters");
    }

    let mut pages = Vec::new();
    let mut failures = Vec::new();
    let mut reports = Vec::new();
    for source in selected {
        match scrape(&client, source).await {
            Ok(html) => match source.parse(&html) {
                Ok(parsed) => {
                    reports.push(SourceReport {
                        harness_id: source.harness_id.to_string(),
                        source_url: source.url.to_string(),
                        plans: parsed.plans.len(),
                        offers: parsed.offers.len(),
                    });
                    pages.push(ScrapedPage {
                        source,
                        html,
                        fetched_at: now_timestamp(),
                        parsed,
                    });
                }
                Err(error) => failures.push(format!("{}: {error}", source.url)),
            },
            Err(error) => failures.push(format!("{}: {error}", source.url)),
        }
    }

    let written = if cli.dry_run {
        None
    } else {
        let mut store = SnapshotStore::open(&snapshot)?;
        Some(store.write(models_dev.as_ref(), &pages)?)
    };

    let report = IngestReport {
        snapshot: snapshot.display().to_string(),
        models_dev_providers: models_dev.as_ref().map(|catalog| catalog.providers.len()),
        models_dev_models: models_dev.as_ref().map(|catalog| catalog.models.len()),
        sources: reports,
        failures: failures.clone(),
        written,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);

    if !failures.is_empty() {
        bail!("{} harness sources failed to scrape", failures.len());
    }
    Ok(())
}

async fn raw_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .header("user-agent", USER_AGENT)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !response.status().is_success() {
        bail!("{url} returned HTTP {}", response.status());
    }
    let body = response.text().await?;
    if body.trim().is_empty() {
        bail!("{url} returned an empty body");
    }
    Ok(body)
}

fn now_timestamp() -> String {
    // A naive UTC timestamp string keeps the DuckDB CAST deterministic; an
    // RFC 3339 offset string would depend on the session time zone.
    chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.f")
        .to_string()
}

async fn scrape(client: &reqwest::Client, source: &HarnessSource) -> Result<String> {
    if source.url.ends_with(".md") || source.url.ends_with(".txt") {
        // spider only retains parsed HTML pages; a raw quota document has to be
        // read as the text that it is.
        return raw_text(client, source.url).await;
    }

    let mut website = Website::new(source.url);
    // A bare request without a browser user agent is rejected by the CDN in
    // front of most vendor pricing pages.
    website.with_user_agent(Some(USER_AGENT));
    website.with_respect_robots_txt(false);
    // A limit of 1 leaves no budget for the start page itself, so the crawl
    // returns nothing; 2 keeps the scrape to the page and at most one link.
    website.with_limit(2);
    website.scrape().await;
    let pages = website
        .get_pages()
        .with_context(|| format!("{} returned no page", source.url))?;
    // The crawl can follow one link, so pick the requested page instead of
    // whichever page happened to be retained first.
    let page = pages
        .iter()
        .find(|page| page.get_url() == source.url || page.url_final() == source.url)
        .or_else(|| pages.first())
        .with_context(|| format!("{} returned no page", source.url))?;
    let html = page.get_html();
    if html.trim().is_empty() {
        bail!("{} returned an empty body", source.url);
    }
    Ok(html)
}

fn latest_snapshot() -> Result<PathBuf> {
    let artifacts = workspace_root().join("apps/providers-catalog-web/artifacts");
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in
        std::fs::read_dir(&artifacts).with_context(|| format!("reading {}", artifacts.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("duckdb") {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if newest
            .as_ref()
            .map(|(current, _)| modified > *current)
            .unwrap_or(true)
        {
            newest = Some((modified, path));
        }
    }
    newest
        .map(|(_, path)| path)
        .with_context(|| format!("no .duckdb snapshot in {}", artifacts.display()))
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives two levels below the workspace root")
}

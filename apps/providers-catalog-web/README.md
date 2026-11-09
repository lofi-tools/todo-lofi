# providers-catalog-web

The separate static catalog of coding-agent providers, models, plans and
recurring free offers. It is not part of the Shepherd desktop app and does not
become the agent pool's authority.

The product and data contracts are in
[`docs/spec/spec.md`](./docs/spec/spec.md); the implementation order is in
[`docs/plan.md`](./docs/plan.md).

## Snapshot

The sources are the public models.dev catalog and one official pricing page per
harness. A snapshot stores providers, companies and their plans, models,
canonical models, model families, harness links, benchmark definitions,
subscription plans and recurring free offers in DuckDB; local runtimes stay empty
until an approved source exists. There is no per-fact verification status: each
row keeps its source URL and source record, and the snapshot's age is shown on
every route.

```sh
pnpm --dir apps/providers-catalog-web run snapshot
pnpm --dir apps/providers-catalog-web run validate
pnpm --dir apps/providers-catalog-web run test
```

The Rust ingestion binary applies both sources to an existing snapshot. It
fetches models.dev and every harness pricing page on each run, so the plans and
recurring offers replace the previous values for those sources.

```sh
# Newest snapshot in apps/providers-catalog-web/artifacts
cargo run -p providers-catalog-ingest --
# One specific snapshot, or only some harnesses
cargo run -p providers-catalog-ingest -- --snapshot artifacts/<snapshot>.duckdb
cargo run -p providers-catalog-ingest -- --harness kiro --harness cursor
# Parse and report without writing
cargo run -p providers-catalog-ingest -- --dry-run
```

The harness sources and their caveats are listed in
[`docs/scraping-sources.md`](./docs/scraping-sources.md).

Snapshots are gitignored. A failed scrape never replaces the previous artifact.

To rebuild locally from a retained artifact without deploying:

```sh
pnpm --dir apps/providers-catalog-web run rebuild
pnpm --dir apps/providers-catalog-web run rebuild -- --snapshot artifacts --build
```

If the artifact is missing or expired, `rebuild` prints recovery steps and exits
zero; it never deploys. Downloading a retained GitHub Actions artifact is a
separate, credentials-dependent manual-dispatch step.
When the newest artifact is present in `artifacts/`, the writer retains any
provider or model omitted by the new response, preserves its `verified_at`, marks
it `missing`, and includes it in the snapshot report's review items. Recurring
offer rows are normalized before writing: exact duplicates merge deterministically,
conflicting duplicates fail, and one-time or expiring-trial wording is rejected.
Unit tests also write a synthetic agent, plan, recurring offer and local runtime;
that fixture does not enable an unapproved source.

The build also publishes `/harness-ranking.json`: a precomputed ranking of
harnesses by monthly free requests or tokens times the mean coding-benchmark
score of the models they can use. Requests and tokens are ranked separately.
The browser loads that small static asset directly; the page never runs DuckDB
or downloads the full catalog.

The build also publishes `/agent-pool-advisory.v2.json`, a versioned advisory
contract derived from recurring offers. Stale or incomplete records expose
`capacity: "unknown"`; local pool observations remain authoritative.

For Shepherd's pool, export a filtered local copy instead of fetching the site:

```sh
pnpm --dir apps/providers-catalog-web run export:pool
```

It writes `.cache/shepherd/pool-catalog-advisory.v2.json` with only Claude Code,
Codex and OpenCode, model-expanded limit records and provider/model choices.
The file is not committed. Shepherd reads it when present and never opens DuckDB
at runtime; adding a slot stays an explicit user action.

## Static site

The Astro site opens the newest `artifacts/*.duckdb` read-only at build time.
Set `CATALOG_DUCKDB_PATH` to build a specific snapshot. The build fails on a
missing snapshot, a non-singleton `catalog_meta` row or an unsupported schema
version.

```sh
pnpm --dir apps/providers-catalog-web run dev       # http://localhost:4325
pnpm --dir apps/providers-catalog-web run build
pnpm --dir apps/providers-catalog-web run preview   # http://localhost:4326
pnpm --dir apps/providers-catalog-web run test:e2e
```

Routes: `/`, `/models`, `/models/canonical/[...model]`, `/providers/[provider]`,
`/models/[provider]/[...model]`, `/companies`, `/companies/[company]`, `/families`,
`/families/[family]`, `/harnesses`, `/harnesses/[harness]`, `/ranking` and
`/about`. Providers and models are
statically generated; the browser only filters and ranks the embedded public
data. There is no runtime database, scraper endpoint or account data.

A provider company is the vendor; a provider entry can be one of its plans.
OpenCode is a company and also a harness name, while its provider entries are
the OpenCode Zen and OpenCode Go plans, and OpenCode Go Plus is a named plan with
no provider entry yet.

Canonical models correlate the same release across providers. Families are a
tree defined by a parent column, so a parent family includes its descendants.
Harness links record which coding agent a provider serves, including exclusive
links such as Anthropic providers only serving Claude Code and OpenAI/GPT
providers only serving Codex.

Plans come from the scraped official pricing pages. A recurring free offer is
written only when a page states an explicit numeric recurring allowance; vague
"limited" tiers and trials stay out of `free_offers`. The UI treats zero token
prices as model prices, never as free offers.

# Providers catalog — todo

Working checklist for the catalog data-model revision. Spec:
[spec.md](./spec/spec.md). Implementation order: [plan.md](./plan.md).

Schema v2. Every item below is implemented in one revision; the checklist is
kept as the record of what was asked.

## 1. Remove the verification

- [x] Drop fact-level verification columns from the schema: `verified_at`,
      `last_attempt_at`, `confidence`, `scrape_status`, `last_error`.
- [x] Drop `fact_sources` and the source-independent retention marker's
      verification semantics; retained rows are flagged `retained` instead.
- [x] Keep source links (`source_id`, `source_url`, `source_record`), license,
      attribution, snapshot freshness and the free-offer policy.
- [x] Remove the `Confidence`/`ScrapeStatus` types and every parser, mapper,
      merge path and validator check that depends on them.
- [x] Remove verification UI: "Verified" pills and table columns, the
      "Verified on or after" filter, the confidence/scrape-status rows in
      provenance, and verification wording in copy.
- [x] Update the public-claim check so it still forbids unsupported
      availability/free claims without requiring "verified" wording.
- [x] Update the agent-pool advisory record, which currently copies
      `confidence`, `verifiedAt` and `scrapeStatus`.
- [x] Regenerate the snapshot; schema v1 snapshots are rejected with a clear
      error.

## 2. Add model benchmarks

- [x] Add `benchmarks` (id, name, description, unit, higher-is-better, source
      URL) and `model_benchmarks` (canonical model, benchmark, score, source
      URL) to schema v2.
- [x] Key scores by canonical model, so one score covers every provider serving
      that model.
- [x] Add a curated benchmark definition set and an optional curated score set;
      never invent a score, and leave unknown scores unknown.
- [x] Show benchmark scores on the canonical model and model pages, with the
      benchmark source, and an explicit unknown state.
- [x] Validate duplicate scores, unknown benchmark ids and out-of-range units.

## 3. Correlate models across providers

- [x] Add `canonical_models` (id, name, family, lab) and make
      `models.canonical_model_id` non-null.
- [x] Use the source's `canonical_model_id` when it publishes one, otherwise
      derive it from the wire model id and family markers, so the same release
      served by several providers, including harness-specific providers, lands
      on one canonical row.
- [x] Normalize dots to dashes so dotted and dashed wire ids agree.
- [x] Add `/models/canonical/[model]` listing every provider serving the
      canonical model, with per-provider facts and links.
- [x] Link model detail pages to their canonical model and sibling providers.
- [x] Treat harness-specific providers as normal servings of the canonical
      model, not as separate models.

## 4. Relate providers to harnesses

- [x] Replace the empty `agents`/`agent_*_links` tables with `harnesses` and
      `provider_harnesses` (provider, harness, exclusive).
- [x] Seed harness identities (Claude Code, Codex, Gemini CLI, OpenCode,
      Copilot)
      and their provider links.
- [x] Encode the exclusivity the request names: Anthropic/Claude providers
      serve Claude Code only; OpenAI/GPT providers serve Codex only.
- [x] Validate that an exclusive provider link is the provider's only harness
      link, and that link endpoints exist.
- [x] Show harnesses on provider pages and harness pages, and add a harness
      filter to the model browser.
- [x] Remove the `/agents` routes in favour of `/harnesses`.

## 5. Add model families

- [x] Add `model_families` (`family_id`, `name`, `parent_family_id`) as a tree
      defined by the parent column, and point canonical models at a leaf family.
- [x] Seed the requested trees, for example
      `claude → opus → opus-5.5` and
      `deepseek → deepseek-v4.1 → deepseek-v4.1-0731`.
- [x] Infer parents of parents by following each parent family's own parent,
      exposing the full ancestor chain as nested tags.
- [x] Detect cycles and missing parents during validation instead of looping.
- [x] Show the family chain on model and canonical model pages, and add a
      family filter that includes descendant families.

## 6. Distinguish provider companies from provider plans

- [x] Add `companies` and `company_plans` (company, plan name, optional provider
      entry, source URL) so a provider entry can be a company's plan rather than
      the company itself.
- [x] Seed the OpenCode company with its OpenCode Zen, OpenCode Go and OpenCode
      Go Plus plans, and link the `opencode` and `opencode-go` provider entries
      to the plans they serve. OpenCode Go Plus has no provider entry yet.
- [x] Keep the names distinct in the UI: OpenCode is the company and also a
      harness, while the provider entries are shown as plans of that company.
- [x] Add `/companies` and `/companies/[company]` pages and show the company
      and plan on provider, harness and model-browser rows.
- [x] Validate that a plan's company exists, that a mapped provider entry
      exists, and that a provider entry is not linked to several plans.

## 7. Replace compare with a harness ranking

- [x] Remove the Compare tab, page, selection controls, links and tests.
- [x] Rank harnesses by monthly recurring free requests/tokens times the mean
      coding-benchmark score of the models the harness can use.
- [x] Average a model's coding benchmark scores first, then average across the
      canonical models a harness can reach; never estimate a missing score.
- [x] Keep requests and tokens in separate rankings, because the units are not
      interchangeable, and state the missing inputs when nothing is rankable.
- [x] Precompute the ranking at build and publish `/ranking` plus
      `/harness-ranking.json`; the browser loads a small static asset and never
      runs DuckDB.

## 8. Scrape harness plans in Rust

- [x] Move the models.dev fetch, harness scraping and DuckDB upsert into one
      Rust binary, `apps/providers-catalog-ingest`, built on spider.
- [x] Scrape one official page per harness: Claude Code, Codex, Gemini CLI,
      OpenCode, GitHub Copilot, Cursor, Devin, Kiro and OpenHands.
- [x] Extend the scrape to more corporate harnesses whose pages publish plan
      prices: Amazon Q Developer, Warp, Zed, Augment Code, Factory and Replit
      Agent. JetBrains AI, Tabnine, Continue, Sourcegraph, Qodo, Cerebras and
      Windsurf were checked and excluded because their plan pages cannot be
      scraped for a real plan price.
- [x] Re-read every page on each run and keep each source authoritative for its
      own plans, so a plan the vendor stops selling is removed.
- [x] Write only explicit numeric recurring free allowances to `free_offers`;
      trials, vague limits and plan caps stay out.
- [x] Add the new harnesses and their vendor provider entries so plan rows
      satisfy the provider join.

## Gate

- [x] `pnpm --dir apps/providers-catalog-web run typecheck`
- [x] `pnpm --dir apps/providers-catalog-web run test`
- [x] `pnpm --dir apps/providers-catalog-web run snapshot` (schema v2)
- [x] `pnpm --dir apps/providers-catalog-web run validate`
- [x] `pnpm --dir apps/providers-catalog-web run build`
- [x] `pnpm --dir apps/providers-catalog-web run test:e2e`

## Done

Evidence (2026-11-09): package typecheck passed; 38 Vitest tests passed; a fresh
models.dev scrape produced `providers-catalog-2-20261109T161828Z-e84d5fb9.duckdb`
with 235 providers, 8,996 model entries, 1,649 canonical models, 218 families,
5 harnesses and 4 benchmark definitions; `validate` passed with zero errors and
zero warnings; the static build emitted 11,109 pages.

Schema v3 adds provider companies and their plans. OpenCode is the company,
OpenCode Zen, OpenCode Go and OpenCode Go Plus are its plans, and the `opencode`
and `opencode-go` provider entries are mapped to the first two. Snapshot
`providers-catalog-3-20261109T163951Z-7b6ceb76.duckdb` holds 1 company and 3
plans alongside 235 providers, 8,993 model entries, 1,647 canonical models, 218
families, 5 harnesses and 4 benchmark definitions. Typecheck passed with zero
errors, 39 Vitest tests passed, `validate` passed, the build emitted 11,106
pages, and all 13 Playwright catalog tests passed.

The Compare tab, page, selection controls, links and tests were removed. They
are replaced by `/ranking` and `/harness-ranking.json`, which precompute the
monthly free requests/tokens × mean coding-score ranking at build time. The
snapshot currently publishes no recurring free offer and no benchmark score, so
the page reports that no harness is rankable instead of inventing a score.
Typecheck passed with zero errors, 39 Vitest tests passed, the build emitted
11,106 pages, and all 13 Playwright catalog tests passed.

Rust ingestion (2026-11-09): `apps/providers-catalog-ingest` fetches models.dev
and scrapes fifteen harness pricing pages with spider, then upserts into the
schema-3 snapshot. A live run refreshed 235 providers and 9,001 model entries,
wrote 15 source rows, 53 plans and 5 recurring offers (Amazon Q 1, Copilot 1,
Gemini 2, Kiro 1), rewrote the checksum sidecar and left no skipped plans. Each
source is authoritative for its own plans, so a plan the vendor stops selling is
removed, and a source that is no longer configured is pruned with its plans,
offers and harness rows. Two consecutive live runs produced identical plan and
offer rows. Cargo tests cover every parser and the delete-and-rewrite path (22
passed); `validate` reported 15 harnesses, 53 plans and 5 offers with zero errors
and zero warnings; the build emitted 11,139 pages, and clippy passed with
`-D warnings`.

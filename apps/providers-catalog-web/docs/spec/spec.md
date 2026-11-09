# Providers catalog — specification

Status: S1 (snapshot/schema) and S3 (static catalog site) implemented on
2026-11-09. The S2 merge policy, review summary, free-offer normalization and
synthetic fixture gate are implemented; S2 sources for plans/offers/agents/local
runtimes remain blocked. The S5 advisory JSON contract is published at
`/agent-pool-advisory.v1.json`; pool wiring remains separate. S4 is blocked on
Cloudflare Pages credentials. No deployment exists yet.

This is a separate public site from Shepherd. It compares coding-agent
providers, plans, models and recurring free offers. It is informational only:
it does not become the authority for the agent pool, and it does not expose a
runtime database or a scraper endpoint. The Shepherd roadmap item is
`free-tier-catalog` in [`apps/shepherd/docs/todo.md`](../../../shepherd/docs/todo.md).

## 0. Decisions from the interview

These answers are normative for the first implementation.

| Area | Decision |
| --- | --- |
| Surface | A separate site in this repository, as a new pnpm workspace package at `apps/providers-catalog-web`. It is not added to the Shepherd desktop app or the existing docs site. |
| Spec location | `apps/providers-catalog-web/docs/spec/spec.md`. |
| Hosting | Cloudflare Pages, using the Pages host URL (`*.pages.dev`). No custom domain is specified. |
| Schedule | A GitHub Actions scheduled workflow runs weekly, with manual dispatch for recovery. |
| Snapshots | GitHub Actions artifacts only. DuckDB snapshots are not committed to the repository. |
| First source | `models.dev`, using its public data/API directly. No HTML parsing is required for this source. |
| Source access | Public pages and public data only. No provider credentials, private account data or paid API access. |
| Free offers | Only recurring free allowances. Exclude launch credits, expiring trials, one-time grants and unverified community claims. |
| Browse scope | Implement the full filters and comparison from the roadmap item, not a reduced launch view. |
| Freshness | Warn when the snapshot is older than 7 days, keep showing the last verified value, and never replace a verified value with a scrape failure. |
| Agent pool | The catalog publishes an advisory-only contract now. Pool implementation comes later and local usage/provider responses remain authoritative. |
| Checks | Add checks for the new package and include the package in the root typecheck. |
| First slice | Scraper and DuckDB schema first, before the public catalog site. |

## 1. Purpose and non-goals

### Purpose

A visitor can answer:

- Which coding agents are available, and which providers/plans/models can they
  use?
- Which plans have a recurring free allowance, what is the allowance and reset
  period, and what conditions apply?
- Which models are available from a provider, with context limits, modalities,
  pricing and capabilities?
- Which open-weight models can be run locally, and what runtime and approximate
  hardware do they need?
- What is the source and verification date for each claim?
- Which options are comparable on free/paid status, agent, provider, model,
  context, modality, local/self-hosted status and last verification?

### Non-goals

- No runtime database, public query API, scraper endpoint or background service.
- No provider credentials, account scraping or private usage data.
- No claim that a quota is guaranteed. A published allowance is a sourced
  claim with conditions, not a promise from the provider or from this site.
- No inferred free tier from a zero token price. A zero-priced model is a model
  fact, not a recurring free offer.
- No Taskstream content, old `docs-web` paths, internal reference-project names,
  or live `/downloads` and `/roadmap` links.
- The catalog does not become the agent pool's source of truth. See §9.

## 2. Product experience

The site is static and searchable. All catalog data is read at build time and
serialized into pages and a client-side search index.

### Routes

| Route | Purpose |
| --- | --- |
| `/` | Catalog entry point: search, filters, counts and the freshness banner. |
| `/agents/[agent]` | One coding agent, its compatible providers, plans and models, with provenance. |
| `/providers/[provider]` | One provider, its models, plans and free offers. |
| `/models/[provider]/[model]` | One model as served by a provider, including canonical model identity, limits, pricing, modalities and capabilities. |
| `/compare` | Side-by-side comparison of selected agents, plans, offers or models. Selection is encoded in the URL where practical. |
| `/about` | Data sources, license/attribution, snapshot version, verification policy, known gaps and freshness policy. |

There is no runtime route that queries DuckDB. Dynamic-looking routes are
statically generated from the snapshot.

### Browse and filters

Filters cover at least:

- free offer / paid / unknown;
- agent;
- provider;
- model;
- context window;
- input/output modality;
- local/self-hosted;
- last verified date;
- plan usage model.

Filters must be keyboard-accessible, work with JavaScript disabled for the
initial server-rendered result where practical, and use the repository's
`web-design-system` tokens/components. The client-side index must not contain
private data; it is built from public facts only.

### Comparison

Comparison shows the selected entries side by side with:

- the source link for each fact;
- verification time and confidence;
- the plan usage model and conditions;
- the recurring allowance and reset period for free offers;
- local runtime and approximate hardware for local models, or an explicit
  unknown state;
- an explicit warning that quotas and prices can change and are not guaranteed.

The comparison must not normalize different allowance units into a single
"free" score. Unknown capacity stays unknown.

### Empty and stale states

- If no plans or free offers have been verified, the UI says so. It does not
  show an empty table as if the provider had no offers.
- If a fact is stale, the entry stays visible with the last verified value and a
  warning naming the verification date. A failed scrape does not turn the value
  into zero, free or unknown.
- If the snapshot is older than 7 days, every catalog route shows a freshness
  warning. The build still succeeds.
- If the snapshot is missing or has an unsupported schema version, the build
  fails rather than publishing an empty catalog.

## 3. Architecture

```
models.dev public data
        |
        v
  source adapter  ->  validation  ->  staging tables
                                        |
                                        v
                             versioned DuckDB snapshot
                                        |
                          GitHub Actions artifact (weekly)
                                        |
                                        v
                         Astro static build (build-time read)
                                        |
                                        v
                              Cloudflare Pages
```

- Package: `apps/providers-catalog-web`, a pnpm workspace member.
- Runtime for ingestion and build: Node 24, TypeScript, pnpm.
- DuckDB access: `@duckdb/node-api` (official Node client). The snapshot is a
  single file passed to the build through `CATALOG_DUCKDB_PATH`.
- Site: Astro, static output, using `libs/web-design-system`.
- Search: a build-generated JSON index queried in the browser. No search server.
- Schedule: GitHub Actions, weekly plus `workflow_dispatch`.
- Hosting: Cloudflare Pages. Preview and production both use the Pages host URL
  until a custom domain is explicitly added.
- No runtime database or scraper route is deployed.

### Package boundaries

The package owns:

- source adapters and source registry;
- the DuckDB schema and migrations;
- snapshot creation, validation and checksums;
- the build-time data reader and generated search index;
- catalog routes, filters, comparison and provenance UI;
- tests and the public claim/link checks for catalog routes.

It does not own Shepherd's Rust core, the agent pool implementation, or the
existing docs site.

## 4. Data model

The schema is versioned (`schema_version = 1` initially). All tables use
deterministic IDs so repeated snapshots are diffable.

Every fact-bearing table carries the same provenance columns:

| Column | Meaning |
| --- | --- |
| `source_id` | Foreign key to `sources`. |
| `source_url` | The public URL the fact was read from, or the API URL when the source is a dataset. |
| `source_record` | The record path within that source, for example `provider/model`. |
| `verified_at` | UTC timestamp of the last successful observation of this fact. |
| `last_attempt_at` | UTC timestamp of the most recent scrape attempt for this fact. |
| `confidence` | `verified`, `community`, `inferred` or `unknown`. `models.dev` facts start as `community` until independently checked. |
| `scrape_status` | `ok`, `missing`, `failed` or `not_fetched`. |
| `last_error` | Nullable failure detail. Never contains credentials. |

### Core tables

| Table | Key | Purpose |
| --- | --- | --- |
| `sources` | `source_id` | Source URL, license, attribution, fetch time, HTTP status, ETag and content hash. |
| `scrape_runs` | `run_id` | One scheduled or manual run: start/end, status, source, error, artifact version. |
| `catalog_meta` | singleton | Snapshot ID, schema version, generated time, source times, known gaps and freshness deadline. |
| `agents` | `agent_id` | Coding-agent identity and links. May be empty in the first snapshot. |
| `providers` | `provider_id` | Provider name, npm package, API endpoint, docs URL, environment-variable names and logo reference. |
| `models` | `(provider_id, model_id)` | One model as served by one provider, including canonical identity and provider-specific fields. |
| `plans` | `plan_id` | A paid or free subscription/token plan. May be empty in the first snapshot. |
| `free_offers` | `offer_id` | One recurring free allowance claim. May be empty in the first snapshot. |
| `agent_provider_links` | `(agent_id, provider_id)` | Agent-to-provider compatibility. |
| `agent_plan_links` | `(agent_id, plan_id)` | Agent-to-plan compatibility. |
| `agent_model_links` | `(agent_id, provider_id, model_id)` | Agent-to-model compatibility. |
| `local_runtimes` | `(provider_id, model_id, runtime)` | Runtime, quantization and approximate hardware for local use. |
| `fact_sources` | `(fact_type, fact_id, source_id)` | Additional sources for a fact, with per-source verification and confidence. |

`agents`, `plans`, `free_offers` and `local_runtimes` are separate tables joined
through link tables. A quota claim is stored once in `free_offers`; it is not
copied into `agents`, `plans` or `models`. A plan can serve many models and an
agent can link to many plans and models without duplicating allowance text.

### Key fields

`providers`:

- `provider_id` (source key), `name`, `npm`, `api_url`, `doc_url`;
- `env_keys` as a JSON array of names only, never values;
- `logo_path` (optional, public asset).

`models`:

- `provider_id`, `model_id` (the source's model key);
- `canonical_model_id` (nullable; the lab/model identity when the source exposes
  one);
- `name`, `family`, `description`;
- `release_date`, `last_updated`, `knowledge_date`;
- `open_weights`, `attachment`, `reasoning`, `tool_call`,
  `structured_output`, `temperature`, `interleaved`;
- `context_limit`, `input_limit`, `output_limit`;
- `cost_input`, `cost_output`, `cost_reasoning`, `cost_cache_read`,
  `cost_cache_write`, `cost_input_audio`, `cost_output_audio`;
- `modalities_input`, `modalities_output` as JSON arrays;
- `status` (`alpha`, `beta`, `deprecated` or null).

Costs and prices use `DECIMAL(18, 6)` in USD and preserve null for unknown. A
zero token cost does not create a free offer.

`plans`:

- `plan_id`, `provider_id`, `name`;
- `usage_model`: `subscription`, `token_pack`, `pay_as_you_go`, `free` or
  `unknown`;
- `billing_period`, `price`, `currency`, `included_allowance`,
  `allowance_unit`;
- provenance columns, including the provider's official pricing/limits URL.

`free_offers`:

- `offer_id`, `provider_id`, nullable `plan_id`;
- `allowance`, `allowance_unit`, `reset_period`;
- `eligibility`, `conditions`;
- provenance columns, including the official source URL;
- a constraint or validation rule that rejects one-time credits and expiring
  trials by policy, not only by field name.

`local_runtimes`:

- `provider_id`, `model_id`, `runtime`, `quantization`,
  `approximate_hardware`, `setup_notes`;
- provenance columns.

### Validation rules

- IDs are stable, lowercase where the source allows it, and never derived from
  display text alone.
- `(provider_id, model_id)` is unique; `(agent_id, plan_id)` is unique;
  `(agent_id, provider_id, model_id)` is unique.
- Every fact row has non-null `source_url`, `verified_at`, `confidence` and
  `scrape_status`.
- Every `free_offers` row has an allowance, a unit, a reset period, an official
  source URL and a verification time. Rows that cannot meet this are excluded or
  marked incomplete and not published as offers.
- Every paid plan has a usage model and a source link.
- Duplicate offer rows for the same provider, plan, allowance and reset period
  are rejected or merged deterministically.
- `cost_input = 0` never implies a free offer.
- No `env_keys` values, tokens, cookies or private account identifiers are
  stored.

## 5. models.dev ingestion

`models.dev` is the first approved source. It is a public, MIT-licensed,
community-contributed database of model specifications, pricing and
capabilities. It exposes public JSON directly, so the adapter fetches data and
does not parse HTML.

### Endpoint and license

- Primary data: `https://models.dev/catalog.json?type=all`.
- Provider/model fallback: `https://models.dev/api.json?type=all` when the
  combined endpoint does not expose a required field.
- `type=all` includes specialized model types; the default response omits them.
- License: MIT, copyright models.dev. The site's `/about` page must include the
  attribution and license notice.
- No credentials are sent. Requests use a descriptive user agent and a bounded
  timeout.

The adapter treats the source as community-maintained. `confidence` is
`community` for models.dev facts. A fact becomes `verified` only after a
separate official-source check; that check is not part of the first slice.

### Mapping

| models.dev | Catalog |
| --- | --- |
| provider key | `providers.provider_id` |
| provider `name`, `npm`, `api`, `doc`, `env` | `providers.name`, `npm`, `api_url`, `doc_url`, `env_keys` |
| model key | `models.model_id` |
| `canonical_model_id` | `models.canonical_model_id` |
| model `name`, `family`, `description` | matching model columns |
| `release_date`, `last_updated`, `knowledge` | matching date columns |
| `open_weights`, `attachment`, `reasoning`, `tool_call`, `structured_output`, `temperature` | matching boolean columns |
| `limit.context`, `limit.input`, `limit.output` | matching limit columns |
| `cost.*` | matching cost columns |
| `modalities.input`, `modalities.output` | JSON arrays |
| `status` | `models.status` |
| provider/model path | `source_record` |

`source_url` is the endpoint used for the snapshot. The upstream provider
`doc` URL is stored as `providers.doc_url`; it is not a substitute for the
snapshot's source record.

### What models.dev does not provide

models.dev does not provide, as of this spec:

- coding-agent identities or agent compatibility;
- subscription plan names, prices, usage models or included allowances;
- recurring free allowances or reset periods;
- local runtime and hardware requirements.

Therefore:

- the first snapshot may contain zero agents, plans, free offers and local
  runtimes;
- the site must show those gaps explicitly;
- a zero-priced model from models.dev is published as a model fact, not as a
  free offer;
- plans, free offers, agents and local runtimes require separately approved
  sources before they can be published.

## 6. Free-offer policy

A row is a free offer only when all of the following hold:

1. The provider documents a recurring allowance, not a one-time grant.
2. The allowance has a defined unit (requests, tokens, credits with a stated
   conversion, or time) and a reset period.
3. The source is an official provider page or an official public dataset.
4. The source URL and verification time are recorded.
5. The conditions and eligibility restrictions are recorded.

Excluded:

- launch credits and one-time signup grants;
- expiring trials and "free for a limited time" promotions;
- unverified community claims;
- zero token prices without a recurring allowance;
- allowances whose reset period or unit is unknown;
- private or account-specific offers.

Free offers are informational. The UI must not promise that an allowance will be
available, that a provider will honor it, or that a quota is guaranteed.

## 7. Snapshot, provenance and freshness

### Snapshot identity

Each successful run writes:

```
artifacts/providers-catalog-<schemaVersion>-<UTC timestamp>-<sha256 prefix>.duckdb
```

The snapshot contains `catalog_meta.snapshot_id`, the schema version, generation
time, source fetch times, source hashes, known gaps and the freshness deadline.
A SHA-256 checksum is written next to the artifact.

Snapshots are uploaded as GitHub Actions artifacts and are gitignored. The
initial retention is 90 days (the GitHub default). If an older artifact is
needed after expiry, the workflow re-scrapes; it does not reconstruct a snapshot
from memory.

### Success and failure

- A run writes to a temporary file and atomically renames it only after schema
  validation and row-count checks pass.
- A fetch, parse, validation or write failure produces no new artifact and fails
  the workflow. The previous artifact remains untouched.
- If a fact existed in the previous snapshot and is absent from a new successful
  fetch, the new snapshot keeps the last verified value and marks
  `scrape_status = 'missing'` with `last_attempt_at` updated. It does not delete
  the fact.
- If the source is unavailable, the run records `scrape_runs.status = 'failed'`
  and leaves the previous artifact as the last buildable snapshot.
- A failed or stale scrape is visible in the workflow summary and in the site's
  `/about` page when that snapshot is built.

### Freshness

- Freshness is measured from `catalog_meta.generated_at`.
- Older than 7 days: show a warning on every route; keep the last verified
  values; do not block the build.
- Missing snapshot or unsupported schema version: fail the build.
- A source page change or scrape failure creates a review item (at minimum a
  failed workflow with a named source and error). It never creates an invented
  value.

## 8. Build and deployment

### Build

- `CATALOG_DUCKDB_PATH` points at the snapshot.
- The Astro build opens the DuckDB file read-only, validates
  `catalog_meta.schema_version`, and emits static pages plus a JSON search index.
- The build fails on missing data, duplicate primary keys, missing provenance on
  published facts, or an unsupported schema version.
- No runtime database connection, scraper route or server-side API is emitted.

### GitHub Actions

- Workflow: `.github/workflows/providers-catalog.yml`.
- Triggers: `schedule` weekly (`0 6 * * 1` UTC) and `workflow_dispatch`.
- Scrape job: install, run the scraper, validate, upload the versioned DuckDB
  artifact and checksum, write a summary.
- Build/deploy job: download the artifact from the same run, build Astro, deploy
  to Cloudflare Pages.
- The deploy job needs `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID` and the
  Pages project name. These are repository/environment secrets and are not
  committed.
- A scrape failure fails the workflow before deployment. A stale but valid
  snapshot may still deploy with the warning.
- No provider credentials are used by the scraper.

### Copy and public-claim constraints

- No Taskstream content.
- No old `docs-web` paths.
- No internal reference-project names.
- No live `/downloads` or `/roadmap` links unless a future page explicitly
  requires them.
- No "available", "guaranteed" or "free" marker on an unverified plan or offer.
- Provenance and freshness are visible on catalog entries and on `/about`.

## 9. Agent pool advisory contract

The catalog can seed the future pool, but it is never authoritative.

The published advisory contract is version 1 at `/agent-pool-advisory.v1.json`.
Its records are keyed by provider/model/plan/offer and include:

- `provider_id`, `model_id` (null until a plan/model link exists), `plan_id`,
  `offer_id`;
- `allowance`, `allowance_unit`, `reset_period`;
- `usage_model`, `capacity` (`known` or `unknown`), `confidence`, `verified_at`,
  `scrape_status`;
- `snapshot_id`, `schema_version`, `stale` (derived from the 7-day threshold);
- source URL and conditions.

Rules for the pool:

- Local usage and provider rate-limit/quota responses win over catalog values.
- A missing or stale catalog entry marks capacity `unknown`, not `free`.
- The pool must not copy credentials or account data into a catalog record.
- The catalog is read as a snapshot; the pool does not fetch the scraper or a
  runtime catalog API.
- The catalog publishes the contract now; the pool connection remains a
  separate implementation. This spec fixes the fields and semantics so the
  catalog does not have to be redesigned for the pool.

## 10. Verification

The package must expose and pass:

```sh
pnpm --dir apps/providers-catalog-web run typecheck
pnpm --dir apps/providers-catalog-web run test
pnpm --dir apps/providers-catalog-web run build
pnpm --dir apps/providers-catalog-web run test:e2e
```

The root `pnpm typecheck` must include the package. The full catalog slice must
also pass the Shepherd package typecheck, static build and full e2e suite when
those are part of the final gate.

Required coverage:

- schema validation: required columns, primary keys, provenance columns and
  schema version;
- source mapping: provider/model flattening, canonical IDs, null optional fields
  and unknown values;
- duplicate detection: duplicate models, duplicate offers and duplicate
  agent links;
- free-offer policy: one-time credits, expiring trials and zero-cost models are
  not published as recurring offers;
- freshness: a snapshot older than 7 days warns but builds; a missing snapshot
  fails;
- failure behavior: a failed scrape does not overwrite the previous snapshot; a
  missing fact keeps the last verified value and is marked `missing`;
- route coverage: every generated route is reachable and every navigation entry
  resolves;
- filters and comparison: each filter changes the result set; comparison shows
  sources, verification dates, usage models and unknown states;
- public claims and links: the shared public-surface guard plus catalog-specific
  provenance/no-guarantee checks;
- responsive overflow and axe WCAG 2.1 AA checks at mobile, tablet and desktop
  widths.

## 11. Known gaps and open inputs

These do not block the scraper/schema slice, but they block a complete catalog:

- **Agents:** no approved source or initial registry for coding-agent identity
  and compatibility.
- **Plans and pricing:** no approved source for subscription names, prices,
  usage models and included allowances.
- **Recurring free offers:** no approved source; models.dev zero prices are not a
  substitute.
- **Local runtimes/hardware:** no approved source for runtime, quantization and
  approximate hardware.
- **Cloudflare Pages:** project name, account and API token are not configured.
- **Artifact expiry:** if the previous artifact has expired, the merge policy
  cannot retain facts from it; the next successful scrape starts a new baseline.
- **Agent pool integration:** contract only; no implementation in this slice.
- **Public claims helper:** the catalog-specific claim assertions are new; the
  generic link guard should be shared with the existing docs tests when the
  catalog routes are added.

## 12. Definition of done for the first slice

The first slice is complete when:

- the package exists in the pnpm workspace;
- schema version 1 is defined and validated;
- the models.dev adapter fetches and maps providers and models without HTML
  parsing or credentials;
- a local command produces a versioned DuckDB snapshot and checksum;
- validation tests cover mapping, provenance, duplicates, zero-cost-not-free and
  failed-scrape behavior;
- the weekly workflow uploads the artifact and fails cleanly on scrape errors;
- `pnpm typecheck`, package unit tests and snapshot validation pass;
- no public catalog route, plan/offer claim or Cloudflare deployment is required
  yet;
- generated snapshots are gitignored and the working tree is clean after the
  commit.

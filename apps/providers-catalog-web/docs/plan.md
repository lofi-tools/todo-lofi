# Providers catalog — implementation plan

Status: S1 and S3 implemented on 2026-11-09. S2's source-independent merge policy, review summary, free-offer normalization and synthetic fixture gate are implemented; its source adapters remain blocked on approved sources. S4 is blocked on Cloudflare credentials. S5's advisory contract is implemented; connecting it to the Shepherd pool remains a separate Shepherd todo.

Spec: [spec.md](./spec/spec.md). The spec governs behavior and data semantics;
this file gives the working order.

## Working agreements

- Implement one slice per commit. Keep the tree clean after each commit.
- Do not commit DuckDB snapshots, scraped JSON, build output or secrets.
- Do not publish a plan, free offer, agent link or local-hardware claim without a
  source URL, verification time, confidence and scrape status.
- Do not infer a recurring free offer from a zero token price.
- Keep the site static. No public runtime database, scraper endpoint or server
  API.
- Use the existing pnpm workspace, Astro conventions, `web-design-system` and
  Playwright/axe checks.
- The first slice is scraper and schema only. Do not start the public catalog
  routes until the snapshot contract is proven.

## Slice S1 — scraper and DuckDB schema (first)

**Outcome:** a scheduled scrape produces a validated, versioned DuckDB snapshot
of models.dev providers and models, with provenance and failure state. No public
catalog site yet.

### S1.1 Package skeleton

- [x] Add `apps/providers-catalog-web` to `pnpm-workspace.yaml`.
- [x] Create `apps/providers-catalog-web/package.json` with:
  - `type: module`;
  - scripts `scrape`, `snapshot`, `validate`, `test`, `typecheck`;
  - `@duckdb/node-api`, a TypeScript runner (`tsx`), TypeScript and Vitest;
  - no Astro dependency yet.
- [x] Add `tsconfig.json` and a minimal `README.md` pointing to the spec and
  plan.
- [x] Add package scripts to the root `package.json`:
  - `catalog:snapshot`;
  - `catalog:validate`;
  - include the package in `typecheck`.
- [x] Ignore `artifacts/`, `*.duckdb`, `*.duckdb.wal` and downloaded snapshots
  in the package or root ignore file.

### S1.2 Schema v1

- [x] Create `schema/0001_catalog.sql` with the tables from spec §4:
  `sources`, `scrape_runs`, `catalog_meta`, `agents`, `providers`, `models`,
  `plans`, `free_offers`, `agent_provider_links`, `agent_plan_links`,
  `agent_model_links`, `local_runtimes` and `fact_sources`.
- [x] Add primary keys and unique keys. Orphan links are rejected by validation
  because the snapshot writer does not rely on foreign-key enforcement.
- [x] Add `schema_version` to `catalog_meta` and a code constant that must match
  the SQL.
- [x] Add a validation query set for:
  - required provenance columns;
  - duplicate provider/model IDs;
  - duplicate agent links;
  - orphaned plan/offer/model links;
  - free offers missing allowance, unit, reset period, source or verification;
  - plan rows missing usage model or source.
- [x] Add a small fixture snapshot builder for tests; do not commit a production
  snapshot.

### S1.3 models.dev adapter

- [x] Add a source registry with the models.dev entry: endpoint, license,
  attribution and confidence policy.
- [x] Fetch `https://models.dev/catalog.json?type=all` with a bounded timeout and
  descriptive user agent; fall back to `api.json?type=all` when the combined endpoint
  fails or cannot be parsed.
- [x] Validate the response shape with explicit TypeScript guards. Do not trust
  optional fields.
- [x] Flatten providers and nested models into typed records.
- [x] Map all fields listed in spec §5, preserving nulls for unknown optional
  values.
- [x] Keep `env_keys` as names only. Never read or store environment values.
- [x] Set `source_url`, `source_record`, `verified_at`, `confidence =
  'community'` and `scrape_status = 'ok'` for observed facts.
- [x] Do not create plans, agents, free offers or local runtimes from models.dev
  data.
- [x] Add unit tests for mapping, optional fields, canonical model IDs and a
  provider with no models.

### S1.4 Snapshot writer

- [x] Create the DuckDB file at a temporary path, apply schema v1 and insert the
  source run and mapped rows inside one transaction.
- [x] Write `catalog_meta` with snapshot ID, schema version, generated time,
  source fetch time, content hash, license and known gaps.
- [x] Atomically rename the temporary file to
  `artifacts/providers-catalog-<schemaVersion>-<timestamp>-<hash>.duckdb`.
- [x] Write a SHA-256 checksum file beside the snapshot.
- [x] On fetch, parse, validation or write failure, remove the temporary file,
  record the failure, and leave any previous artifact untouched.
- [x] Add a test that a failed scrape does not replace a good snapshot.
- [x] Add a test that a zero-cost model is stored as a model, not a free offer.
- [x] Add a test that an old snapshot is reported stale at 7 days but remains
  buildable.

### S1.5 Validation command

- [x] Add `validate` that opens an existing snapshot read-only and runs the
  schema/provenance/duplicate checks.
- [x] Fail with a non-zero exit code and a named check on any violation.
- [x] Print a compact summary: snapshot ID, schema version, generated time,
  provider count, model count, agent/plan/offer/local-runtime counts and known
  gaps.
- [x] Add fixture tests for duplicate models, duplicate recurring offers, stale
  data and failed snapshots.

### S1.6 Weekly workflow

- [x] Add `.github/workflows/providers-catalog.yml` with `schedule` weekly at
  `0 6 * * 1` UTC and `workflow_dispatch`.
- [x] Install dependencies, run `catalog:snapshot`, run `catalog:validate`, and
  upload the DuckDB file plus checksum as an artifact.
- [x] Name the uploaded artifact from the versioned snapshot filename; retain for
  90 days.
- [x] Write a job summary naming the source, snapshot ID, row counts and errors.
  Do not deploy in this slice.
- [x] Ensure a failed scrape fails the job before any artifact upload.

### S1 gate

Run from the repository root:

```sh
pnpm --dir apps/providers-catalog-web run typecheck
pnpm --dir apps/providers-catalog-web run test
pnpm --dir apps/providers-catalog-web run snapshot
pnpm --dir apps/providers-catalog-web run validate
pnpm typecheck
```

Pass when the snapshot is created, validation passes, unit tests pass, the root
typecheck passes, and the workflow file is syntactically valid. Record the
snapshot path and row counts as evidence. Commit S1 alone with a terse message,
for example `docs: add providers catalog snapshot`.

S1 evidence (2026-11-09): `pnpm --dir apps/providers-catalog-web run typecheck`
passed; 9 Vitest tests passed; a real models.dev scrape produced
`providers-catalog-1-20261109T125001Z-c63f35a4.duckdb` with 235 providers and
9,000 models; `validate` passed read-only with zero errors; the SHA-256 checksum
verified; root `pnpm typecheck` passed. The artifact is gitignored and the
workflow YAML parsed successfully. The next slice remains blocked on approved
agent, plan and recurring-free-offer sources.

## Slice S2 — plans, agents and recurring offers

**Outcome:** approved sources populate the remaining tables without weakening
provenance or inventing offers.

- [ ] Record the approved source for agent identity/compatibility.
- [ ] Record the approved source for plan names, prices, usage models and
  included allowances.
- [ ] Record the approved source for recurring free allowances and reset
  periods.
- [ ] Record the approved source for local runtime and approximate hardware.
- [ ] Add adapters with fixture tests; no source is enabled without a license and
  attribution entry.
- [x] Implement the merge policy that retains the last verified provider or
  model when a new fetch omits it, marks `scrape_status = 'missing'`, and
  updates only `last_attempt_at`. Plan/offer/agent/runtime retention is deferred
  until their adapters exist.
- [x] Implement duplicate-offer merging and the one-time/expiring-trial exclusion
  in normalization and validation, not just in the adapter. Conflicting
  duplicates fail closed; a merged snapshot reports the dropped ids.
- [x] Add a review summary when a provider or model source record disappears;
  the snapshot report lists retained records and their review items.
- [x] Gate: snapshot validation, unit tests and a fixture snapshot with at least
  one agent, plan, recurring offer and local runtime. The fixture is synthetic;
  it does not enable an unapproved source.

S2 evidence (2026-11-09): a fixture test creates a second snapshot from a
catalog that omits the first provider/model and passes the first snapshot as the
previous artifact. The new snapshot retains both rows, preserves their
`verified_at`, sets `last_attempt_at` to the new fetch, marks them `missing`, and
reports `provider acme` and `model acme/free-model` as review items. A second
fixture writes two identical recurring offers, receives one row plus
`mergedOfferIds: ['offer-2']`, and validation rejects a direct insert whose
conditions say `Free for 30 days; limited-time trial.` A third fixture writes one
agent, one plan, one recurring offer linked to that plan, and one local runtime;
validation passes with all four counts at one. Source adapters remain blocked on
approved sources.

## Slice S3 — static catalog site

**Outcome:** the Astro site reads the snapshot and exposes browse, filters,
comparison and provenance.

- [x] Add Astro, the `web-design-system` dependency and the Panda codegen setup
  matching `apps/shep-docs-web`.
- [x] Add the build-time DuckDB reader; validate the singleton metadata row,
  supported schema version and required provenance before rendering.
- [x] Add routes `/`, `/agents/[agent]`, `/providers/[provider]`,
  `/models/[provider]/[model]`, `/compare` and `/about`. The model route is a
  catch-all because some source model ids contain `/`.
- [x] Add the static model data and client-side filters for offer status,
  provider, model, context, modality, local/self-hosted and last verified. Agent
  and usage-model controls stay disabled and explicit until an approved source
  exists; the offer filter never invents a free/paid value.
- [x] Add comparison with source links, verification times, confidence, model
  facts and explicit unknown states. Plan usage models, allowances and reset
  periods remain visibly unpublished until S2.
- [x] Add the freshness banner at 7 days and explicit empty states for missing
  plans, offers, agents and local runtimes.
- [x] Extract the generic public link guard into `libs/web-test-guards`; keep
  the Shepherd roadmap claim data in the existing app.
- [x] Add catalog-specific claim checks: no unverified "free"/"available"
  marker, no guarantee language, no Taskstream or internal reference names, no
  live `/downloads` or `/roadmap` links, and an explicit plan/offer gap on every
  key page.
- [x] Add route, navigation, filter, comparison, responsive overflow and axe
  WCAG 2.1 AA tests at mobile, tablet and desktop widths.
- [x] Gate: package typecheck, unit tests, static build, package e2e, existing
  docs e2e, root typecheck. Commit S3 alone.

S3 evidence (2026-11-09): package typecheck passed; 9 Vitest tests passed; the
static build emitted 9,240 pages from the 235-provider/9,000-model snapshot;
the 8 catalog Playwright tests passed, including filters, URL comparison,
legacy-link/claim checks, overflow at 375/768/1440 and axe at those widths;
the existing Shepherd docs e2e and root `pnpm typecheck` passed. Cloudflare
Pages deployment remains S4 and was not attempted.

## Slice S4 — Cloudflare Pages deployment

**Outcome:** the weekly workflow builds and publishes the static catalog.

- [ ] Add the Cloudflare Pages project configuration, build command and output
  directory.
- [ ] Add the deploy job after a successful scrape/validate step. Download the
  artifact from the same workflow run.
- [ ] Use repository/environment secrets for `CLOUDFLARE_API_TOKEN`,
  `CLOUDFLARE_ACCOUNT_ID` and the Pages project name. Do not commit them.
- [ ] Deploy only when the snapshot validates. A stale but valid snapshot may
  deploy with the warning; a failed scrape must not.
- [ ] Add a manual dispatch path that can rebuild from a retained artifact, and a
  documented recovery path when the artifact has expired.
- [ ] Verify the Pages URL, generated pages, search assets, content types and
  the freshness/provenance pages. No custom domain.
- [ ] Gate: package typecheck, static build, package e2e, full Shepherd e2e,
  root typecheck, a successful deployed-preview check and a clean tree. Commit
  S4 alone.

## Slice S5 — agent pool advisory integration

**Outcome:** the future pool can consume the advisory snapshot without making
the catalog authoritative.

- [x] Publish the advisory record shape from spec §9 as versioned JSON at
  `/agent-pool-advisory.v1.json`.
- [x] Ensure missing or stale entries map to `capacity: 'unknown'`, never
  `known` or a free-capacity claim.
- [x] Add a fixture test showing local usage/provider responses overriding a
  catalog allowance.
- [x] Do not add credentials or private account data to the catalog snapshot or
  the advisory records.
- [x] Gate: contract test plus the full catalog checks. Pool wiring remains a
  separate Shepherd todo.

S5 evidence (2026-11-09): the static endpoint publishes contract version 1 with
one record per recurring offer and the plan's usage model. Unit tests cover a
known record, stale/missing/unknown-plan records mapping to `capacity:
'unknown'`, and a local observation overriding a stale catalog allowance. The
contract test checks the built public route; the Shepherd pool is not modified.

## Risks and follow-ups

- **models.dev shape changes:** the adapter must fail closed and preserve the
  previous artifact; fixtures should cover optional-field changes.
- **No plan/free-offer source yet:** S1 and S3 must show explicit gaps; do not
  fill them from zero prices or community posts.
- **DuckDB native dependency:** verify the `@duckdb/node-api` prebuilt binary on
  the GitHub Actions runner before relying on the weekly schedule.
- **Artifact expiry:** retained 90 days; an expired merge source means a new
  baseline. Document this in `/about`.
- **Cloudflare credentials:** S4 is blocked until the project and secrets are
  configured; S1–S3 do not depend on them.
- **Shared claim helper:** extracting the link guard is a small refactor of the
  existing docs tests; keep their current claim data and behavior unchanged.

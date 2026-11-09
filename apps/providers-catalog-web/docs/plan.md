# Providers catalog — implementation plan

Status: planned. The specification was recorded on 2026-11-09.

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

- [ ] Add `apps/providers-catalog-web` to `pnpm-workspace.yaml`.
- [ ] Create `apps/providers-catalog-web/package.json` with:
  - `type: module`;
  - scripts `scrape`, `snapshot`, `validate`, `test`, `typecheck`;
  - `@duckdb/node-api`, a TypeScript runner (`tsx`), TypeScript and Vitest;
  - no Astro dependency yet.
- [ ] Add `tsconfig.json` and a minimal `README.md` pointing to the spec and
  plan.
- [ ] Add package scripts to the root `package.json`:
  - `catalog:snapshot`;
  - `catalog:validate`;
  - include the package in `typecheck`.
- [ ] Ignore `artifacts/`, `*.duckdb`, `*.duckdb.wal` and downloaded snapshots
  in the package or root ignore file.

### S1.2 Schema v1

- [ ] Create `schema/0001_catalog.sql` with the tables from spec §4:
  `sources`, `scrape_runs`, `catalog_meta`, `agents`, `providers`, `models`,
  `plans`, `free_offers`, `agent_provider_links`, `agent_plan_links`,
  `agent_model_links`, `local_runtimes` and `fact_sources`.
- [ ] Add primary keys, unique constraints and foreign keys where DuckDB
  supports them.
- [ ] Add `schema_version` to `catalog_meta` and a code constant that must match
  the SQL.
- [ ] Add a validation query set for:
  - required provenance columns;
  - duplicate provider/model IDs;
  - duplicate agent links;
  - orphaned plan/offer/model links;
  - free offers missing allowance, unit, reset period, source or verification;
  - plan rows missing usage model or source.
- [ ] Add a small fixture snapshot builder for tests; do not commit a production
  snapshot.

### S1.3 models.dev adapter

- [ ] Add a source registry with the models.dev entry: endpoint, license,
  attribution and confidence policy.
- [ ] Fetch `https://models.dev/catalog.json?type=all` with a bounded timeout and
  descriptive user agent; fall back to `api.json?type=all` only when a required
  field is unavailable.
- [ ] Validate the response shape with explicit TypeScript guards. Do not trust
  optional fields.
- [ ] Flatten providers and nested models into typed records.
- [ ] Map all fields listed in spec §5, preserving nulls for unknown optional
  values.
- [ ] Keep `env_keys` as names only. Never read or store environment values.
- [ ] Set `source_url`, `source_record`, `verified_at`, `confidence =
  'community'` and `scrape_status = 'ok'` for observed facts.
- [ ] Do not create plans, agents, free offers or local runtimes from models.dev
  data.
- [ ] Add unit tests for mapping, optional fields, canonical model IDs and a
  provider with no models.

### S1.4 Snapshot writer

- [ ] Create the DuckDB file at a temporary path, apply schema v1 and insert the
  source run and mapped rows inside one transaction.
- [ ] Write `catalog_meta` with snapshot ID, schema version, generated time,
  source fetch time, content hash, license and known gaps.
- [ ] Atomically rename the temporary file to
  `artifacts/providers-catalog-<schemaVersion>-<timestamp>-<hash>.duckdb`.
- [ ] Write a SHA-256 checksum file beside the snapshot.
- [ ] On fetch, parse, validation or write failure, remove the temporary file,
  record the failure, and leave any previous artifact untouched.
- [ ] Add a test that a failed scrape does not replace a good snapshot.
- [ ] Add a test that a zero-cost model is stored as a model, not a free offer.
- [ ] Add a test that an old snapshot is reported stale at 7 days but remains
  buildable.

### S1.5 Validation command

- [ ] Add `validate` that opens an existing snapshot read-only and runs the
  schema/provenance/duplicate checks.
- [ ] Fail with a non-zero exit code and a named check on any violation.
- [ ] Print a compact summary: snapshot ID, schema version, generated time,
  provider count, model count, agent/plan/offer/local-runtime counts and known
  gaps.
- [ ] Add fixture tests for each failing check.

### S1.6 Weekly workflow

- [ ] Add `.github/workflows/providers-catalog.yml` with `schedule` weekly at
  `0 6 * * 1` UTC and `workflow_dispatch`.
- [ ] Install dependencies, run `catalog:snapshot`, run `catalog:validate`, and
  upload the DuckDB file plus checksum as an artifact.
- [ ] Name the artifact with the schema version and timestamp; retain for 90
  days.
- [ ] Write a job summary naming the source, snapshot ID, row counts and any
  failure. Do not deploy in this slice.
- [ ] Ensure a failed scrape fails the job before any artifact upload.

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
- [ ] Implement the merge policy that retains the last verified fact when a new
  fetch omits it, marks `scrape_status = 'missing'`, and updates only
  `last_attempt_at`.
- [ ] Implement duplicate-offer merging and the one-time/expiring-trial exclusion
  in validation, not just in the adapter.
- [ ] Add a review summary when a provider page or source record disappears.
- [ ] Gate: snapshot validation, unit tests and a fixture snapshot with at least
  one agent, plan, recurring offer and local runtime.

## Slice S3 — static catalog site

**Outcome:** the Astro site reads the snapshot and exposes browse, filters,
comparison and provenance.

- [ ] Add Astro, the `web-design-system` dependency and the Panda codegen setup
  matching `apps/shep-docs-web`.
- [ ] Add the build-time DuckDB reader; validate schema version and required
  provenance before rendering.
- [ ] Add routes `/`, `/agents/[agent]`, `/providers/[provider]`,
  `/models/[provider]/[model]`, `/compare` and `/about`.
- [ ] Add the build-generated search index and client-side filters for free/paid,
  agent, provider, model, context, modality, local/self-hosted, last verified
  and usage model.
- [ ] Add comparison with source links, verification times, confidence, usage
  models, allowance/reset period and unknown/stale states.
- [ ] Add the freshness banner at 7 days and explicit empty states for missing
  plans, offers and local runtimes.
- [ ] Extract the generic public link guard from
  `apps/shep-docs-web/tests/publicClaims.ts` into a shared test helper; keep the
  Shepherd roadmap claim data in the existing app.
- [ ] Add catalog-specific claim checks: no guarantee language, no unverified
  "free"/"available" marker, every offer/plan has a source and verification
  time, no Taskstream or internal reference names, no live `/downloads` or
  `/roadmap` links.
- [ ] Add route, navigation, filter, comparison, responsive overflow and axe
  WCAG 2.1 AA tests at mobile, tablet and desktop widths.
- [ ] Gate: package typecheck, unit tests, static build, package e2e, existing
  docs e2e, root typecheck. Commit S3 alone.

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

- [ ] Publish the advisory record shape from spec §9 as a versioned JSON or
  DuckDB view.
- [ ] Ensure missing or stale entries map to `unknown`, never `free`.
- [ ] Add a fixture test showing local usage/provider responses overriding a
  catalog allowance.
- [ ] Do not add credentials or private account data to the catalog snapshot.
- [ ] Gate: contract test plus the full catalog checks. This slice can wait until
  the pool implementation exists.

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

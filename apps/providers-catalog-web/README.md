# providers-catalog-web

The separate static catalog of coding-agent providers, models, plans and
recurring free offers. It is not part of the Shepherd desktop app and does not
become the agent pool's authority.

The product and data contracts are in
[`docs/spec/spec.md`](./docs/spec/spec.md); the implementation order is in
[`docs/plan.md`](./docs/plan.md).

## Snapshot

The first source is the public models.dev catalog. A snapshot stores providers,
models and provenance in DuckDB; plans, agents, recurring offers and local
runtimes stay empty until an approved source exists.

```sh
pnpm --dir apps/providers-catalog-web run snapshot
pnpm --dir apps/providers-catalog-web run validate
pnpm --dir apps/providers-catalog-web run test
```

Snapshots are gitignored. A failed scrape never replaces the previous artifact.
When the newest artifact is present in `artifacts/`, the writer retains any
provider or model omitted by the new response, preserves its `verified_at`, marks
it `missing`, and includes it in the snapshot report's review items. Recurring
offer rows are normalized before writing: exact duplicates merge deterministically,
conflicting duplicates fail, and one-time or expiring-trial wording is rejected.
Unit tests also write a synthetic agent, plan, recurring offer and local runtime;
that fixture does not enable an unapproved source.

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

Routes: `/`, `/models`, `/providers/[provider]`,
`/models/[provider]/[...model]`, `/compare`, `/about` and `/agents`.
Providers and models are statically generated; the browser only filters and
compares the embedded public data. There is no runtime database, scraper
endpoint or account data.

The plan and free-offer tables are empty in the current source. The UI says so
explicitly and treats zero token prices as model prices, never as free offers.

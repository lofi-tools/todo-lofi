# providers-catalog-web

The separate static catalog of coding-agent providers, models, plans and
recurring free offers. It is not part of the Shepherd desktop app and does not
become the agent pool's authority.

The product and data contracts are in
[`docs/spec/spec.md`](./docs/spec/spec.md); the implementation order is in
[`docs/plan.md`](./docs/plan.md).

## First slice

The first slice is the scraper and DuckDB schema only. It fetches the public
models.dev catalog, maps providers and models, and writes a versioned snapshot
under `artifacts/` for GitHub Actions to retain.

```sh
pnpm --dir apps/providers-catalog-web run snapshot
pnpm --dir apps/providers-catalog-web run validate
pnpm --dir apps/providers-catalog-web run test
pnpm --dir apps/providers-catalog-web run typecheck
```

Snapshots are gitignored. A failed scrape never replaces the previous artifact.

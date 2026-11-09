# shep-docs-web

Shepherd's documentation site, built with the repository's own design system
(`libs/web-design-system`) and the same Panda preset, tokens and marketing
components as the rest of the web packages.

This package ships `/`: the individual-developer promise, the local workspace,
preview annotation, and an explicit available/next/planned split. It also ships
two audience guides: `/docs/users` for installation, configuration and the
workspace, and `/docs/contributors` for adapter, annotation, quota-ledger,
release and fixture internals. It does not contain Taskstream documentation.

## Structure

- `src/pages/index.astro` — the landing page.
- `src/pages/docs/users/` and `src/pages/docs/contributors/` — the two guides.
- `src/data/docs.ts` — the one navigation source for tabs, sidebars, footer and pagers.
- `data/roadmap.ts` — the one maintained roadmap and landing availability source.
- `src/layouts/Docs.astro`, `src/components/blocks/DocsHeader.astro` and
  `DocsFooter.astro` — the docs shell.
- `data/shepherd.ts` — landing copy and illustrative data.
- `src/components/illustrations/` — the workspace, agent-pool and preview
  replicas, plus the shared illustration stage.
- `tests/landing.spec.ts` and `tests/docs.spec.ts` — landing and docs route,
  navigation, honest-claims, layout, theme and axe checks.

## Running

From the repository root:

```sh
pnpm install
pnpm dev:shep-docs   # Astro dev on http://localhost:4323
pnpm build:shep-docs # codegen + static build
pnpm preview:shep-docs
pnpm test:e2e:shep-docs
```

`pnpm dev` starts both this package (port 4323) and the Taskstream docs package
(port 4321). The committed `test:e2e` script stops a preview left by an earlier
run, builds, serves on port 4324 with two workers, and stops the preview again
when the run ends. It can run alongside both dev servers.

From this directory:

```sh
pnpm codegen    # panda codegen + panda cssgen
pnpm dev
pnpm build
pnpm typecheck  # astro check
pnpm test:e2e   # Playwright landing/docs routes, theming, layout and axe AA checks
```

## Notes

- Styling comes from `panda cssgen`, not PostCSS. `styled-system/styles.css` is
  generated and gitignored.
- `panda.config.ts` must keep `prefix`, `hash`, and `separator` identical to
  `libs/web-design-system/panda.config.ts` and the showcase's config, and its
  `include` globs must cover the design system's source.
- Landing data lives in `data/`, outside `src/`, because Panda scans every file
  under `src/` for style-shaped objects.
- The design system's reset/base stylesheet is imported after the generated
  stylesheet so their cascade layers line up.

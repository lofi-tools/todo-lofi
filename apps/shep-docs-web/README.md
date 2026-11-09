# shep-docs-web

Shepherd's documentation site, built with the repository's own design system
(`libs/web-design-system`) and the same Panda preset, tokens and marketing
components as the rest of the web packages.

This package currently ships `/`: the individual-developer promise, the local
workspace, preview annotation, and an explicit available/next/planned split.
The user and contributor guides are later roadmap items; this package does not
contain Taskstream documentation.

## Structure

- `src/pages/index.astro` — the landing page.
- `data/shepherd.ts` — landing copy, availability groups and illustrative data.
- `src/components/illustrations/` — the workspace, agent-pool and preview
  replicas, plus the shared illustration stage.
- `src/components/blocks/SiteHeader.astro` and `DocsFooter.astro` — the
  Shepherd-only site shell.
- `tests/landing.spec.ts` — landing, navigation, theme, layout and axe checks.

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
(port 4321). The Playwright suite builds and previews on port 4324, so it can
run alongside both dev servers.

From this directory:

```sh
pnpm codegen    # panda codegen + panda cssgen
pnpm dev
pnpm build
pnpm typecheck  # astro check
pnpm test:e2e   # Playwright landing, theming, layout and axe AA checks
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

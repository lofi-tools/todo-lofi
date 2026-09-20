# design-system-showcase

An Astro site that documents the `web-design-system` package and rebuilds
Linear-like page shapes. Every word, name, logo, and metric on the site is
placeholder content.

Spec: `docs/spec/web-design-system-spec.md`.

## Running

From the repository root:

```sh
pnpm install
pnpm dev:ds     # astro dev, regenerates Panda CSS first
pnpm build      # codegen for the design system + static build
pnpm preview    # serve the built site
```

From this directory:

```sh
pnpm codegen    # panda codegen + panda cssgen
pnpm dev
pnpm build
pnpm typecheck  # astro check
pnpm test:e2e   # Playwright smoke, theming, and axe AA checks
```

The e2e suite expects the locally installed Chrome and starts its own preview
server (`pnpm run build && pnpm run preview`).

## Routes

| Route | Content |
| --- | --- |
| `/` | Overview: layers and a composed example |
| `/foundations` | Tokens: palette, semantic roles, type, spacing, radii, shadows, motion |
| `/layout` | Box, Stack, Grid, Container, Separator |
| `/primitives` | Buttons, badges, avatars, inputs, feedback, and the interactive Ark UI primitives |
| `/marketing` | Every marketing section in isolation |
| `/product-ui` | Issue rows/board, timeline, activity, chat, code, diff |
| `/replicas/home` | Homepage replica |
| `/replicas/pricing` | Pricing tiers + comparison table |
| `/replicas/features` | Feature sections |
| `/replicas/customers` | Customer stories + testimonials |
| `/replicas/changelog` | Changelog entries |
| `/replicas/docs` | Docs chrome: sidebar, table of contents, prose, code |

## Notes

- Styling comes from `panda cssgen`, not PostCSS: Astro's CSS pipeline does not
  run `postcss.config` here. `styled-system/styles.css` is generated and
  gitignored.
- The design system's reset/base styles are imported after the generated
  stylesheet so their cascade layers line up (`DESIGN.md` §2).

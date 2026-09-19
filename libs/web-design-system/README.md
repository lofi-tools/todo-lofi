# web-design-system

A Linear-style web design system: Panda CSS tokens and recipes, an Ark UI
headless layer, Solid components, and `.astro` wrappers.

Part of the pnpm workspace at the repository root. See `DESIGN.md` for tokens,
layers, and conventions, and `docs/spec/web-design-system-spec.md` for the
spec it implements.

## Entry points

| Import | Provides |
| --- | --- |
| `web-design-system` | preset, tokens, styled components, `cn` |
| `web-design-system/preset` | the shared Panda preset (`tokens`, recipes, text styles) |
| `web-design-system/components` | styled components, layout primitives, product types |
| `web-design-system/headless` | unstyled Ark UI primitives |
| `web-design-system/icons` | placeholder icon set |
| `web-design-system/styles.css` | reset, base styles, self-hosted Inter Variable |
| `web-design-system/astro/*` | `.astro` wrappers (e.g. `astro/marketing/Hero.astro`) |

## Usage

```ts
// panda.config.ts in a consuming app
import { defineConfig } from '@pandacss/dev'
import pandaPreset from '@pandacss/preset-panda'
import { preset } from 'web-design-system/preset'

export default defineConfig({
  presets: [pandaPreset, preset],
  include: [
    './src/**/*.{ts,tsx,astro}',
    // Required: your codegen must see the design system's source.
    '../path/to/libs/web-design-system/src/**/*.{ts,tsx,astro}',
  ],
  outdir: 'styled-system',
  jsxFramework: 'solid',
  // Keep these identical to the design system's config (see DESIGN.md §3).
  hash: false,
  separator: '_',
})
```

Then import the stylesheet once and the components:

```astro
---
import 'styled-system/styles.css'
import 'web-design-system/styles.css'
import { Button, Badge } from 'web-design-system/components'
import Hero from 'web-design-system/astro/marketing/Hero.astro'
---
<Hero title="Hello" subtitle="Placeholder copy." />
<Button variant="primary">Get started</Button>
<Badge tone="accent">Preview</Badge>
```

## Development

```sh
pnpm --dir libs/web-design-system codegen   # regenerate styled-system
pnpm --dir libs/web-design-system test      # Vitest
pnpm --dir libs/web-design-system typecheck
```

`styled-system/` is generated and gitignored.

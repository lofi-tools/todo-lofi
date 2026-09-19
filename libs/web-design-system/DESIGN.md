# Web Design System — DESIGN.md

A Linear-style design system built on **Astro + SolidJS + Panda CSS + Ark UI**.
Spec: `docs/spec/web-design-system-spec.md`.

All content in this package and the showcase is placeholder material. No
Linear wordmark, logo, screenshot, or copy is reproduced.

---

## 1. Layers

```
tokens ─► Panda preset ─┬─► headless (Ark UI Solid, unstyled behavior + ARIA)
                        ├─► styled   (Panda recipes/patterns + Solid)
                        └─► astro    (.astro wrappers that hydrate Solid)
```

| Layer | Location | Responsibility |
| --- | --- | --- |
| Tokens | `src/tokens/` | Raw palette, semantic colors, type, spacing, sizes, radii, shadows, motion, breakpoints |
| Preset | `src/preset.ts` | The shared Panda preset exporting tokens, semantic tokens, text styles, keyframes, and recipes |
| Headless | `src/components/headless/` | Ark UI Solid primitives re-exported behind a stable module boundary |
| Styled | `src/components/styled/` | Panda recipes/patterns over the headless primitives, plus `.astro` wrappers |
| Icons | `src/icons/` | Placeholder stroke icon set behind one `IconProps` API |

Components must consume **semantic tokens** (`accent.text`, `surface.subtle`,
`fg.muted`, …) rather than raw palette values, so a theme is a token swap.

---

## 2. Theming

- **Dark-first.** The base value of every semantic token is the dark theme.
  `[data-theme="light"]` (the Panda condition `_light`) overrides each role.
- `ThemeToggle` flips `document.documentElement.dataset.theme` and persists to
  `localStorage['wds-theme']`. The host page must set the attribute before
  first paint (the showcase layout's inline script does this) to avoid a flash.
- `src/styles/` holds the reset and base element styles. Both are wrapped in
  the `reset` / `base` **cascade layers** so Panda's `utilities` layer always
  wins — an unlayered reset would silently disable every padding utility.

---

## 3. Class parity (most important rule)

The preset is consumed by two projects that each run Panda codegen: this
package and the showcase. Both `panda.config.ts` files must keep these settings
identical or components render unstyled with no error:

- `prefix`, `hash`, `separator`
- and the showcase's `include` globs must cover this package's `src/**`.

Both currently use `hash: false`, `separator: '_'`, no prefix. Readable class
and variable names are intentional: `src/styles/base.css` references generated
variables like `--colors-canvas` directly.

The showcase generates CSS with `panda cssgen`, because Astro's CSS pipeline
does not run `postcss.config` for it.

---

## 4. Component conventions

- One component per file; PascalCase file names matching the export.
- Props extend the underlying DOM props (`JSX.ButtonHTMLAttributes<…>`) and
  split them with `splitProps` so rest-props pass through.
- Interactive primitives are Solid components in `.tsx` with a sibling
  `.astro` wrapper. States are styled with Ark UI's `data-*` attributes
  (`&[data-state="open"]`, `&[data-selected]`).
- **Hydration is opt-in:** Ark-based wrappers use `client:only="solid"`
  (Ark portals do not SSR cleanly); simple stateful wrappers use
  `client:visible`; marketing and mock-product components are pure `.astro`
  with no client runtime.
- `.astro` wrappers that are `client:only` cannot receive slot content, so any
  body text is passed as a prop (`body`, `description`) instead of a slot.

### Adding a component

1. Define tokens/recipes in `src/tokens/` and `src/recipes/`, add them to
   `src/preset.ts`.
2. Build the Solid component in `src/components/styled/`.
3. Add a `.astro` wrapper beside it, choosing a hydration directive.
4. Export it from `src/components/styled/index.ts`.
5. Add a `Demo` entry to the showcase gallery page.

---

## 5. Accessibility

- Target **WCAG 2.1 AA**. Text roles (`fg.default`, `fg.muted`, `fg.subtle`)
  and the `badge.*` / `accent.text` / `danger.fill` roles are tuned to clear
  4.5:1 in both themes; `tests/tokens.test.ts` asserts this.
- Focus is always visible (`:focus-visible` + the `shadows.focus` ring).
- Icon-only controls require `aria-label` (see `IconButton`).
- Horizontally scrollable regions (code blocks, diff panes, the plan table,
  the file-tab strip) are keyboard-focusable (`tabindex="0"`).
- Decorative mock visuals are `aria-hidden`; meaningful icons use `role="img"`
  with a label.
- Motion is CSS-only and collapses under `prefers-reduced-motion: reduce`.

---

## 6. Testing

| Suite | Command | Covers |
| --- | --- | --- |
| Unit (Vitest) | `pnpm --dir libs/web-design-system test` | `cn`, token integrity, WCAG contrast math, Button rendering |
| E2E + a11y (Playwright) | `pnpm --dir demos/design-system-showcase test:e2e` | Every showcase route loads with no console errors; the token stylesheet applies; the theme toggle persists; axe finds no serious/critical WCAG 2.1 AA violations |

Playwright uses the locally installed Chrome (`channel: 'chrome'`), so no
browser download is required.

---

## 7. Known gaps

Deliberately not in v1: `Select`, `Combobox`, `RadioGroup`, `Slider`,
`SegmentGroup`, `Carousel`, `Progress`, `Toast`, `Pagination`, `Breadcrumb`,
`CommandMenu`, `HoverCard`, and the pricing FAQ accordion. The headless barrel
already re-exports the Ark UI primitives for them, so each is a styling pass
plus an `.astro` wrapper.

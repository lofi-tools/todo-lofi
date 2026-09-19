# Web Design System (Linear-style) + Astro Showcase — Spec

Status: Implemented v1 (see §14 for what shipped and the deliberate gaps).
Every decision in §2 was answered explicitly during the interview; §11 lists
the residual mechanical questions, and §14 records which of them were
resolved during implementation.

Purpose: Add `libs/web-design-system`, a reusable, Linear-styled web design
system built on **Astro + SolidJS islands + Panda CSS + Ark UI**, and
`demos/design-system-showcase`, an Astro site that documents every component
and rebuilds the mined Linear page shapes.

---

## 1. Intent

Two deliverables, specified together because the second consumes the first:

- **Design system** (`libs/web-design-system`): a layered component library —
  design tokens → Panda CSS preset → headless primitives (Ark UI) → styled
  Solid components → Astro wrappers — that visually reproduces Linear's
  marketing and product surfaces while shipping only placeholder content and
  our own identity.
- **Showcase** (`demos/design-system-showcase`): an Astro app with a component
  gallery (all components, grouped) plus full-page replicas of the mined Linear
  page families.

Why now: the repo has a marketing-grade visual language nowhere in code (the
desktop app's palette lives in `apps/todo-2/src/theme.rs` and is deliberately
**not** coupled to this — §2 round 5), and the Linear clone is a useful,
self-contained exercise in a modern Astro/Panda/Solid stack.

**Fidelity is visual, not brand.** We reproduce Linear's layout, spacing, type
scale, color system, radius, shadows, and motion; we do **not** reproduce their
wordmark, logo, screenshots, or copy. All content is placeholder (§4.5).

---

## 2. Interview decisions (normative)

### 2.1 Round 1 — foundations

| # | Question | Decision |
|---|----------|----------|
| 1 | Package location/name | **`libs/web-design-system`** |
| 2 | Distribution | **Workspace package, source consumed.** Nothing is published; the showcase imports the package by workspace path. |
| 3 | Component technology | **Astro + SolidJS islands + Panda CSS** |
| 4 | Styling | **Panda CSS + a headless layer + a separate styled layer** (recipes/patterns over the headless primitives). |

### 2.2 Round 2 — stack details

| # | Question | Decision |
|---|----------|----------|
| 5 | npm package name | **`web-design-system`** (unscoped) |
| 6 | Headless base | **Ark UI** (Solid adapter) |
| 7 | Panda sharing | **Export a shared preset** — the DS exports a Panda preset (tokens, semantic tokens, recipes, patterns); the showcase's `panda.config` extends it. One source of truth. |
| 8 | Toolchain | **pnpm in the Nix devShell** — add Node + pnpm to `flake.nix`, pin a workspace root manifest. |

### 2.3 Round 3 — fidelity, scope, inventory

| # | Question | Decision |
|---|----------|----------|
| 9 | Branding fidelity | **Visual clone, placeholder content.** Copy the visual system and layout precisely; replace all copy, names, and logos with placeholders. No Linear wordmark or logo. |
| 10 | Source pages | **Everything reachable, including docs** — homepage, features, pricing, customers, changelog, docs, and the other linked marketing pages. |
| 11 | Component groups | **All three**: core primitives, marketing sections, and mock product UI. |
| 12 | Interaction | **Primitives interactive, marketing static.** Ark UI primitives are fully functional Solid islands; marketing sections and mock-product scenes render as static visuals with CSS hover/focus only. |

### 2.4 Round 4 — showcase, theming, verification

| # | Question | Decision |
|---|----------|----------|
| 13 | Showcase shape | **Full-page replicas + gallery.** A component gallery plus rebuilt Linear-like pages. |
| 14 | Theming | **Dark-first + light toggle.** Linear's dark look is the default; semantic tokens define both modes, switchable at runtime. |
| 15 | Layer layout | **`src/components/headless`** and **`src/components/styled`**. |
| 16 | Verification | **Add Vitest + Playwright.** Unit tests for headless/styled behavior; Playwright smoke + screenshot + a11y checks for the showcase. |
| 17 | Token source | **Extract computed values from linear.app** (one-off run, documented — see #21). |

### 2.5 Round 5 — integration details

| # | Question | Decision |
|---|----------|----------|
| 18 | Astro/Solid shape | **Astro wrappers around Solid islands.** Each styled component has a `.astro` file that renders the Solid component as an island only when it needs behavior; static parts stay pure markup. |
| 19 | Extraction delivery | **One-off, documented.** Extract once during implementation; record the method, URL list, and date; commit only the resulting token files. |
| 20 | Fonts | **Self-host Inter Variable** (open source) via local files/Fontsource. No CDN font requests. |
| 21 | Docs | **`DESIGN.md` in the package** — tokens, layers, and component conventions (design-system-baton convention). |
| 22 | GPUI interop | **None. Keep separate.** `apps/todo-2/src/theme.rs` is untouched; no Rust token generation. |

### 2.6 Round 6 — wiring, motion, a11y

| # | Question | Decision |
|---|----------|----------|
| 23 | pnpm wiring | **Root `package.json` + `pnpm-workspace.yaml`.** The repo root owns the workspace and lockfile; the showcase depends on `web-design-system` via `workspace:*`. |
| 24 | Nix | **Yes, add Node + pnpm** to the devShell, and add Cargo `exclude` entries so `cargo` is unaffected. |
| 25 | Motion | **CSS transitions/animations only now**, structured so an animation library can be added later; honor `prefers-reduced-motion`. |
| 26 | Accessibility | **WCAG 2.1 AA** — contrast-checked tokens, full keyboard nav, `focus-visible`, Ark UI ARIA, reduced motion. |
| 27 | Inventory detail | **Full mapped inventory table** — every component, its Linear source, its layer, and an implementation priority (§6). |

### 2.7 Round 7 — naming and closing scope

| # | Question | Decision |
|---|----------|----------|
| 28 | Showcase id | **`demos/design-system-showcase`** |
| 29 | Spec file name | **`web-design-system-spec.md`** |
| 30 | Replica routes | **Home, pricing, features, customers, changelog, docs** |
| 31 | Layout primitives | **Yes: `Box`, `Stack`, `Container`, `Grid`**, built as Panda pattern-based primitives and exported alongside components. |

---

## 3. What is being cloned (source inventory)

Linear's site is a React marketing + product surface. The component families
observed while browsing (homepage and pricing fetched directly; the rest named
as families to mine):

| Source | Surface mined |
|--------|---------------|
| `linear.app/homepage` | Announcement pill ("New Loops →"), navbar, hero headline + CTA, product visual, logo cloud, feature sections ("Intake and integrations", "Planning and monitoring", "AI and automations", "Build, review, and ship"), interactive mock board, timeline/gantt, agent chat, code diff, changelog cards, testimonial carousel, stats ("40,000 product teams"), CTA band, footer. |
| `linear.app/pricing` | Pricing tier cards (Free/Basic/Business/Enterprise), plan comparison table, final CTA. |
| `linear.app/features` | Feature index cards and repeated feature sections. |
| `linear.app/customers` | Customer story cards, logo grid, testimonial quotes. |
| `linear.app/changelog` | Changelog list/detail entries with dates. |
| `linear.app/docs` | Docs chrome: sidebar nav, table of contents, prose, callouts, code blocks. |
| Other reachable links (`/method`, `/now`, `/blog`, `/careers`, `/contact`, `/download`, `/security`, …) | Additional marketing layouts, article/blog cards, and section variants worth folding into the gallery. |

The exact URL list actually used for token extraction is recorded in the
extraction note (§4.4). The visual result must be recognizable as Linear's
system; the content must not be Linear's.

---

## 4. Architecture

### 4.1 Layer model

```
tokens ─────────────► Panda preset (shared)
                          │
                          ├──► headless:  Ark UI (Solid) primitives, unstyled behavior + a11y
                          │
                          ├──► styled:    Panda recipes/patterns around headless (Solid)
                          │
                          └──► astro:     .astro wrappers that hydrate Solid only where needed
```

- **Tokens layer** — raw + semantic tokens (colors, type scale, spacing,
  radius, shadow, blur, motion durations), authored for dark-first with a light
  set, exported through the preset (§4.4).
- **Headless layer** (`src/components/headless`) — thin Solid wrappers over the
  Ark UI Solid primitives (`Dialog`, `Popover`, `Menu`, `Tabs`, `Tooltip`,
  `Select`, `Combobox`, `Checkbox`, `Switch`, `RadioGroup`, `Slider`,
  `Accordion`, `Carousel`, `Toast`, `HoverCard`, `Progress`, `SegmentGroup`,
  `TagsInput`, …). No styling; accessible behavior, keyboard handling, focus
  management, and state come from Ark UI.
- **Styled layer** (`src/components/styled`) — Panda `styled()` recipes and
  patterns composing the headless primitives into the Linear look. Also hosts
  the layout primitives (`Box`, `Stack`, `Container`, `Grid`).
- **Astro wrappers** — live beside their styled components in
  `src/components/styled` (per decision #18/#15): a `.astro` file importing the
  Solid component and choosing a hydration directive (§4.3). Static components
  (marketing, product visuals) are pure `.astro` using Panda `css()`.

### 4.2 Panda preset sharing and class parity

The DS exports a preset from `web-design-system/preset` (`definePreset` from
`@pandacss/dev`) containing `tokens`, `semanticTokens`, `recipes`, and
`patterns`. Consumers (the showcase) `extend` it in their `panda.config`.

Panda generates a per-project `styled-system`, so both packages run their own
`panda codegen`. For class names to line up across the boundary:

- both configs must share the same `prefix`, `hash`, `separator`, and `outdir`
  class-generation settings;
- the showcase's `panda.config` must list the DS source in its `include` globs,
  so the CSS for every DS component the app uses is emitted into the app's
  stylesheet;
- the DS keeps its own generated `styled-system` as a **dev-only** artifact; the
  showcase never imports the DS's generated output, only the DS source +
  preset.

This class-parity requirement is the single most likely source of "styles
silently missing" bugs and is called out in §11.

### 4.3 Astro + Solid island policy

- **Interactive (primitives)**: the `.astro` wrapper renders the Solid
  component with `client:load` for above-the-fold/nav/shortcut surfaces and
  `client:visible` for below-the-fold interactive sections. Each wrapper's
  directive is specified per component in §6 (or defaults to `client:visible`).
- **Static (marketing + mock product UI)**: pure `.astro` markup styled with
  Panda `css()` — **no** Solid island, no runtime. `client:*` is never applied
  to these.
- Hydration is opt-in and documented on each wrapper; there is no blanket
  `client:load`.

### 4.4 Theming and tokens

- **Dark-first**: `:root` carries Linear's dark values; `[data-theme="light"]`
  overrides. A `ThemeToggle` component flips `document.documentElement`'s
  `data-theme` and persists to `localStorage`; no flash-of-wrong-theme
  (a tiny inline script sets the attribute before first paint).
- **Semantic tokens** (`bg.surface`, `fg.default`, `fg.muted`, `border.hairline`,
  `accent.default`, `accent.hover`, `danger`, …) map onto raw palettes; recipes
  consume semantic tokens only, so light mode is a token swap, not a component
  rewrite.
- **Token extraction**: a throwaway Playwright script reads computed styles and
  CSS custom properties from the URL list at a fixed viewport and dumps raw
  values. Only the resulting token source files are committed; `DESIGN.md`
  records the method, the exact URLs, the date, and the viewport used. Tokens
  are rounded/normalized by hand after extraction (Linear's values are
  minified and not a designed scale).
- **Typography**: self-hosted Inter Variable (OFL). Display sizes use the same
  family with tighter tracking rather than a second font. No external font
  requests.
- **Parsing note**: do not scrape Linear's class names or copy their CSS
  verbatim — only *values* are extracted; all authored CSS is ours.

### 4.5 Fidelity rules

- Layout, spacing, type scale, color, radius, shadow, and motion: matched.
- Copy, product names, customer names, quotes, logos, and screenshots:
  **replaced** with generic placeholders ("Product X", "Acme", dummy issue
  ids, invented quotes).
- The Linear wordmark/logo is **not** reproduced; the navbar uses a neutral
  placeholder mark.
- Mock product data must be invented (issue ids, people, repos) and must not
  restate Linear's own demo content.

---

## 5. Package layout: `libs/web-design-system`

```
libs/web-design-system/
├── package.json                 # name "web-design-system"; exports map below
├── panda.config.ts              # extends its own preset; dev-only codegen
├── postcss.config.cjs
├── tsconfig.json
├── DESIGN.md                    # tokens, layers, conventions (decision #21)
├── README.md
├── src/
│   ├── preset.ts                # exported Panda preset (definePreset)
│   ├── tokens/
│   │   ├── colors.ts            # raw palette extracted from linear.app
│   │   ├── semantic.ts          # semantic tokens, dark + light
│   │   ├── typography.ts
│   │   ├── spacing.ts  radius.ts  shadows.ts  motion.ts
│   │   └── EXTRACTION.md        # method, URLs, date, viewport (§4.4)
│   ├── styles/
│   │   ├── reset.css            # base reset + normalize
│   │   ├── base.css             # global element styling
│   │   └── fonts.css            # @font-face for Inter Variable
│   ├── assets/fonts/            # self-hosted Inter Variable files
│   ├── components/
│   │   ├── headless/            # Ark UI wrappers (unstyled)
│   │   │   ├── Dialog.tsx  Popover.tsx  Menu.tsx  Tabs.tsx ...
│   │   │   └── index.ts
│   │   └── styled/              # Panda recipes + Solid + .astro wrappers
│   │       ├── Button.tsx  Button.astro  button.recipe.ts
│   │       ├── ...
│   │       ├── marketing/       # .astro-only sections
│   │       ├── product/         # .astro-only mock product UI
│   │       ├── layout/          # Box, Stack, Container, Grid (Panda patterns)
│   │       └── index.ts
│   ├── icons/                   # placeholder icon set (see §11.2)
│   └── lib/                     # cx(), type helpers, polymorphic helpers
└── tests/                       # Vitest specs for headless/styled behavior
```

**Exports map** (source-consumed, decision #2):

| Entry | Provides |
|-------|----------|
| `web-design-system/preset` | the Panda preset (`tokens`, `semanticTokens`, `recipes`, `patterns`) |
| `web-design-system/components` | styled components + `.astro` wrappers + layout primitives |
| `web-design-system/headless` | unstyled Ark UI wrappers |
| `web-design-system/styles.css` | reset + base + fonts |

Because Astro consumes `.astro` files as source, the `exports` map points at
`src/**` (`"./components/*": "./src/components/styled/*"`, etc.) rather than a
build output.

---

## 6. Component inventory (full mapped inventory)

Priority: **P0** = needed before any replica; **P1** = replica-facing;
**P2** = polish/deep-surface. Layer: `H` = headless (Ark UI), `S` = styled
(Panda + Solid), `A` = `.astro` static. `client:*` applies to `S`/`H` only; `A`
is never hydrated.

### 6.1 Foundations (P0, no component)

Tokens (color/semantic/type/spacing/radius/shadow/motion), global reset + base,
self-hosted Inter Variable, focus ring, reduced-motion handling, theme toggle.

### 6.2 Layout primitives (P0) — `web-design-system/components`

| Component | Layer | Notes |
|-----------|-------|-------|
| `Box` | S | Panda `box` pattern wrapper |
| `Stack` / `HStack` / `VStack` | S | flex + gap pattern |
| `Grid` | S | grid pattern |
| `Container` | S | max-width + gutters (Linear's ~1200px content column) |
| `Separator` / `Divider` | S | hairline borders |

### 6.3 Core primitives (P0–P1) — headless (`H`) + styled (`S`)

| Component | Headless | Source (Linear surface) |
|-----------|----------|-------------------------|
| `Button` (variants: primary/secondary/ghost/danger; sizes) | — | navbar, hero, pricing CTAs |
| `IconButton` | — | navbar, cards |
| `Badge` / `Tag` / `LabelChip` | — | issue labels, "Beta/GA" pills |
| `Avatar` + `AvatarGroup` | Ark `Avatar` | issue assignees, testimonials, nav |
| `Input` / `Field` / `Textarea` | Ark `Field` | docs forms, newsletter |
| `Select` / `Combobox` | Ark `Select`/`Combobox` | footer region picker, filters |
| `Checkbox` / `RadioGroup` / `Switch` | Ark | pricing table, docs |
| `Slider` | Ark `Slider` | docs/demo controls |
| `Tabs` / `SegmentGroup` | Ark | feature switchers, docs |
| `Menu` / `ContextMenu` | Ark `Menu` | nav dropdowns, row actions |
| `Dialog` / `Modal` | Ark `Dialog` | demo interactions |
| `Popover` / `HoverCard` / `Tooltip` | Ark | nav, property pickers |
| `Toast` | Ark `Toast` | demo notifications |
| `Accordion` | Ark `Accordion` | FAQ/security sections |
| `Carousel` | Ark `Carousel` | testimonial + changelog carousels |
| `Progress` / `Spinner` / `Skeleton` | Ark `Progress` | loading/async surfaces |
| `Pagination` / `Breadcrumb` | — | docs, changelog |
| `KeyboardShortcut` (`<kbd>`) | — | command palette, docs |
| `StatusDot` / `StatusBadge` | — | product mock UI |
| `CommandMenu` (⌘K) | Ark `Dialog`+`Combobox` | navbar search |
| `Table` / `DataTable` | — | pricing comparison table |
| `Alert` / `Notice` | — | docs callouts, inline states |

### 6.4 Marketing sections (P1) — static `A`

| Component | Source (Linear) | Priority |
|-----------|-----------------|----------|
| `AnnouncementPill` ("New … →") | homepage top | P0 |
| `Navbar` + `NavDropdown` (mega-menu) | homepage/docs chrome | P0 |
| `Hero` (eyebrow, H1, subhead, CTA pair, visual slot) | homepage | P0 |
| `LogoCloud` ("Powering the companies…") | homepage | P0 |
| `FeatureSection` (label/title/body + visual, alternating sides) | homepage ×4 | P0 |
| `FeatureCarouselTabs` (Purpose-built / Powered by agents / Designed for speed) | homepage | P1 |
| `IntegrationGrid` ("Intake and integrations") | homepage | P1 |
| `StatsBand` ("40,000 product teams") | homepage | P1 |
| `ChangelogCard` / `ChangelogList` | homepage + `/changelog` | P1 |
| `TestimonialCard` / `TestimonialCarousel` | homepage + `/customers` | P1 |
| `CTASection` ("Built for the future. Available today.") | homepage, pricing | P1 |
| `Footer` (multi-column, theme toggle, region) | all pages | P0 |
| `PricingCard` / `PricingTierGrid` | `/pricing` | P1 |
| `PricingComparisonTable` | `/pricing` | P1 |
| `FAQAccordion` | pricing/security | P2 |
| `CustomerStoryCard` / `CustomerLogoGrid` | `/customers` | P1 |
| `DocSidebar` / `DocTOC` / `Prose` / `Callout` | `/docs` | P1 |
| `BlogPostCard` / `ArticleHeader` | `/blog`, `/method` | P2 |
| `SectionHeading` / `Eyebrow` | all | P0 |

### 6.5 Mock product UI (P1–P2) — static `A`

| Component | Source (Linear) | Priority |
|-----------|-----------------|----------|
| `IssueRow` / `IssueCard` (id, title, label, avatar, priority) | hero board, feature mocks | P0 |
| `IssueList` (grouped: Todo / In Progress / Done) | homepage board | P0 |
| `KanbanColumn` + `ColumnHeader` (count chips "Backlog 8") | homepage board | P0 |
| `KanbanBoard` (columns side by side) | homepage board | P0 |
| `ProjectRow` / `CycleWidget` / `PropertyPicker` | homepage board | P2 |
| `PriorityIcon` / `LabelChip` / `AssigneeStack` | everywhere | P0 |
| `Timeline` + `TimelineRow` / `GanttBar` (MAR–SEP month columns) | "Planning and monitoring" | P1 |
| `AgentActivityFeed` ("Linear created the issue via Slack…") | hero activity | P1 |
| `ChatMessage` / `ChatThread` (user + agent bubbles, "Worked for 8 sec") | "AI and automations" | P1 |
| `EditorTabs` (`kinetic-ios/src/…`) | "Build, review, and ship" | P2 |
| `CodeBlock` (syntax-highlighted) | docs + code mock | P1 |
| `DiffViewer` (side-by-side / unified) | "Build, review, and ship" | P1 |
| `NotificationToast` | product mock | P2 |

---

## 7. Showcase: `demos/design-system-showcase`

An Astro site with pnpm-workspace link to `web-design-system`.

Routes:

| Route | Content |
|-------|---------|
| `/` | Overview: what the DS is, layers diagram, install/usage snippet. |
| `/foundations` | Token gallery: color, semantic roles, type scale, spacing, radius, shadow, motion; dark/light side by side. |
| `/primitives` | Every `H`/`S` primitive with variants, sizes, states, and an interactive demo. |
| `/layout` | `Box`, `Stack`, `Grid`, `Container` demos. |
| `/marketing` | Every marketing section rendered in isolation with placeholder content. |
| `/product-ui` | Every mock product component and a composed board/timeline/diff scene. |
| `/replicas/home` | Full homepage replica. |
| `/replicas/pricing` | Full pricing replica. |
| `/replicas/features` | Full features replica. |
| `/replicas/customers` | Full customers replica. |
| `/replicas/changelog` | Full changelog replica. |
| `/replicas/docs` | Docs chrome replica with sidebar/TOC/prose. |

- Shared layout: top navbar + left sidebar index (all routes), theme toggle,
  responsive collapse at the same breakpoints as the DS.
- Every gallery entry shows the rendered component, its import path, its layer
  tag (`headless`/`styled`/`astro`), and its hydration directive when any.
- Placeholder content only (§4.5).

---

## 8. Tooling and workspace wiring

### 8.1 pnpm workspace

- **Root `package.json`**: `private: true`, `packageManager` pinned to the pnpm
  version added to Nix, scripts for `dev`, `build`, `typecheck`, `test`
  (delegating to the packages).
- **`pnpm-workspace.yaml`**: `packages: ["libs/web-design-system",
  "demos/design-system-showcase"]`.
- `pnpm-lock.yaml` committed at the repo root.
- The showcase depends on `"web-design-system": "workspace:*"`.

### 8.2 Nix devShell

- `flake.nix`: add `nodejs` and `pnpm` to the devShell packages so
  `pnpm install` / `pnpm build` work inside `nix develop` (matching the repo's
  reproducible-environment convention).
- Node major version pinned in the flake; `packageManager` in the root
  `package.json` matches.

### 8.3 Cargo workspace exclusion

`Cargo.toml` uses `members = ["apps/*", "libs/*", "demos/*"]`. The JS packages
must not be treated as Cargo members. Add explicit exclusions so `cargo
check --workspace` is unaffected:

```toml
[workspace]
members = [ "apps/*", "libs/*", "demos/*" ]
exclude = [ "libs/web-design-system", "demos/design-system-showcase" ]
```

(The `exclude` entries are defensive and document intent, regardless of whether
the glob would otherwise skip a directory without a `Cargo.toml`.)

### 8.4 `.gitignore`

Add JS build artifacts so the Rust ignore list stays focused:

```
node_modules/
.astro/
dist/
styled-system/
.panda/
playwright-report/
test-results/
```

`styled-system/` is generated by Panda in both packages and is never committed.

---

## 9. Accessibility and motion

- **Target: WCAG 2.1 AA** for every shipped component.
- Text/icon contrast checked for both themes as part of token authoring; the
  extraction step records contrast ratios in `DESIGN.md`.
- Ark UI supplies roles, labels, focus traps, and keyboard interaction; the
  styled layer must not strip them.
- Visible `:focus-visible` ring on every interactive element; focus order
  matches visual order.
- `prefers-reduced-motion`: motion tokens collapse to `0ms` and scene
  animations are disabled.
- Icon-only controls carry `aria-label`; decorative mock visuals are
  `aria-hidden`.

---

## 10. Verification plan

Required to pass:

1. **Install**: `pnpm install` from the repo root (workspace links resolve).
2. **Typecheck**: `pnpm typecheck` (`tsc --noEmit` per package) and
   `astro check` in the showcase.
3. **Panda codegen**: `pnpm build` runs `panda codegen` in each package and
   emits the showcase stylesheet; a missing-CSS check (class parity, §4.2) is
   part of review.
4. **Vitest**: unit tests for headless wrappers (open/close, keyboard behavior,
   controlled state) and for token/recipe helpers.
5. **Playwright**: showcase smoke test across every route (no console errors,
   no unresolved styles), light/dark screenshots of the gallery and replicas,
   and axe-based accessibility assertions at AA.
6. **Nix**: `cargo check --workspace` still clean with the new directories
   present.

Explicitly **not** in scope: visual-regression baselines against Linear itself
(we match by eye, not by pixel-diffing their site), and no live network in
tests beyond local dev.

---

## 11. Risks and open questions

### 11.1 Panda class parity across packages (§4.2)

Highest technical risk. If the DS and showcase configs diverge on `prefix`,
`hash`, `separator`, or `outdir`, DS components render unstyled in the showcase
with no error. Mitigation: the showcase extends the exported preset and matches
generation settings exactly; a Playwright assertion checks that primitives have
computed styles.

### 11.2 Icon set

Linear's icons are custom and cannot be reproduced. **Assumed default:** ship a
placeholder set (e.g. Lucide) behind the DS's own `Icon` wrapper so the set can
be swapped without touching components. Confirm before implementation.

### 11.3 Astro + Ark UI + Solid + Panda integration friction

No such stack exists in the repo yet. Risks: Ark UI's Solid peer versions vs
Astro's Solid integration, SSR of Ark primitives (must be island-only), and
Panda's Astro integration ordering. Mitigation: a vertical slice (Button +
Dialog + Navbar) is built first (§13 step 1) before committing to the full
inventory.

### 11.4 Extraction fidelity

Linear's computed values change and are minified; extraction gives raw values,
not a clean scale. Tokens will be hand-normalized, so "exact" is not
guaranteed — the goal is indistinguishable-at-a-glance, not value-identical.

### 11.5 Fidelity boundary

"Visual clone / placeholder content" still invites drift: it is easy to paste
Linear's real copy while copying layout. Review must check that no Linear
wordmark, screenshot, quote, or product name ships.

### 11.6 Still open (mechanical)

1. Icon set confirmation (§11.2).
2. Exact Node/pnpm major versions to pin in the flake.
3. Whether Ark UI's full primitive set is vendored or only the components the
   inventory needs.
4. Exact viewport and URL list for the one-off extraction.
5. Whether `demos/design-system-showcase` gets its own `README.md` (a run
   instruction) beyond the spec.
6. Whether the showcase ships a built `dist/` in the repo (like
   `demos/desktop-tauri/dist`) or is build-only.
7. Breakpoint set (assumed: Linear-like ~640/768/1024/1280).

---

## 12. Non-goals

- No changes to the Rust workspace, the GPUI app, or `apps/todo-2/src/theme.rs`.
- No coupling between web tokens and the desktop theme (decision #22).
- No publishing of `web-design-system` to a registry.
- No real Linear content: no wordmark, logos, screenshots, customer names,
  quotes, or product copy.
- No JavaScript framework beyond Solid (no React/Svelte islands).
- No animation library in v1 (CSS only, decision #25).
- No Storybook; `DESIGN.md` + the showcase gallery are the documentation.
- No i18n, no dark/light beyond the two themes, no RTL work in v1.

---

## 13. Implementation order

1. **Vertical slice** — pnpm workspace + root manifest, flake Node/pnpm, Cargo
   excludes, `.gitignore`; `libs/web-design-system` skeleton (preset, tokens
   from extraction, Panda config) and `demos/design-system-showcase` skeleton;
   prove Astro + Solid + Ark UI + Panda end to end with `Button`, `Dialog`, and
   a minimal `Navbar`. Tests: typecheck, `astro check`, one Vitest spec, one
   Playwright smoke test.
2. **Foundations** — full token set (dark + light), reset/base/fonts, layout
   primitives, `DESIGN.md`.
3. **Core primitives** — headless wrappers then styled components, plus their
   `.astro` wrappers and gallery entries (`/primitives`, `/layout`).
4. **Mock product UI** — issue/board/timeline/chat/diff components
   (`/product-ui`).
5. **Marketing sections** — navbar through footer (`/marketing`).
6. **Replicas** — home, then pricing, features, customers, changelog, docs.
7. **A11y + motion pass** — axe assertions, reduced-motion, focus-visible,
   contrast notes in `DESIGN.md`.
8. **Polish** — responsive breakpoints, gallery presentation, README/run
   instructions, screenshot baselines.

---

## 14. Implementation status

Shipped in this repository.

**Workspace wiring**

- Root `package.json` + `pnpm-workspace.yaml` (pnpm 11; `allowBuilds.esbuild`,
  since pnpm blocks build scripts by default).
- `flake.nix` devShell gained `nodejs_22` and `pnpm`.
- `Cargo.toml` gained `exclude = ["libs/web-design-system",
  "demos/design-system-showcase"]`, and `.gitignore` covers the JS artifacts.
- Verified: `cargo metadata` does not list either JS directory as a member.

**`libs/web-design-system`**

- Tokens: raw palette, semantic colors (dark base + `_light` override),
  spacing/sizes, type scale + text styles, radii, shadows, motion, keyframes.
  Spacing and sizes are defined explicitly in the preset because the base
  scale did not survive the preset merge; package-level CSS-variable names are
  readable (`--colors-canvas`) because both configs use `hash: false`.
- Preset exported from `./preset`, consumed by the showcase via
  `presets: [pandaPreset, preset]`.
- Headless barrel over Ark UI Solid (`@ark-ui/solid` has no `Portal` export in
  v5; overlay positioners render without it).
- Styled: `Box`, `Stack`/`HStack`/`VStack`, `Grid`, `Container`, `Separator`,
  `Card`, `Button`, `IconButton`, `Badge`, `Avatar`/`AvatarGroup`, `Input`,
  `Field`, `Kbd`, `StatusDot`, `Spinner`, `Skeleton`, `Alert`, `Tabs`,
  `Accordion`, `Switch`, `Checkbox`, `Dialog`, `Popover`, `Tooltip`, `Menu`,
  `ThemeToggle`, plus product types. `.astro` wrappers for the interactive
  primitives.
- `src/styles/` reset + base, wrapped in the `reset`/`base` cascade layers.
- Marketing: `Navbar`, `Footer`, `Hero`, `AnnouncementPill`, `SectionHeading`,
  `LogoCloud`, `FeatureSection`, `StatsBand`, `ChangelogCard`,
  `TestimonialCard`, `CustomerStoryCard`, `PricingCard`,
  `PricingComparisonTable`, `Callout`, `Prose`, `DocSidebar`, `DocTOC`.
- Mock product UI: `IssueRow`, `LabelChip`, `PriorityIcon`, `KanbanBoard`,
  `Timeline`, `AgentActivityFeed`, `ChatThread`, `CodeBlock`, `DiffViewer`,
  `EditorTabs`, `NotificationToast`.
- `DESIGN.md`, `README.md`, `src/tokens/EXTRACTION.md`.

**`demos/design-system-showcase`**

- Gallery pages (`/`, `/foundations`, `/layout`, `/primitives`, `/marketing`,
  `/product-ui`) and six replicas (`home`, `pricing`, `features`, `customers`,
  `changelog`, `docs`), with placeholder data in `src/data/placeholder.ts`.
- CSS is generated with `panda cssgen` (Astro's CSS pipeline does not run
  `postcss.config`), wired into `prebuild`/`predev`/`pretypecheck`.
- 12 static routes build cleanly.

**Verification**

- `pnpm build` (design-system codegen + Astro build): green, 12 pages.
- `pnpm typecheck` (DS `tsc --noEmit` + showcase `astro check`): green.
- `pnpm test` (Vitest): 15 tests green — `cn`, token integrity, WCAG AA
  contrast math, and a Button render/behavior test.
- `pnpm test:e2e` (Playwright against installed Chrome, no browser download):
  15 tests green — every route loads with no console errors, the token
  stylesheet applies, the theme toggle persists, and **axe reports no
  serious/critical WCAG 2.1 AA violations on all 12 routes**.

**§11 open items, as resolved**

1. Icons: a hand-rolled placeholder set behind `src/icons` (no Lucide
   dependency).
2. Node/pnpm: `packageManager: pnpm@11.25.0`; `nodejs_22` + `pnpm` in the flake.
3. Ark UI: only the primitives the inventory needs are re-exported.
4. Extraction: method, URL list, viewport, and date recorded in
   `src/tokens/EXTRACTION.md`.
5. Showcase README: added.
6. The showcase builds to `dist/`; it is gitignored, not committed.
7. Breakpoints: 640/768/1024/1280/1536, matching the token definitions.

**Contrast fixes forced during implementation** (recorded in `DESIGN.md` §5)

- `fg.muted`/`fg.subtle` were retuned to clear 4.5:1 in both themes.
- Accent-tinted text uses a new `accent.text` role, separate from the `accent`
  fill which white text sits on.
- Badge/label foregrounds use new `badge.*` roles.
- The danger button uses `danger.fill` (a darker red) for white-text contrast.
- Scrollable regions are `tabindex="0"`; the plan table and file-tab strip got
  `role="region"`/`role="group"` labels.

**Deliberately not implemented** (listed in `DESIGN.md` §7): `Select`,
`Combobox`, `RadioGroup`, `Slider`, `SegmentGroup`, `Carousel`, `Progress`,
`Toast`, `Pagination`, `Breadcrumb`, `CommandMenu`, `HoverCard`, and the
pricing FAQ accordion. The headless barrel already re-exports their Ark UI
primitives, so each is a styling pass plus an `.astro` wrapper.

# docs-web

The documentation website for todo-lofi: what the app does, how to run it, how the
workspace is put together, and what contributing looks like.

It is a plain Astro site built with the repository's own design system
(`libs/web-design-system`) — the same Panda preset, tokens, marketing docs
components (`DocSidebar`, `DocTOC`, `Prose`, `Callout`), and `CodeBlock` that
`demos/design-system-showcase` renders.

## Structure

`/` is the landing page: a marketing-shaped introduction built from the design
system's marketing and product components, with links into the docs.

**Every documentation page lives under `/docs`.** The docs navigation has two
levels: the header's tab bar splits the docs into two guides, one per audience,
and the left nav inside each guide lists only that guide's pages, nested under
section headings (`Getting started`, `Tasklist`, `Integrations`,
`Configuration`). Both levels come from `src/data/docs.ts` — `Mini-apps and
extensions` is filed under `Integrations`, since a mini-app is the app's
extension surface rather than part of the everyday tasklist.

| Guide / section | Route | Content |
| --- | --- | --- |
| Landing | `/` | What the app is, what it does, and the way into both guides |
| User guide | `/docs` | Overview: what the app is, highlights, and where to start |
| User guide | `/docs/install` | Requirements, running from a checkout, the macOS app bundle |
| User guide | `/docs/tasks` | The task list, task details, tags, projects and areas, filtering |
| User guide | `/docs/repeats` | Repeat rules, dates and times, why a picker |
| User guide | `/docs/workflows` | Workflows as staged state, and the coding workflow |
| User guide | `/docs/agent` | The agent pane: what it does, what it inherits, how to configure it |
| User guide | `/docs/sync` | Offline first, Todoist, GitHub, and what syncs |
| User guide | `/docs/mini-apps` | Travel checklists today, the planned ones, and the model behind them |
| User guide | `/docs/status` | Available / preview / planned, per feature |
| User guide | `/docs/configuration` | In-app settings, providers and keys, Todoist, GitHub |
| Contributor guide | `/docs/contributor` | Landing: what the guide covers and the reading order |
| Contributor guide | `/docs/contributor/development` | Nix + direnv setup, the dev shell commands, web packages |
| Contributor guide | `/docs/contributor/architecture` | Workspace layout, the desktop app, persistence, specs |
| Contributor guide | `/docs/contributor/data-model` | Storage internals: the task, priority scoring, tags, links, ownership, repeats |
| Contributor guide | `/docs/contributor/workflow-engine` | Recipes, runs, the coding pipeline, git worktrees, the loopback tool server |
| Contributor guide | `/docs/contributor/agent-runtime` | ACP surface, the connection, tools, sub-agents, vendored patches |
| Contributor guide | `/docs/contributor/integrations` | Sync internals, provider layer, MCP/hooks, agent protocol |
| Contributor guide | `/docs/contributor/contributing` | Workflow, checks, and the Rust/web conventions |
| Contributor guide | `/docs/contributor/license` | AGPL-3.0 in practice, third-party code, contributions |

## Running

From the repository root:

```sh
pnpm install
pnpm dev           # astro dev, regenerates Panda CSS first
pnpm build:docs    # codegen + static build
pnpm preview:docs  # serve the built site
pnpm typecheck     # includes this package
```

From this directory:

```sh
pnpm codegen    # panda codegen + panda cssgen
pnpm dev
pnpm build
pnpm typecheck  # astro check
pnpm test:e2e   # Playwright smoke, theming, layout, and axe AA checks
```

The e2e suite expects the locally installed Chrome and starts its own preview
server on port 4322 (the showcase suite uses 4321, so both can run at once).

## Adding a page

1. Create `src/pages/docs/<slug>.astro` (contributor pages under
   `src/pages/docs/contributor/`) and wrap the content in the `Docs` layout,
   passing `title`, `description`, and a `toc` list whose hrefs match the page's
   heading ids. Reference tables get wrapped in `TableScroll.astro` so wide
   content scrolls instead of widening the page.
2. Add the page to the matching section in `src/data/docs.ts`. The header tabs,
   the left nav, and the previous/next links all read from that file, so one
   entry is enough. Section headings are only rendered when a guide has more
   than one.

## Notes

- Styling comes from `panda cssgen`, not PostCSS: Astro's CSS pipeline does not
  run `postcss.config` here. `styled-system/styles.css` is generated and
  gitignored.
- `panda.config.ts` must keep `prefix`, `hash`, and `separator` identical to
  `libs/web-design-system/panda.config.ts` and the showcase's config, and its
  `include` globs must cover the design system's source — otherwise the design
  system's components render unstyled with no error (`DESIGN.md` §3).
- The automations pane on the landing page and the overview is a
  **design-system replica** (`src/components/AutomationsPanel.astro`), not a
  bitmap — the same approach the showcase takes for its page replicas. Its
  action affordances are `span`s rather than `button`s on purpose: a row of
  controls that do nothing would be a lie to anyone using a keyboard or a
  screen reader.
- The landing page's illustrative data lives in `data/landing.ts`, **outside**
  `src/`. Panda scans every file under `src/` for style-shaped objects, and
  those records use keys that are also CSS property names (`title`, `status`,
  `id`, `priority`, `meta`), which it turned into declarations like
  `meta: round 1;` and broke the CSS build. Keep mock product data out of the
  scanned tree.
- The design system's reset/base stylesheet is imported after the generated
  stylesheet so their cascade layers line up (`DESIGN.md` §2).
- `src/styles/app.css` holds the only app-level CSS: long file paths and command
  names are unbreakable, and without `overflow-wrap` they force the page sideways
  at mobile widths. Reference tables are wrapped in `TableScroll.astro`, a
  focusable scroll region (`DESIGN.md` §5), for the same reason.

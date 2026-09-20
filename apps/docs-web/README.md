# docs-web

The documentation website for todo-lofi: what the app does, how to run it, how the
workspace is put together, and what contributing looks like.

It is a plain Astro site built with the repository's own design system
(`libs/web-design-system`) — the same Panda preset, tokens, marketing docs
components (`DocSidebar`, `DocTOC`, `Prose`, `Callout`), and `CodeBlock` that
`demos/design-system-showcase` renders.

## Routes

| Route | Content |
| --- | --- |
| `/` | Overview: what the app is, highlights, and where to start |
| `/quickstart` | Nix + direnv setup, running the app, the command reference |
| `/features` | Tasks, agent, repeats, workflows, sync, mini-apps, and status |
| `/architecture` | Workspace layout, the desktop app, persistence, specs |
| `/integrations` | Todoist, GitHub, providers, MCP/hooks, and the agent protocol |
| `/contributing` | Workflow, checks, and the Rust/web conventions |
| `/license` | AGPL-3.0 in practice, third-party code, contributions |

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

1. Create `src/pages/<slug>.astro` and wrap the content in the `Docs` layout,
   passing `title`, `description`, and a `toc` list whose hrefs match the page's
   heading ids.
2. Add the page to `src/data/docs.ts`. The header nav, the sidebar, and the
   previous/next links all read from that file, so one entry is enough.

## Notes

- Styling comes from `panda cssgen`, not PostCSS: Astro's CSS pipeline does not
  run `postcss.config` here. `styled-system/styles.css` is generated and
  gitignored.
- `panda.config.ts` must keep `prefix`, `hash`, and `separator` identical to
  `libs/web-design-system/panda.config.ts` and the showcase's config, and its
  `include` globs must cover the design system's source — otherwise the design
  system's components render unstyled with no error (`DESIGN.md` §3).
- The design system's reset/base stylesheet is imported after the generated
  stylesheet so their cascade layers line up (`DESIGN.md` §2).
- `src/styles/app.css` holds the only app-level CSS: long file paths and command
  names are unbreakable, and without `overflow-wrap` they force the page sideways
  at mobile widths. Reference tables are wrapped in `TableScroll.astro`, a
  focusable scroll region (`DESIGN.md` §5), for the same reason.

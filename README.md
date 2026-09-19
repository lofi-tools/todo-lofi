# todo-lofi

A local-first task manager for macOS and Linux, built on [GPUI](https://github.com/nmrshll/gpui). Nested tags, an embedded AI agent pane, Todoist sync, and a keyboard-first workflow—all running offline.

![Screenshot](docs/img/Screenshot%202026-09-14%20at%2011.24.37.png)

## Features

- **Integrated AI agent** — integrated AI assistant for doing tasks.
- **Repeat & scheduling** — recurring task configuration via a dedicated picker.
- **Group & filter tasks using tags** — tree-structured project/area organization with drag-and-drop reordering.
- **Semi-automated workflows** — for instance, coding tasks all go through: interview, spec, coding, review, merge.
<!--- **Tag-to-directory binding** — associate tags with local filesystem directories (for lofi git-note workflows).-->
<!--- **Keyboard-first** — all core actions reachable via keybindings; mouse always optional.-->
<!--- **Nix reproducible dev environment** — `nix develop` gives you every tool, every time.-->
- **2-way sync with your current Todo-list app** — 
  - Todoist
  - Github issues & projects — `preview`
- **Integrated mini-apps (extensions)** — 
  - Travel checklist templates
  - Message follow-ups & birthday wishes — `coming soon`
  - Job application tracking — `coming soon`
- **Let outside apps manage part of your task lists** — `coming soon`

## Quickstart

1. Install Nix and direnv (see [`how`](docs/installing-nix-and-direnv.md)).
2. Allow the environment from the repository root:
   ```bash
   direnv allow
   ```
3. Run the app:
   ```bash
   cargo run -p todo-2    # run the desktop app
   t2                     # or use the watch script for auto-rebuilds
   ```

### Useful commands

#### Launching
- `cargo run -p todo-2` — run the desktop app
- `t2` (or `cargo watch -x "run -p todo-2"`) — run the desktop app with auto-rebuilds
#### Testing
- `ccheck` — cargo check all workspace packages (todo-2, storage, report_proc, agent-cli)
- `testdbg` — run storage tests with debug logging (`cargo test -p storage -- --nocapture --show-output`)
#### Web design system
- `pnpm install` — install the pnpm workspace (`libs/web-design-system`, `demos/design-system-showcase`)
- `pnpm dev` — run the design-system showcase with auto-rebuilds
- `pnpm build` — Panda codegen + static build of the showcase
- `pnpm typecheck` / `pnpm test` — typecheck both JS packages / run the Vitest suite
- `pnpm test:e2e` — Playwright smoke, theming, and WCAG 2.1 AA checks (uses local Chrome)

See `docs/spec/web-design-system-spec.md` and `libs/web-design-system/DESIGN.md`.
#### Contributions
- `mig <args>` — run a storage migration (`cargo run --bin migrate -- migration ...`)
- `clone-patch <crate>` — clone a crate's repo into `patched/` to vendor a fork

## Project Structure

```
todo-lofi/
├── apps/
│   └── todo-2/        # Desktop GPUI application

├── libs/
│   ├── storage/          # SQLite-backed persistence layer
│   ├── web-design-system/ # Linear-style web design system (Astro/Solid/Panda/Ark UI)
│   ├── gpui-tokio/       # GPUI ↔ Tokio bridge
│   ├── ai_providers/     # LLM provider abstraction
│   ├── acp-client/       # Agent Control Protocol client
│   └── ...
├── demos/
│   └── design-system-showcase/ # Astro showcase of the web design system
├── patched/           # Vendored upstream patches
├── docs/              # Specs and agent skills
└── flake.nix          # Nix flake
```

## Contributing

### Temporary docs website: https://deepwiki.com/lofi-tools/todo-lofi

Contributions welcome. Please open an issue first to discuss any non-trivial change.

1. Fork the repo and create a feature branch.
2. Run `cargo check -p todo-2` and `cargo test -p storage` before pushing.
3. Keep commits focused and rebased.

### License

[GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0).

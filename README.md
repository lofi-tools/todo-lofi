# taskstream

A local-first task manager for macOS and Linux, built on [GPUI](https://github.com/nmrshll/gpui). Nested tags, an embedded AI agent pane, Todoist sync, and a keyboard-first workflow—all running offline.

[Landing page](https://get-taskstream.netlify.app/) · [Documentation](https://get-taskstream.netlify.app/docs)

![Screenshot](docs/img/Screenshot%202026-09-19%20at%2021.20.15.png)

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
   cargo run -p taskstream-desktop    # run the desktop app
   t2                     # or use the watch script for auto-rebuilds
   ```

### Useful commands

#### Launching
- `cargo run -p taskstream-desktop` — run the desktop app
- `t2` — run the desktop app with auto-rebuilds. It watches taskstream-desktop's own sources and its in-workspace dependencies, so editing the web packages does not restart the Rust build (see `flake.nix` for the watched paths)
#### Shepherd and reference repositories
- `cargo run -p shepherd` — run the Shepherd Rust app scaffold (`apps/shepherd`, included by the workspace's `apps/*` member glob)
- Shepherd's [goal](apps/shepherd/docs/spec/goal.md), [master spec](apps/shepherd/docs/spec/master-spec.md), [stack](apps/shepherd/docs/spec/stack.md), and [implementation plan](apps/shepherd/docs/todo.md) describe the planned app; the binary is still a scaffold.
- `checkout-references` — shallow-clone all configured reference repositories into `.cache/checkout/<name>`
- `checkout-references herdr-core pi` — download selected references by name
- `checkout-references --list` — list configured names and URLs
- `skills` — install local agent skills, including `reference-repos`, from `docs/agent_skills`

The reference list lives in `flake.nix` as `referenceRepos`: `pandacode`, `herdr`,
`herdr-core`, `herdr-app`, and `pi`. Run checkout commands inside the Nix dev shell
(or use `nix develop -c checkout-references`). Existing checkouts are preserved;
the command does not refresh or reset them. The local skill lets agents inspect
these checkouts by name.

#### Testing
- `ccheck` — cargo check all workspace packages (taskstream-desktop, storage, report_proc, agent-cli)
- `testdbg` — run storage tests with debug logging (`cargo test -p storage -- --nocapture --show-output`)
#### Documentation websites
- `pnpm dev` — run both docs sites: Taskstream on `4321` and Shepherd on `4323`, after codegen
- `pnpm dev:docs` — run only the Taskstream docs (`apps/ts-docs-web`)
- `pnpm dev:shep-docs` — run only the Shepherd docs (`apps/shep-docs-web`)
- `pnpm build:docs` / `pnpm build:shep-docs` — Panda codegen + static build of either site
- `pnpm test:e2e:docs` / `pnpm test:e2e:shep-docs` — Playwright smoke, theming, layout, and WCAG 2.1 AA checks for either site
- `web` — install the JS workspace, stop any dev server a previous session left running, then start both docs sites
- `dev-stop` — stop the dev and preview servers the web packages left running (`astro dev stop` / `astro preview stop`, in `apps/ts-docs-web`, `apps/shep-docs-web`, and the showcase)
- `astro <args>` — the workspace's pinned Astro CLI, e.g. `astro dev status` or `astro check`

Astro refuses to start a second dev server for the same project, and the e2e
suites reuse a preview server that is already listening — so reach for `dev-stop`
(or `web`) instead of killing the process by hand, or a test run will quietly
exercise the previous build.

See `apps/ts-docs-web/README.md` and `apps/shep-docs-web/README.md`.
#### Web design system
- `pnpm install` — install the pnpm workspace (`libs/web-design-system`, `demos/design-system-showcase`, `apps/ts-docs-web`, `apps/shep-docs-web`)
- `pnpm dev:ds` — run the design-system showcase with auto-rebuilds
- `pnpm build` — Panda codegen + static build of the showcase
- `pnpm typecheck` / `pnpm test` — typecheck all JS packages / run the Vitest suite
- `pnpm test:e2e` — Playwright smoke, theming, and WCAG 2.1 AA checks (uses local Chrome)

See `docs/spec/web-design-system-spec.md` and `libs/web-design-system/DESIGN.md`.

The documentation sites consume the same preset and docs components; the
showcase is where the components themselves are documented.
#### Contributions
- `mig <args>` — run a storage migration (`cargo run --bin migrate -- migration ...`)
- `clone-patch <crate>` — clone a crate's repo into `patched/` to vendor a fork

## Project Structure

```
taskstream/
├── apps/
│   ├── taskstream-desktop/        # Desktop GPUI application
│   ├── agent-cli/     # Terminal AI agent + ACP server
│   ├── ts-docs-web/   # Taskstream documentation website (Astro + web design system)
│   └── shep-docs-web/ # Shepherd documentation website (Astro + web design system)

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

### Documentation websites

`apps/ts-docs-web` is the Taskstream site: what the app does, how to run it, how
the workspace is put together, and the integrations it talks to. `/` is the
landing page, and every page of documentation lives under `/docs`, split into a
user guide (`/docs`) and a contributor guide (`/docs/contributor`). Read the
[documentation online](https://get-taskstream.netlify.app/docs). The
[deepwiki mirror](https://deepwiki.com/lofi-tools/taskstream) remains available as
a temporary fallback.

`apps/shep-docs-web` is Shepherd's separate landing site. Both are built with the
in-repo web design system. `pnpm dev` runs both; use `pnpm dev:docs` or
`pnpm dev:shep-docs` for one at a time.

Contributions welcome. Please open an issue first to discuss any non-trivial change.

1. Fork the repo and create a feature branch.
2. Run `cargo check -p taskstream-desktop` and `cargo test -p storage` before pushing.
3. Keep commits focused and rebased.

### License

[GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0).

## Nix development environment

The root flake pins upstream `NixOS/nixpkgs` at `c7def046b9a883d46974757852106483d741586f`, replacing `cachix/devenv-nixpkgs` at its existing upstream base rather than updating package versions. Other locked inputs and local override behavior are unchanged.

On 2026-10-27, macOS arm64 shell evaluation with import-from-derivation disabled passed with the existing local `my-nix` override. The committed `my-nix` pin lacks `flakeModuleGroups`; that error also reproduces with the former Devenv input and is separate from this switch.

```sh
nix eval .#devShells.aarch64-darwin.default.drvPath --raw --impure \
  --no-write-lock-file --option allow-import-from-derivation false \
  --override-input my-nix "path:$HOME/src/scripts/my-nix"
```

These checks evaluate the shell; they do not build the application or every package.

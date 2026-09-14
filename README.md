# todo-lofi

A local-first task manager for macOS and Linux, built on [GPUI](https://github.com/nmrshll/gpui). Nested tags, an embedded AI agent pane, Todoist sync, and a keyboard-first workflow—all running offline.

![Screenshot](docs/img/Screenshot%202026-09-14%20at%2011.24.37.png)

## Features

- **Integrated AI agent** — integrated AI assistant for doing tasks.
- **Repeat & scheduling** — recurring task configuration via a dedicated picker.
- **Nested tag hierarchy** — tree-structured project/area organization with drag-and-drop reordering.
- **Tag-to-directory binding** — associate tags with local filesystem directories (for lofi git-note workflows).
<!--- **Keyboard-first** — all core actions reachable via keybindings; mouse always optional.-->
- **Nix reproducible dev environment** — `nix develop` gives you every tool, every time.
- **2-way sync with your current Todo-list app** — 
  - Todoist
  - Github issues & projects
- **Integrated mini-apps (extensions)** — 
  - Travel checklist templates
  - Message follow-ups & birthday wishes
  - Job application tracking


## Quickstart

```bash
# Enter the Nix dev shell (requires Nix with flakes)
nix develop

# Run the desktop app
cargo run -p todo-2

# (or use the watch script for auto-rebuilds)
t2
```

## Project Structure

```
todo-lofi/
├── apps/
│   └── todo-2/        # Desktop GPUI application
├── libs/
│   ├── storage/       # SQLite-backed persistence layer
│   ├── gpui-tokio/    # GPUI ↔ Tokio bridge
│   ├── ai_providers/  # LLM provider abstraction
│   ├── acp-client/    # Agent Control Protocol client
│   └── ...
├── patched/           # Vendored upstream patches
├── docs/              # Specs and agent skills
└── flake.nix          # Nix flake
```

## Contributing

Contributions welcome. Please open an issue first to discuss any non-trivial change.

1. Fork the repo and create a feature branch.
2. Run `cargo check -p todo-2` and `cargo test -p storage` before pushing.
3. Keep commits focused and rebased.

### License

[GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0).

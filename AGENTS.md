# Repository Guidelines

## Project Overview

`todo-lofi` is a Rust workspace for a local-first todo application with a GPUI-based desktop frontend. The project is in active early development — much of `libs/core` is stubbed out with `todo!()` macros pending task-backend integration.

---

## Project Structure

```
todo-lofi/
├── apps/
│   ├── desktop-gpui/       # Primary desktop app (GPUI renderer, macOS-first)
│   └── desktop-tauri/      # Experimental Tauri-based desktop app
├── libs/
│   └── core/               # Shared domain logic: Task, AppState, Operation
├── Cargo.toml              # Workspace manifest (resolver = "2", edition = "2024")
├── flake.nix               # Nix dev environment (direnv + flake-parts)
└── flake.lock
```

Workspace-level dependency versions are declared in the root `Cargo.toml` and inherited by member crates via `.workspace = true`.
Shared dependencies (duplicates across projects) should preferably go into the workspace.

---

## Build & Development Commands

Enter the nix environment first using `direnv reload` (if necessary, `nix develop`). Then:

| Command | Description |
|---|---|
| `utest` | Test current crate |
| `cargo build` | Build all workspace members |
| `cargo build -p desktop-gpui` | Build only the GPUI app |
| `cargo run -p desktop-gpui` | Run the GPUI desktop app |
| `cargo check --workspace` | Fast type-check without producing artifacts |
| `cargo test --workspace` | Run all tests across the workspace |
| `cargo clippy --workspace` | Lint all crates |
| `cargo fmt --all` | Format all source files |

---

## Coding Style & Naming Conventions

- **Edition:** Rust 2024. Use idiomatic edition-2024 patterns.
- **Formatting:** `rustfmt` with default settings — run `cargo fmt --all` before committing.
- **Linting:** `cargo clippy --workspace -- -D warnings` must pass cleanly.
- **Naming:** follow standard Rust conventions — `snake_case` for functions/variables, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- **Error handling:** use `Result`/`Option`; avoid `.unwrap()` outside tests.
- **Dependencies:** always inherit versions from the workspace manifest where possible (`dep.workspace = true`). When adding a new crate, query the latest version before pinning.

---

## Testing Guidelines

- Tests live in `#[cfg(test)]` modules inside each source file (`libs/core/src/lib.rs`, etc.).
- No integration test directory exists yet — add one under `libs/core/tests/` when needed.
- Run all tests: `cargo test --workspace`.
- Test function names should follow `test_<unit>_<scenario>` (e.g., `test_app_state_add_task`).
- Commented-out test stubs in `libs/core` should be uncommented and completed as the backend is implemented — do not delete them.

---

## Commit Guidelines

Commits in this repository use a `<scope>: <description>` format:

```
gpui: flake deps + hello-world
rust: dep versions
nix: use flake module
desktop-gpui: re-init
```

- Use the affected crate or subsystem as the scope (`gpui`, `nix`, `rust`, `core`, `tauri`).
- Keep the subject line short and lowercase.
- No pull request process is established yet — direct commits to `main` are the current workflow.

---

## Architecture 

- `libs/core` is the single source of truth for domain types (`Task`, `AppState`, `Operation`). All app crates depend on it; it must not depend on any app crate.
- `Operation` is modelled as a CRDT-friendly append-only log entry (Create / Delete / Update / UndoPoint) — keep this invariant when extending it.
- The Tauri app (`apps/desktop-tauri`) is currently secondary to the GPUI app. Avoid coupling the two frontends.

# Shepherd

Desktop shell for Shepherd: a Tauri 2 window over the SolidJS UI in `ui/`. Task
behaviour lives in `libs/shepherd-core`; this directory owns only the window, the
invoke commands and the event channel.

## Run

From the repository root, inside the dev shell (`direnv allow .` once; the shell
then loads it automatically):

```sh
pnpm install
pnpm dev:shepherd
```

`pnpm dev:shepherd` runs the Tauri CLI from this directory, which starts the Vite
dev server in `ui/` on port 1420 and then opens the desktop window. Non-interactive
runners need the wrapper: `direnv exec . pnpm dev:shepherd`.

Until the Codex adapter lands in S2, the shell runs a scripted adapter
(`src-tauri/src/dev_harness.rs`). Its messages say they are scripted; it is not a
real agent.

## Bridge

Every core command has one `#[tauri::command]`, and the window receives core
events over a single Tauri channel. `ui/src/bridge.ts` holds the typed client;
the snapshots come from `shepherd-core`, never from a second frontend model.

Design and build order: [docs/todo.md](./docs/todo.md).

# Shepherd

Desktop shell for Shepherd: a Tauri 2 window over the SolidJS UI in `ui/`. Task
behaviour lives in `libs/shepherd-core`; this directory owns only the window, the
invoke commands and the event channel.

## Run

From the repository root, inside the dev shell (`direnv allow .` once; the shell
then loads it automatically):

```sh
pnpm install
shep
```

`shep` runs `pnpm dev:shepherd`, which starts Vite on port 1420 and opens the
desktop window. Tauri relaunches the app when the shell, its local Rust
dependencies, or the frontend source and configuration change. Vite keeps
running between app restarts. Non-interactive runners can use
`direnv exec . shep`.

Set `SHEPHERD_DEV_DATA_DIR` to an absolute path when exercising the debug app
with a separate project list and database. Set `SHEPHERD_DEV_PROJECT_ROOT`
to point the debug project picker at a fixture directory.

Until the Codex adapter lands in S2, the shell runs a scripted adapter
(`src-tauri/src/dev_harness.rs`). Its messages say they are scripted; it is not a
real agent.

The native terminal smoke uses a disposable project and database and a
separate dev server on port 1432. The first launch exercises Vim, less, Kitty
key reports, scrollback search, split resizing and output bursts. The second
launch checks layout restoration and terminal restart. Run it from an active
desktop session with `vim`, `less` and `python3` on your path.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh
```

## Bridge

Every core command has one `#[tauri::command]`, and the window receives core
events over a single Tauri channel. `ui/src/bridge.ts` holds the typed client;
the snapshots come from `shepherd-core`, never from a second frontend model.

## Live Codex tests

The ignored Codex tests use the ChatGPT login with `gpt-6.1-terra` at
`low` effort. A successful run records the app-server responses under
`.cache/shepherd-live/codex/terra-low-v1/`. Later runs replay them through
the adapter, including the recorded file change where the scenario needs it.
Delete a scenario's JSON file to request a fresh live run.

```sh
direnv exec . cargo test -p agent-harness -- --ignored live_codex
direnv exec . cargo test -p shepherd-core -- --ignored live_exit
direnv exec . cargo test -p shepherd-core -- --ignored live_restart
```

Design and build order: [docs/todo.md](./docs/todo.md).

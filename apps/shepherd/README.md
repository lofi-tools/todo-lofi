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

`shep` starts Vite on port 1420 and uses `.cache/test-db/shepherd.db`. Projects
and chats in that test database persist across launches.

`shep --test` starts on port 1435 with an empty database in a fresh directory
under `.cache/test-db/`. Changes survive Tauri reloads during that launch.
Closing the app discards the session database. The next `shep --test` launch
starts empty again.

Choose the Send button color under **Settings → Appearance**. The choice is
saved between launches for `shep` and lasts for the window session in
`shep --test`.

Tauri reloads the app when the shell, its local Rust dependencies, or the frontend
changes. Vite keeps running during those reloads. Non-interactive runners can use
`direnv exec . shep` or `direnv exec . shep --test`.

Set `SHEPHERD_DEV_DATA_DIR` to an absolute path when exercising the debug app
with a separate project list and database. Set `SHEPHERD_DEV_PROJECT_ROOT`
to point the debug project picker at a fixture directory.

The native terminal smoke uses a disposable Git project, database and scripted
agent in a temporary directory under `.cache/test-db/`. The first launch saves
a Vim edit, pages through less, selects a result in full-screen fzf, and
exercises Kitty key reports, bracketed paste,
mouse reports, scrollback search, PTY resizing, output bursts and terminal
continuity while switching tasks or projects. It also checks distinct Ctrl+I and Tab
bindings when fish 4 or later is installed. The second launch checks layout
restoration and restarts a shared terminal and a task terminal in its worktree.
Run it from an active desktop session with `vim`, `less`, `fzf` and `python3`
on your path.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh
```

The smoke uses a separate dev server on port 1432. Set
`SHEPHERD_DESKTOP_SMOKE_PORT` to use another port.

On Ubuntu 24.04, the **Shepherd terminal smoke** GitHub Actions workflow runs
the same two launches under Xvfb. Dispatch it for the ref you want to verify.

## Import projects from the ChatGPT app

When the Projects list is empty, choose **ChatGPT app** under **or import all
from** to add the available primary folders in one batch. Shepherd skips missing
folders. Imported projects use the folder name in Shepherd, and agent tasks
still require a Git repository. Use the folder-plus button beside **Projects**
to add a repository individually. **Settings → Projects** offers the same
import sources when the list already contains projects.

The import uses the installed Codex app server's local project list. It works
with a Codex installation that supports `project/list`.

Shepherd launches its Codex app server with GPT-6.1 Terra (`gpt-6.1-terra`) as
the default model. The composer displays the resolved model for the project or
the model reported by the active session.

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

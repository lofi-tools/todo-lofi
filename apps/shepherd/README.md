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
agent in a temporary directory under `.cache/test-db/`. It requires `python3`.
The full suite also requires `vim`, `less` and `fzf`.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh
```

On macOS the default smoke and the `--codex-*` modes launch a hidden window
without activating Shepherd. The terminal smoke checks shell input and output,
bracketed paste, and terminal continuity while switching tasks or projects.
The second launch checks the saved split and inspector tab,
restarts shared and task terminals, and resumes the interrupted task.

Use the macOS window tests when the change affects the behavior they exercise.

- `--macos-window` runs the full native suite for changes to window layout,
  terminal focus, keyboard shortcuts, mouse interaction, resizing, search
  highlights, clipboard selection or canvas painting. It also drives Vim,
  less and fzf. Keep its window visible during the run.
- `--macos-visibility` checks PTY output and painting across a real minimize
  and restore. Use it for changes to window or webview visibility handling.
- `--macos-ime` checks native candidate selection and placement with Pinyin.
  Use `--macos-ime british` for the British layout's dead keys and Option input.

The smoke uses a separate dev server on port 1432. Set
`SHEPHERD_DESKTOP_SMOKE_PORT` to use another port.

On Ubuntu 24.04, the **Shepherd terminal smoke** GitHub Actions workflow runs
the full two-launch suite under Xvfb. A push to `codex/shepherd-terminal-smoke`
starts the run. Once the workflow is on the default branch, you can also
dispatch it for a published ref.

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
When the installed CLI only exposes `exec --json`, Shepherd uses that
protocol and starts a separate Codex process for each prompt.

## Bridge

Every core command has one `#[tauri::command]`, and the window receives core
events over a single Tauri channel. `ui/src/bridge.ts` holds the typed client;
the snapshots come from `shepherd-core`, never from a second frontend model.

## Attachment limits

Shepherd allows up to 50 MB per attachment by default. Set
`attachments.max_file_bytes` to a positive whole number of bytes in
Shepherd's `settings.json`. Put it under `app` to change the default or under
`projects` and the project's absolute path to override that project. The same
limit applies to composer files and preview screenshots. Restart Shepherd to
load changes.

```json
{
  "version": 1,
  "app": { "attachments.max_file_bytes": 50000000 },
  "projects": {
    "/Users/me/src/example": { "attachments.max_file_bytes": 20000000 }
  }
}
```

## Use an installed OpenCode

In Shepherd's `settings.json`, set `agents.opencode.executable` under `app`
to the absolute path of your existing OpenCode executable. On macOS the file
is `~/Library/Application Support/dev.shepherd.shell/settings.json`. On Linux
it is `dev.shepherd.shell/settings.json` under `$XDG_CONFIG_HOME` (by default
`~/.config`). If the file already exists, add the key to its `app` object.

```json
{
  "version": 1,
  "app": {
    "agents.opencode.executable": "/etc/profiles/per-user/me/bin/opencode"
  }
}
```

Restart Shepherd, then select **OpenCode** from the agent picker in an empty
conversation or a draft. Shepherd probes OpenCode's native 2.x HTTP API and
falls back to ACP with the same executable when the native probe fails.
The task keeps its agent after the first prompt. The diagnostics show which
protocol ran and why Shepherd selected the fallback.
The conversation header shows the tier after a session opens. Diagnostics list
the command, protocol, supported features and recent stderr. Turn actions
follow the capabilities the adapter reported.

To run the opt-in smoke tests, set `SHEPHERD_OPENCODE_TEST_SETTINGS` to the
absolute path of a settings file with that key. The tests use a disposable
OpenCode profile. The native turn smoke uses a loopback model fixture, and
the installed ACP fallback smoke checks session creation and reload.

```sh
direnv exec . cargo test -p agent-harness opencode::tests::installed_ -- --ignored
```

## Use an installed Claude Code ACP adapter

In the same `settings.json`, set `agents.claude.executable` under `app` to the
absolute path of your Claude Code ACP executable. The optional
`agents.claude.args` array supplies arguments as separate values.

```json
{
  "version": 1,
  "app": {
    "agents.claude.executable": "/path/to/claude-code-acp",
    "agents.claude.args": []
  }
}
```

Restart Shepherd and select **Claude Code** in an empty conversation or a
draft. The ACP agent selects its model and sends permission requests to
Shepherd. ACP approval cards offer **Allow once** when supported and
**Allow for session** when the request identifies the operation and its input.
Shepherd reuses the latter for identical requests while the ACP session is
connected, and labels those decisions in the transcript.
**Always allow for project** saves a matching grant for the configured ACP
agent in that project. Open **Settings → Projects** to review or reset saved
grants and live session grants for the selected project. Each live grant
shows its task and expires when its ACP connection closes.
When an ACP agent opens a terminal, its **agent:** pane appears in the
selected task's Terminals tab. You can type into it, resize it, hide it, or
stop the process. Shepherd closes unreleased terminals with their ACP
session.

## Run the installed-agent smokes

The **Shepherd adapter smoke** GitHub Actions workflow runs on demand from
the default branch on a self-hosted Nix runner labelled `shepherd-adapters`.
Choose one preinstalled agent when dispatching it. Set the repository variable
`SHEPHERD_ADAPTER_TEST_SETTINGS` to an absolute settings file path on the runner
for OpenCode or Claude Code ACP. For Pi, set `SHEPHERD_PI_TEST_EXECUTABLE` to
the absolute path of the existing executable.

For a local Claude ACP smoke, set `SHEPHERD_CLAUDE_TEST_SETTINGS` to a settings
file with the configured command. The smoke initializes ACP and opens a
session in a disposable project and profile without sending a model prompt.
The Pi smoke checks RPC framing and a local extension dialog. Pi versions
without prompt disposition can pass this dialog probe.

```sh
direnv exec . cargo test -p agent-harness --lib acp::tests::installed_claude_code_opens_an_acp_session -- --ignored
direnv exec . cargo test -p agent-harness --lib pi::tests::installed_pi_exchanges_extension_dialog_records -- --ignored
```

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

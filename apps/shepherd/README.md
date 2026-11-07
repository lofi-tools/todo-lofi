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

Enable **Notify when tasks need input** in **Settings → Workspace** to receive
system notifications when an agent needs input outside the focused task. On
macOS, enable it from the bundled Shepherd app and grant notification
permission. The status bar shows running and review totals across projects.
Click the review count to filter the sidebar and archive selected review
tasks together.

Open the **Command palette** from the icon rail or with Command-K on macOS
and Ctrl-Shift-K on Linux. Search by command name or category. The palette
shows each shortcut and explains unavailable actions.

Tauri reloads the app when the shell, its local Rust dependencies, or the frontend
changes. Vite keeps running during those reloads. Non-interactive runners can use
`direnv exec . shep` or `direnv exec . shep --test`.

Set `SHEPHERD_DEV_DATA_DIR` to an absolute path when exercising the debug app
with a separate project list and database. Set `SHEPHERD_DEV_PROJECT_ROOT`
to point the debug project picker at a fixture directory.

Runtime error toasts link to **Error history** in the status bar. The structured
log is `logs/errors.jsonl` in the active data directory (normally
`.cache/test-db/logs/errors.jsonl` for `shep`). Use `tail -F` on that file to
follow new diagnostics; the `shepherd-debug` repository skill describes the
investigation workflow.

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

## App icon

The [Shepherd logo](ui/public/assets/shepherd-logo.svg) is the source for the
PNG and Windows icons. macOS uses the [borderless white mark](ui/public/assets/shepherd-logo-white.svg).
Its silhouette fits the central 824 × 824 area of the 1024-pixel canvas, with
transparency around the shape.

Regenerate the desktop files after editing the SVG:

```sh
icon_output=$(mktemp -d)
pnpm --dir apps/shepherd tauri icon "$PWD/apps/shepherd/ui/public/assets/shepherd-logo.svg" --output "$icon_output"
cp "$icon_output"/*.{png,ico} apps/shepherd/src-tauri/icons/
pnpm --dir apps/shepherd tauri icon "$PWD/apps/shepherd/ui/public/assets/shepherd-logo-white.svg" --output "$icon_output"
cp "$icon_output/icon.icns" apps/shepherd/src-tauri/icons/
```

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

Shepherd allows up to 50 MB per attachment by default. Change the default in
**Settings → Workspace**, or set a project override in **Settings → Projects**.
The same limit applies to composer files and preview screenshots. To edit
`settings.json` directly, put `attachments.max_file_bytes` under `app` or under
`projects` and the project's absolute path. Choose **Reload settings** in
Workspace to apply a file edit.

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
Set `agents.opencode.config_file` to the absolute path of an existing OpenCode
configuration file when you want Shepherd to select it explicitly.

```json
{
  "version": 1,
  "app": {
    "agents.opencode.executable": "/etc/profiles/per-user/me/bin/opencode",
    "agents.opencode.config_file": "/Users/me/.config/opencode/opencode.json"
  }
}
```

Restart Shepherd, then select **OpenCode** from the agent picker in an empty
conversation or a draft. Shepherd probes OpenCode's native 2.x HTTP API and
falls back to ACP with the same executable when the native probe fails.
Shepherd passes the optional file as `OPENCODE_CONFIG` for both protocols and
for project mode discovery. With the setting omitted, OpenCode uses its usual
configuration lookup.

Agent task launches also accept `agents.codex.environment`,
`agents.opencode.environment`, and `agents.claude.environment` objects in
`settings.json`. Each maps variable names to strings. A project entry under
`projects` can override the entire map for that agent. Values starting with
`!` run as shell commands from the project root when a session starts; their
output is passed to the agent without being saved. Commands time out after
five seconds. For example:

```json
{
  "version": 1,
  "app": {
    "agents.codex.environment": { "EXAMPLE_TOKEN": "!cat .local-token" }
  }
}
```

Restart Shepherd after changing these settings. Model and mode discovery do
not yet use these environment maps.
The task keeps its agent after the first prompt. The diagnostics show which
protocol ran and why Shepherd selected the fallback.
The conversation header shows the tier after a session opens. Diagnostics list
the command, protocol, supported features and recent stderr. Turn actions
follow the capabilities the adapter reported.

To run the opt-in smoke tests, set `SHEPHERD_OPENCODE_TEST_SETTINGS` to the
absolute path of a settings file with the executable key. The tests use a disposable
OpenCode profile. The native turn smoke uses a loopback model fixture, and
the installed ACP fallback smoke checks session creation and reload.

```sh
direnv exec . cargo test -p agent-harness --features live-opencode opencode::tests::installed_
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
The installed-process probes are excluded by default. Each requires the
feature for that agent because a replay would skip the process being tested.

```sh
direnv exec . cargo test -p agent-harness --features live-claude --lib acp::tests::installed_claude_code_opens_an_acp_session
direnv exec . cargo test -p agent-harness --features live-pi --lib pi::tests::installed_pi_exchanges_extension_dialog_records
```

## Live Codex tests

Codex scenarios replay the app-server responses and recorded worktree edits
from `.cache/shepherd-live/codex/terra-low-v1/`. To record a missing scenario,
add `--features live-codex` to the Cargo command or `--record` to the desktop
smoke command. Recording uses the ChatGPT login with `gpt-6.1-terra` at `low`
effort. An existing capture takes precedence when recording is enabled.

```sh
direnv exec . cargo test -p agent-harness -- --ignored live_codex
direnv exec . cargo test -p shepherd-core -- --ignored live_exit
direnv exec . cargo test -p shepherd-core -- --ignored live_restart
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-review-merge
```

Design and build order: [docs/todo.md](./docs/todo.md).

# herdr

Terminal-based agent runtime and workspace manager: "the runtime your coding
agents live on". A single Rust binary that runs in whatever terminal you already
use, with no Electron or bundled browser.

- Checkout: `.cache/checkout/herdr` at `ee5bfb5041bb3e4a49d591c72308aae3c81ecdde`
- Version: `herdr 0.9.5` (`Cargo.toml`)
- Upstream: <https://github.com/herdrdev/herdr>
- License: Apache-2.0

This is the shipped "Herdr 1" TUI. The sibling repositories `herdr-core` and
`herdr-app` are a separate v2 split of the same problem; they do not share this
codebase or its APIs.

## Product shape

Herdr is a client/server terminal multiplexer aimed at coding agents rather than
at shells alone. A background server owns the PTYs; one or more TUI clients
attach to it and draw. Closing the client, losing SSH, or detaching leaves the
server and every terminal running. Agents are first-class: Herdr recognises the
CLI running in a pane, reports whether it is working, blocked or idle, and
exposes all of it through a CLI and a JSON socket API so agents can drive each
other.

Source map: `src/main.rs`, `src/app/` (state, actions, input), `src/client/`
(attach, transport, frames), `src/api/` (server protocol, events, schema),
`src/detect/` (agent detection), `src/integration/`, `src/ui/` (sidebar, panes,
status), `src/platform/` (per-OS), `src/cli/`.

## Features

### Persistent sessions and restore

- Detach with the prefix chord (`ctrl+b q`) and reattach by running `herdr`;
  terminals keep running in the background server.
- Layout (workspaces, tabs, panes) is persisted and restored after a server or
  machine restart. Restored panes start fresh processes; the original child
  processes do not survive.
- Supported agents have their own conversation state resumed on restore
  (`src/app/agent_resume.rs`, `src/agent_resume.rs`).
- Session saves are written to disk before replacing the old file, so a crash or
  power cut cannot leave an unreadable save.
- Multiple sessions can coexist (`herdr session list|attach|stop|delete`), and a
  server can host several attached clients with independent focus/view state.
- On Linux and macOS closing the launching shell no longer stops the server
  (`herdr server stop` or `SIGTERM` instead of `SIGHUP`).

### Workspaces, tabs and panes

- Workspace → tab → split-pane hierarchy, all addressable by number and name.
- Pane splitting with mouse drag or keys; pane zoom, focus, move and swap.
- Workspace and tab creation, closing, renaming, reordering and navigation.
- Worktree-aware workspaces: `herdr worktree` creates and opens Git worktrees,
  and worktree groups survive checkouts that are temporarily unavailable
  (`src/app/worktrees.rs`, `src/worktree.rs`).
- Copy mode for scrollback selection and copy, mouse selection, scrollbars, and
  pane search.

### Agent awareness

- Every pane is classified as working, blocked (an approval or question dialog),
  idle or done; the sidebar aggregates agents across workspaces and machines.
- Detection is manifest-driven screen inspection, not a hard-coded list: 22
  bundled manifests in `src/detect/manifests/` (Claude Code, Codex, Cursor,
  Gemini, OpenCode, Copilot CLI, Devin, Droid, Amp, Antigravity, Cline, Grok,
  Hermes, Kilo, Kimi, Kiro, Letta, Maki, Muse, Pi, QoderCLI, Qwen).
- Manifests are hot-reloadable per user (`~/.config/herdr/agent-detection/`) so
  detection rules can be fixed without a release; `herdr server
  reload-agent-manifests` and `herdr agent explain <pane> --json` support
  debugging a rule against a live pane.
- Detection reads a screen snapshot only and never touches the parser or the
  user-visible viewport (users can scroll it).
- Agent identity (a name bound to the pane occupant) survives suspend/resume and
  is cleared when the agent exits or is replaced.
- Status semantics are exposed to the CLI/API with a server-side "seen" state so
  a Done badge can differ between clients.

### Terminal engine

- `portable-pty` on the server with a vendored Ghostty VT parser
  (`crates/ghostty-vt`), patched `portable-pty` and a raw-input layer.
- Kitty graphics protocol with image files, clipboard forwarding and clipboard
  images, terminal notifications and OSC handling, theme sync, and terminal
  effects.
- Enhanced keyboard handling: Kitty keyboard protocol, key releases, IME and
  non-Latin layouts, Ctrl+Shift translation for plain shells, AltGr and accent
  keys.
- Mouse reporting including pixel coordinates, plus link handling that gives
  plugins first refusal when mouse capture is on.
- Copy/paste, bracketed paste, paste-image, search, scroll and selection.

### Navigation and UI

- tmux-style prefix keys and mouse-first interaction (click, drag, split) as
  equal citizens.
- Sidebar of agents and panes with collapse, width persistence, sorting, and
  status tokens; the layout is a TUI client concern, not server state.
- Dialogs, onboarding, settings, command palette, Go To search, help and
  keybinding help reuse a shared UI language.
- Themes and sounds (`src/terminal_theme.rs`, `src/sound.rs`), per-terminal
  appearance, window title updates, status bar, scrollbar.
- Release notes and product announcements rendered in-app
  (`src/release_notes.rs`, `src/product_announcements.rs`).
- Popups/floating panes, custom commands, and configurable keybindings.

### Remote machines

- Saved SSH machines, each with its own Herdr server, terminals and layout.
- Local and remote workspaces appear in one window and one combined agent list,
  with independent reconnects per machine.
- Remote bridge and SSH agent forwarding (`src/remote.rs`,
  `src/platform/remote_bridge.rs`), remote file reads and image transfer.
- Remote layout/sidebar state survives reconnects.

### Agent-facing control surface

Herdr ships an agent skill (`skills/herdr/SKILL.md`, gated on `HERDR_ENV=1`) and
these CLI groups, all returning JSON for scripting:

| Group | Purpose |
| --- | --- |
| `herdr workspace` | create, list, select, rename, close, next/prev |
| `herdr tab` | create, list, select, move, close |
| `herdr pane` | split, list, send keys/text, read output, resize, close, move |
| `herdr agent` | start, send, read (including `--source detection`), wait, explain, list |
| `herdr terminal` | attach, per-terminal sessions (control/observe) |
| `herdr worktree` | create/open/remove Git worktrees |
| `herdr notification` | raise and route notifications |
| `herdr integration` | install/uninstall/status for agent hooks |
| `herdr session` | list, attach, stop, delete sessions |
| `herdr machine` | add/list/rename/remove SSH machines |
| `herdr server` | stop, reload config, agent-manifest management |
| `herdr api` | `snapshot` (live session JSON) and `schema` |
| `herdr plugin` | list, install, update, link |
| `herdr config`, `herdr channel`, `herdr update`, `herdr status` | diagnostics and self-management |

The socket API (`src/api/`) is a JSON-RPC-style protocol with an event hub,
subscriptions and wait primitives; the schema is generated from Rust types and
published as JSON (`docs/next/api/herdr-api.schema.json`). The architecture is
being migrated toward a server-owned runtime protocol with the TUI as one
client; new shared state is required to go through the API rather than a private
TUI socket.

### Integrations

`herdr integration install|uninstall|status` edits the config of supported
agents (Claude Code settings, Codex, OpenCode) so they report state and turn
boundaries to Herdr instead of relying on screen scraping alone. It tracks the
installed version and can be re-run after agent updates.

### Plugins

- Plugins extend panes and workflows and are distributed through a marketplace.
- `herdr plugin install`, `update` (all or by name, from GitHub), and links for
  local development; running plugin panes keep working on their old files while
  new runs pick up an update.
- Plugins can handle terminal links and get first pick when mouse capture is on.
- Plugin panes run programs, so they are trusted code, not a sandbox.

### Platform and distribution

- Linux, macOS (Apple silicon and Intel), Windows x64/ARM64 (beta, including
  endpoint-protected environments); static Linux builds; Homebrew, mise, shell
  and PowerShell installers.
- Self-update with stable/preview channels and checksum verification
  (`src/update.rs`, `src/checksum.rs`).
- Sound and desktop notifications per OS; Windows notifications stay in the
  notification centre and focus the pane.
- OS-specific behaviour is isolated in `src/platform/<os>.rs`, keeping the rest
  of the code free of `#[cfg(target_os)]`.

### Engineering notes worth knowing

- State is separated from runtime: `AppState`/`PaneState` are pure data testable
  without PTYs, `PaneRuntime` owns the process; `compute_view()` mutates
  geometry while `render()` is pure drawing.
- Performance is treated as multiplicative in panes × tabs × workspaces ×
  clients; `just bench-render-scale` and `just bench-release-smoke` guard it.
- Render profiling (`src/render_prof.rs`), terminal-effect and detection test
  harnesses exist for regression work.
- Development: `cargo build --release`, `just test`, `just check`; Windows lint
  and checks in `.github/workflows/ci.yml`.

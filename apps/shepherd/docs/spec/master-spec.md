# Shepherd — master specification

Status: draft, revised after the product interview of 2026-11-04.
Product goal: [goal.md](./goal.md).
Reference evidence: [`../references/`](../references/README.md).
Workspace UI decisions: [workspace-ui.md](./workspace-ui.md).

Shepherd is a desktop app that multiplexes coding agents and terminals around
persistent local projects, and reviews what they produce — files, diffs,
commits — in the same window. Its Rust core owns every agent, PTY, terminal and
database write, so quitting the app ends the work and the UI never claims
otherwise. v1 draws that core with a Tauri 2 + SolidJS shell; v2 replaces the
shell with GPUI. The stack is specified in [stack.md](./stack.md).

## 0. Decisions from the interview

These were chosen explicitly and override the pre-interview draft.

| Area | Decision |
| --- | --- |
| Primary object | Durable **tasks**, each with its own workspace. Every agent run is a record with state, transcript, worktree and review data; terminals and diffs are views over it. |
| Layout | **Fixed task workspace**: a required chat region in the centre and an always-on inspector on the right. No pane moves between them. The inspector has one Terminals tab, Preview, Review, Git and Files; terminals split only inside the Terminals tab. Switching tasks swaps the chat and task-specific content; the inspector tab and project-shared terminals stay mounted. |
| Navigation | Expandable project list with a **flat task list** under the selected project. Switching projects collapses the previous list. Each project row has a folder icon, a context menu and a compose button. |
| Inspector | **Always-on right inspector** with a horizontal pane switcher in the header; resizable, never closed or torn out. |
| Attention | Task rows and inspector tabs use rings, dots and text badges (`needs input`, `review`), with status-bar counts and the dock badge. |
| Task browser | Sidebar list only; no board or separate review view in v1. |
| Terminals | **One Terminals tab per task workspace**, containing a project-saved arrangement of terminal panes. Slots are project-shared or task-specific; there is no global terminal list. |
| Preview | **Browser preview with element annotation**: select an element, attach a note, and add the annotation to the composer. Pages that block injection fall back to screenshot notes. |
| Density | Dense shell chrome; readable transcript measure and line height. |
| Process model | **One application, no daemon**: the Rust core owns agents, terminals and task state; shells are renderers over its command/event API. No socket, no attach protocol, no listening port. |
| Quit | Quitting the window ends agents and PTYs, after a confirmation when a task is working or waiting. Tasks are marked interrupted and offer resume next launch; terminals come back as ended. |
| Windows | **One window only**, ever. It lists every project and is already the multiplexer. |
| Agent integration order | **Native adapter first**, then ACP, then a terminal wrapper, then a plain pane. First native adapters: **Codex**, then **OpenCode**, then **Pi**. |
| Pi protocol | **JSON-line RPC** (not ACP): `pi --mode rpc` is Pi's documented subprocess interface, with run lifecycle, commands and extension UI. Pi documents no ACP mode. |
| Freebuff | A standalone PTY runs `freebuff` directly and replaces the conversation pane while open. |
| Task isolation | **One Git worktree per task** by default, at `<git-dir>/shepherd/worktrees/<slug>`. A shared checkout is opt-in with a warning. |
| Shared core | `libs/shepherd-core`, UI-agnostic: agents, terminals, state, Git, store. No Tauri or GPUI types; split out in v1 so v2 reuses it unchanged. |
| App shell (v1) | **Tauri 2** with a **SolidJS + TypeScript** frontend on the system webview; terminals paint on a canvas. |
| App shell (v2) | **GPUI + gpui-component (gpui-kit) + gpui-kit-assets**, same core, same frame model. |
| v2 timing | v2 is planned but unscheduled: work starts when v1 is feature complete, not on a calendar date. |
| v1 UI budget | **Thin and functional**; polish that v2 would replace is deferred, so the frontend stays view code over the core. |
| v2 parity | The first usable v2 build covers every v1 flow and carries keybindings, settings, transcripts and search history. Theme and accessibility parity and the platform floor may lag and are tracked as debt. |
| Commands | The core owns command IDs, labels, default chords, focus and chord resolution; shells normalize input and render palette metadata. |
| Palette scope | App commands only; agent slash commands stay in the composer. |
| Keybindings | One chord per command; app-scoped overrides, conflicts rejected. |
| Transcript identity | Core assigns per-session sequence and canonical item IDs; adapter IDs are mapped for updates. |
| Updates | None in v1. |
| Terminal keys | Only explicit app chords are claimed while a terminal has focus; every other key goes to the PTY. |
| ACP client | `libs/acp-client` is the ACP protocol crate with no GPUI dependency; transcript types move to `libs/agent-harness`. |
| Terminal backend | **`libghostty-vt`** (Rust bindings) for parsing, state and input encoding, **`portable-pty`** for PTYs, behind one `TerminalEngine` boundary. `alacritty_terminal` is the fallback engine. |
| Store | **Turso** (in-process, SQLite-compatible) through `toasty` + `toasty-driver-turso`, with numbered migrations embedded in the binary. |
| Approvals | Auto-allow reads and safe commands; other calls are judged by **the agent's own review mode**; if judgment is unavailable, ask the user. Projects may tighten this policy but never loosen it. |
| Model control | Only models, modes and effort the agent advertises. Shepherd stores no provider keys. |
| Git | Status, diff, stage, commit, branch, squash-merge into the base. No push, pull, rebase or PR surface. |
| Files | Read-only viewer, open in external editor. |
| Chat surface (v1) | Attachments and images, cross-task search, model/mode pickers, notifications, `@file` mentions and slash commands. |
| Task lifecycle | A completed turn whose HEAD, branch or worktree status/diff changed since turn start moves the task to **needs review**; a net-zero or chat-only turn returns to **ready**. The user archives it in one action or sends a review note. |
| Review surface | No separate queue or inbox: task rows and the Review tab carry **review** badges, and the status-bar count plus dock badge show how many are waiting. |
| Review batch | The task list supports multi-select and **Archive selected**; review notes stay per task. |
| Archive | Archiving keeps the branch; an archived worktree unused for 30 days is removed automatically, dirty ones are skipped, and reopening recreates it from the branch or recorded tip. |
| Review UI | Approval cards show the tool, its arguments and the affected diff inline; approvals waiting on the same task can be approved together. |
| Settings | Global defaults with per-project overrides, stored in versioned JSON files in the app config directory; keybindings are edited in-app with validation. |
| v1 rendering | Diffs with `@pierre/diffs` and a Shiki worker; transcripts with `marked` + `DOMPurify` + `KaTeX`. |
| Accessibility | The canvas terminal keeps an off-screen text mirror for assistive tech and a keyboard copy mode. |
| Attachments | Copied into a per-task cache, capped at 50 MB per file (project override allowed), and deleted with the task. |
| Subagents | Shown only when the child owns a terminal; not tracked as tasks. |
| Resume | If an adapter cannot resume, the task says so and offers transcript replay with a new session instead of pretending to continue. |
| CLI | None in v1. |
| Remote | Local directories only. |
| Platforms | **macOS 13+** (Ventura) and **Ubuntu 24.04+** with WebKitGTK 2.44+. Windows is later. |
| Teams | No agent orchestration in v1. |
| Customization | Built-in themes only in v1; rebindable keys; no programmable UI. |
| Secrets | No secret values are stored; environment values may be `!command` substitutions resolved at spawn and never persisted. |

The reference reading behind these choices: pandacode for tasks, worktrees,
native harness protocols and review surfaces; herdr-core and herdr-app for
PTY ownership, task workspaces and screen-detection manifests for terminal agents.

## 1. Terminology

| Term | Meaning |
| --- | --- |
| **Project** | A local directory the user added, usually a Git repository. Persisted by path; owns tasks, their workspaces and the project terminal layout. |
| **Task** | One conversation with one agent, in its own worktree, with state, transcript, workspace and output. The durable unit of work. |
| **Agent** | An installed CLI that Shepherd starts as a subprocess in one of the adapter tiers (§4.1). |
| **Adapter** | The code that speaks one agent's protocol and maps it onto Shepherd's transcript, approval and state model. |
| **Agent profile** | A registry entry: label, command, args, env, adapter tier, optional terminal manifest. |
| **Terminal** | A PTY-backed process owned by the app; scoped to one task or shared across a project. |
| **Pane** | A tab or split inside a task workspace: a terminal, preview, review, Git or files view. Chat is a fixed region, not an inspector tab. |
| **Task workspace** | The fixed chat region plus the always-on inspector for one task; the terminal arrangement is saved per project, not per task. |
| **Inspector** | The always-on right region that hosts the Terminals, Preview, Review, Git and Files tabs. |
| **Pane switcher** | The icon tabs in the header above the inspector. |
| **Project terminal layout** | The saved arrangement of terminal panes in the Terminals tab, shared by every task in a project. Each slot is project-shared or task-specific. |
| **Shared terminal** | A terminal instance owned by the project and visible in every task's Terminals tab. |
| **Task terminal** | A terminal instance owned by one task and shown only while that task is active. |
| **Unfinished** | Any task outside the `done` and `cancelled` states. |
| **Review** | The state after a completed turn whose HEAD, branch or worktree status/diff changed since turn start, before a human archives the task or sends a note. |
| **Session** | One agent-side conversation inside a task, identified by the adapter's session or thread ID. |
| **Turn** | One user prompt plus everything the agent streams until it is idle again. |

## 2. Experience

### 2.1 Window shell

The header and left icon rail sit outside a shared rounded surface containing
the Projects sidebar, conversation, and inspector. The status bar sits below.
The [workspace UI decisions](./workspace-ui.md) govern this layout and the
detailed treatment of its controls as the rest of the shell is built.

- The sidebar shows task rows for the selected project when it has more than
  one visible task. A sole visible task opens directly in the chat pane.
  Each project row has a folder icon
  with a branch badge for Git working trees. Its compose button opens a draft
  in that project. Hovering over a project highlights the row and opens a
  context card directly to its right. Right-click or Shift+F10 opens the same
  menu. The card shows the folder path with the home directory abbreviated as
  `~/`. It also shows the Git remote when available. The **Show archived**
  switch includes that project's `done` tasks.
  **Project settings** opens settings for that project.
  Task rows show state and attention using icons and badges for working
  (spinner), waiting (ring and dot), review (badge), failed (cross), interrupted
  (pause), draft (outline), ready (dim).
- The chat takes the remaining width between the sidebar and inspector. In a
  project with no visible tasks, typing a prompt creates a draft. Sending the
  prompt starts the agent.
- Right: the always-on inspector with its icon tabs in the header. Tabs are
  **Terminals**, **Preview**, **Review**, **Git** and **Files**. The Terminals tab
  contains the project's saved terminal arrangement; shared terminals keep their
  PTY across task switches and task terminals belong to the active task.
- Regions are fixed: panes are resized inside their region but never dragged
  between chat, inspector and terminal splits. Terminal panes split only inside
  the Terminals tab.
- Bottom: status bar — agent activity, running task count, review count, branch,
  change count. No connection indicator: there is nothing to connect to.
- Density: compact sidebar, toolbar, tab strip and status bar; the transcript
  keeps a readable measure and line height.
- The window title shows the active project and a working-count suffix, so the
  dock/task switcher reflects running work.

### 2.2 Task workspace model

- A task owns one workspace: its chat, chat scroll, focus and region widths. The
  inspector's selected tab is project-level, so switching tasks keeps the
  Terminals tab and its shared terminals in place; task-specific slots swap to the
  active task. The project remembers its last active task.
- Regions are fixed. The middle region shows the task's chat, Settings, or
  the standalone Freebuff terminal. The inspector is always present, and
  terminal splits live inside its Terminals tab.
- Inspector tabs: **Terminals**, **Preview**, **Review**, **Git**, **Files**.
  Each tab has one pane except Terminals, which holds a split tree of terminal
  panes. Diff views live in Review and Git; there is no standalone diff tab.
- The **Terminals** tab stores one arrangement per project, not per task: a split
  tree with sizes, slot order and terminal scope. A **project-shared** slot shows
  the same PTY in every task of that project; a **task-specific** slot shows the
  active task's PTY, or a create/empty state when that task has none.
- Task terminals keep running while another task is active and reappear when
  their task is selected. Shared terminals keep running across every task in the
  project. Closing a terminal view detaches only the view; the PTY keeps running
  and can be reopened from the Terminals tab's instance menu.
- Terminal layout presets (single, side by side, stacked, quad) are per-project
  starting points a user can apply and then edit; they are shortcuts inside the
  Terminals tab. Single keeps the active pane visible and hides the other
  visible panes while their terminals keep running. Quad arranges four
  visible panes in two rows. Both keep the slots of other tasks.
- Restore validates the task workspace and the project terminal layout: a pane
  whose terminal or file no longer exists becomes a placeholder with a reason,
  and the rest is kept. If the last active task is gone, landing opens a new
  draft, never an older task.
- The sidebar list is the only task browser; there is no board or separate review
  view in v1.

### 2.3 Navigation

- `⌘K` / `Ctrl Shift K` command palette: every command, fuzzy search, current
  binding shown; labels, categories, bindings and disabled reasons come from the
  core registry. The palette lists app commands only; agent slash commands stay
  in the composer.
- **Send next key to terminal** in the palette forwards the next chord to the
  focused PTY without resolving it, for keys the terminal needs but the registry
  claims.
- `⌘P` / `Ctrl Shift P` project switcher, restoring that project's last task
  workspace; `⌘O` / `Ctrl Shift O` task switcher, which swaps the chat and
  task-specific terminal slots; the inspector tab stays where it is.
- `⌘\`` / `Ctrl \`` focus next pane; `⌘⇧\`` / `Ctrl Shift \`` focus previous.
  Left and Right switch inspector tabs. Down from the Terminals tab focuses
  the first live split's header. Arrow keys on a live split's header move
  focus to the adjacent split or back to the Terminals tab above the splits.
  With the terminal input focused, arrow keys go to the PTY.
- `⌘T` / `Ctrl Shift T` new task; `⌘⇧T` / `Ctrl Shift N` new task terminal in the
  active task; **New shared terminal** in the palette creates a project-scoped
  terminal.
- `⌘B` / `Ctrl Shift B` toggle sidebar; `⌘⇧D` / `Ctrl Shift D` and `⌘⇧E` /
  `Ctrl Shift E` split the focused terminal right / down inside the Terminals
  tab; `⌘W` / `Ctrl Shift W` closes the focused terminal view and is disabled on
  other tabs.
- `⌘.` / `Ctrl .` cancel the focused task's turn; `⌘Enter` / `Ctrl Enter` sends
  the composer.
- Palette commands cover terminal presets, Git actions, adapter checks and
  diagnostics, so nothing is mouse-only.
- Cross-task search lists results with a highlighted snippet; choosing one opens
  that task's workspace, focuses the chat, scrolls to the entry and highlights the
  match there.
- The core owns focus and input mode (chat, inspector tab, terminal split,
  composer, palette, modal), so key events carry no UI context. While a terminal
  has focus, only chords in the registry are claimed; everything else is encoded
  for the PTY.
- The inspector's pane switcher is keyboard reachable; focus next/previous
  includes its tabs and terminal splits, and choosing a tab does not close the
  chat.
- Attention rings, dots and badges appear on task rows and inspector tabs: a ring
  means waiting, a dot means unread output, and a text badge names `needs input`
  or `review`. Chat has no tab, so its attention stays on the task row and
  composer; shared-terminal attention stays on the Terminals tab.
- The status-bar review count filters the sidebar task list to tasks awaiting
  review. Multi-select (shift/⌘-click on macOS, shift/Ctrl-click on Linux) plus
  **Archive selected** archives several at once; there is no separate review view
  or inbox.

### 2.4 First run

1. The empty Projects sidebar offers **New project** and one batch import
   button per source. The header's folder-plus button also opens the directory
   picker. See the [sidebar and picker decisions](./workspace-ui.md#projects-sidebar-and-settings).
2. Selecting a project with no visible tasks opens its empty conversation with
   the composer ready for input. Typing creates a draft, and sending the first
   prompt starts the agent. The inspector hosts the project's Terminals tab.
3. If an agent is missing, the chat explains the install command per adapter and
   offers a terminal in the Terminals tab as the working alternative.

### 2.5 Quit

- If any task is working or waiting, `⌘Q` shows a confirmation naming what will
  stop: "Quit? 2 agents and 3 terminals will stop." Idle terminals need no
  confirmation: they end with the app, and the prompt exists to protect live work.
- On quit, confirmed or not: cancel turns where supported, close agent stdin,
  send `SIGTERM` to each agent process group, wait up to 2 s, then `SIGKILL`;
  close PTYs; mark working tasks **interrupted**; flush the database.
- Next launch: task workspaces and tasks restore, interrupted tasks offer **Resume**,
  terminals appear as **ended** with their last cwd and command and a restart
  action. Nothing claims to have survived that did not.
- If the app crashes instead of quitting, the same interrupted state is applied on
  next launch, and leftover agent PIDs recorded with their start time are offered
  for termination. A reused PID is not killed.

## 3. Architecture

### 3.1 Processes and threads

```
shepherd-core (one Rust process, one tokio runtime)
├── harness adapters (stdio, RPC, ACP, PTY wrapper)
├── terminal host: one emulation thread per terminal (libghostty-vt)
├── PTY reader/writer tasks (portable-pty)
├── git commands and worktree operations
├── filesystem watchers
├── Turso writer task
└── command in / event out bus

shell (v1: Tauri webview + SolidJS; v2: GPUI window)
├── sends commands, key events, paste and resize
└── draws transcripts, panes, diffs and terminal frames
```

No daemon, no socket, no second binary. The core is a library linked into the
shell; the v1 webview is the operating system's component, not a second Shepherd
process. Closing the window is quitting the app; §2.5 defines the shutdown
sequence. A crash leaves at most orphaned agent processes, which the next launch
detects and offers to stop.

### 3.2 Crates and modules

| Location | Responsibility |
| --- | --- |
| `libs/shepherd-core/src/shepherd_core.rs` | library root (`[lib] path`): `Core` handle, command/event bus, lifecycle, shutdown |
| `libs/shepherd-core/src/projects.rs` | project registry, Git metadata, path validation |
| `libs/shepherd-core/src/tasks.rs` | task records, queue, state machine, orphan cleanup |
| `libs/shepherd-core/src/input.rs` | focus and input mode, chord resolution, terminal key routing |
| `libs/agent-harness/src/agent_harness.rs` | library root: `AgentHarness` trait, `HarnessEvent`, capabilities and transcript entry types; no UI or database types |
| `libs/acp-client/src/acp_client.rs` | ACP protocol: connection, sessions, permissions, fs and terminal client capabilities; no GPUI or database types; fake agent behind `test-support` |
| `libs/shepherd-core/src/harness.rs` + `harness/` | tier selection and adapter implementations: Codex, OpenCode, Pi RPC, ACP, terminal wrapper |
| `libs/shepherd-core/src/terminal.rs` | `TerminalEngine` boundary, frame model, input encoding, scrollback |
| `libs/shepherd-core/src/pty.rs` | spawn, resize, reader/writer tasks, process groups |
| `libs/shepherd-core/src/git.rs` | Git commands, worktrees, serialized mutations |
| `libs/shepherd-core/src/search.rs` | Turso full-text queries across tasks and projects |
| `libs/shepherd-core/src/store.rs` | Turso via toasty, embedded migrations, batched writes |
| `apps/shepherd/src-tauri/` | v1 Tauri builder, window, invoke commands, channel bridge |
| `apps/shepherd/ui/` | v1 SolidJS views, task workspaces, canvas terminal, key/IME capture |
| `apps/shepherd-gpui/src/main.rs` | v2 GPUI shell: entities, gpui-kit widgets, GPUI canvas terminal |

Reuse rather than reimplement: `libs/agent-harness` for the adapter trait, events
and transcript entries; `libs/acp-client` for ACP connections, permissions, fs and
terminal client capabilities, reaching core services through traits (no GPUI
dependency, fake agent behind `test-support`); `libs/storage` for the `toasty` +
Turso wiring, embedded migration loading and error types (Shepherd owns its own
database and schema); `libs/gpui-tokio` only in the v2 shell, to bridge core
events onto GPUI's foreground. Workspace rules apply: no `mod.rs`, no
`unwrap`, full-word names, errors surfaced rather than swallowed.

### 3.3 Data flow

1. A user intent (send, approve, split, stage) arrives at the core as a
   `Command` over the shell's bridge.
2. The core's tokio tasks call the adapter, PTY service or Git service.
3. The core emits `Event` values on one bus; a shell applies them and coalesces
   per frame at most (GPUI: `cx.notify()` once per frame).
4. Settled transcript entries, state transitions and terminal metadata are
   persisted in batches by the store's writer task; token deltas stay in memory.

The core owns the command registry: IDs, labels, default chords, enabled state and
the effective per-platform map. Key events arrive normalized from the shell; the
core resolves a chord, runs the command, routes text to the focused input, or
encodes terminal bytes when the chord is unclaimed. Focus and input mode are core
state, so neither shell carries UI context in a key event.

### 3.4 Concurrency rules

- One agent process per task; at most one in-flight prompt per task.
- Prompts queue per task and drain when the turn ends, never while an approval is
  pending.
- Terminal IO is one reader task per PTY plus a writer queue; rendering reads a
  ring-buffer snapshot, never the PTY.
- One Turso writer task; reads go through the same core layer.
- Git mutations and worktree creation are serialized globally; status and diff
  reads are debounced and cancelable.
- Adapter probes run at most once per profile per launch, plus on demand.

### 3.5 Shutdown and recovery

- Shutdown is a small state machine: `draining` (stop accepting commands) →
  `terminating` (cancel, close stdin, signal process groups) → `closed`. The UI
  shows progress if it takes longer than 300 ms.
- Every spawned agent and PTY is recorded with PID, start time and process group,
  so shutdown can signal the group rather than the leader alone.
- On launch, tasks in `working` or `waiting` become `interrupted`; a supervisor
  check compares recorded PID and start time and offers to stop a surviving
  process instead of killing a stranger.
- Database writes are transactional; a crash loses at most the last unsettled
  batch, and the transcript is replayed from the last persisted entry.

## 4. Agent integration

### 4.1 Adapter tiers

Every agent profile names a tier. Shepherd prefers the richest tier the installed
CLI supports, and the user can force a lower one.

| Tier | Mechanism | Structured events | Approvals | Resume |
| --- | --- | --- | --- | --- |
| 1. Native adapter | The CLI's own machine protocol | yes | yes, where offered | yes, by session/thread ID |
| 2. ACP adapter | `libs/acp-client` over stdio | yes | yes | `session/load` where supported |
| 3. Terminal wrapper | PTY plus a bundled screen manifest | coarse (working / waiting / exited) | no | no |
| 4. Plain pane | A terminal the user drives | none | no | no |

The tier is visible on the task and in diagnostics, so a "waiting" state from a
wrapper is never confused with a protocol-level approval request.

### 4.2 Adapter contract

`libs/agent-harness` is a UI-free crate: one async trait plus one event enum; it
has no Tauri, GPUI or database types. The UI knows nothing about individual CLIs.

```rust
trait AgentHarness {
    async fn start(&mut self, spec: SpawnSpec, roots: SessionRoots) -> Result<Capabilities>;
    async fn open_session(&mut self, resume: Option<SessionId>) -> Result<SessionId>;
    async fn prompt(&mut self, input: PromptInput) -> Result<()>;
    async fn cancel(&mut self) -> Result<()>;
    async fn respond(&mut self, request: ApprovalId, decision: Decision) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;
}
```

Adapters send `HarnessEvent` values over one bounded channel per session:
`TurnStarted`, `Delta` (message or thought), `ToolCall` start/update/end,
`Plan`, `Usage`, `ApprovalRequested`, `ApprovalResolved`, `ModeChanged`,
`SessionState`, `TurnFinished { reason }`, and `Exited`. Native protocol events
are mapped into the transcript entry types owned by this crate, so persistence,
rendering and search are shared. Adapters never write to the store.

The core, not the adapter, owns turn boundaries and assembly: it starts a turn on
`TurnStarted`, assembles deltas into messages or tool calls, persists only settled
entries, and moves the task on `TurnFinished` (completed, cancelled, failed or
interrupted). It never infers completion from idle time or process exit. The core
assigns a per-session `seq` and a canonical item ID to every persisted entry;
adapter item IDs are mapped in memory so later updates land on the same entry,
and ordering survives reconnects and mixed adapters.

Approval requests carry the tool, arguments, the paths the call touches, and an
optional patch. If the adapter supplies no patch, the core diffs the named paths
inside the worktree; if there are no paths either, the card says the diff is not
available and shows arguments only.

Every adapter declares capabilities instead of assuming them: streaming
granularity, images, resource links, model picker, mode picker, resume, cancel,
approval kinds and whether approvals carry patches, usage reporting and
agent-created terminals. Capabilities are probed at connect, refined per session,
and default to false when unknown; a false capability hides the corresponding UI
rather than showing a dead control.
Shepherd saves the negotiated capabilities with the session. Legacy handles can
be reconnected and probed again. The task shows a tier after a session opens
and displays the reason when a fallback is selected.

Contract tests per adapter: recorded protocol fixtures for parsing, one live
smoke test against an installed CLI, and explicit tests for "binary missing",
"version too old", "auth required" and "process exits mid-turn".

### 4.3 Native adapters

Order of work is deliberate: **Codex, then OpenCode, then Pi**. Each is developed
against the installed CLI and capability-gated, because these protocols are not
stable public contracts.

**Codex** — the primary adapter. `codex app-server` speaks JSON-RPC over stdio
(the same surface the ChatGPT desktop integration uses): thread start, turn start,
streaming items, approvals, interrupt. `codex exec --json` is the fallback for
older versions, giving a one-shot transcript without interactive approvals. The
adapter probes the method surface and refuses features the installed version does
not implement; resume uses the thread ID the server returns.

The desktop app is a normal way to have Codex installed, and it does not put
`codex` on `PATH`: on macOS the same binary sits inside the app bundle
(`Contents/Resources/codex-cli/bin/codex` in current builds,
`Contents/Resources/codex` in older ones, bundle `com.openai.codex`). The adapter
resolves the executable from the profile's explicit path first, then `PATH`, then
those bundle locations, and diagnostics show the path it chose or every path it
tried. A bundle layout change is a resolvable misconfiguration with a visible
reason, never a crash or a silent fallback.

Detection is therefore one step that resolves, verifies and classifies, and it
has a fourth answer beyond the three location kinds: **not installed**. A
candidate is only believed once `--version` and the handshake succeed, and the
kind, path and version are recorded and shown in diagnostics. An ACP client such
as Zed's `codex-acp` is not a fourth kind: it is a bridge that starts the same
Codex binary, so an installation reachable only through that client still has to
be resolved and verified here. Shepherd drives Codex natively; ACP stays the tier
for agents that ship no native surface, not a second Codex path.

**OpenCode** — second. Prefer its server mode (`opencode serve`, HTTP with an
event stream) when available, because it gives session control and reconnect;
otherwise its ACP mode through the same adapter interface. Both paths map to one
adapter; choosing between them is a probe, not a user setting. Model and mode
selection come from what the server advertises.

**Pi** — third, and deliberately **RPC, not ACP**. Pi documents no ACP mode of its
own; its supported subprocess interface is RPC (`pi --mode rpc`), a long-lived
child speaking strict JSONL: commands on stdin, `response` records and session
events on stdout, and an extension-UI subprotocol in both directions
(`docs/rpc.md`, `docs/rpc-extension-ui.md`). That is more surface than a generic
client protocol, which is why the adapter speaks it. Details it must respect:

- Frame on LF only. A generic line reader that also splits on `U+2028`/`U+2029`
  corrupts JSON strings, so the reader is byte-oriented.
- Correlate commands by `id`, never by order. A `prompt` response carries a
  `disposition` (`started`, `queued`, `handled`); `handled` means no run follows.
- Wait for `agent_settled`, not `agent_end`: retries, compaction and steering can
  continue after a run ends.
- Read stdout continuously; Pi honors backpressure and a stalled reader stalls
  the process. stderr is diagnostics, never protocol.
- `@file` arguments are rejected in RPC mode, so attachments and mentions go
  through the `prompt` command rather than the command line.
- Extension-UI dialogs (`select`, `confirm`, `input`, `editor`) become Shepherd
  cards and are answered by `id`; fire-and-forget methods (`notify`, `setStatus`,
  `setWidget`, `setTitle`) update the task header and may be dropped. An
  unsupported request type is shown as text, never silently ignored.
- Shutdown closes stdin and sends `SIGTERM`; a second signal forces exit. An
  unexpected exit is a failure with the exit code.

Pi's extension UI is how a Pi extension asks the user a question; it is not a
tool-approval API. Real tool approvals need the small bundled Pi extension that
Shepherd ships and loads, which forwards tool calls as extension-UI requests and
lets the normal approval cards decide them. If the bridge is missing or fails to
load, Pi still runs: its tools follow Pi's own configuration, the task is labelled
**unreviewed tools** in its header and diagnostics, and Shepherd never claims
that a tool call was approved. The adapter does not trust a version string: it
probes LF framing and the record families the installed binary emits, then
enables only what those families support; unrecognized framing is a probe
failure. If RPC mode is unavailable, the adapter drops to the terminal pane with
a visible reason.

Each native adapter records the exact command and protocol version used, shown in
**Task → Diagnostics** next to the stderr tail.

### 4.4 Freebuff terminal

Choosing **Freebuff (terminal)** starts the `freebuff` executable directly as
the PTY process in the selected project's directory. The terminal replaces
the conversation pane.
Freebuff handles all prompts, approvals, and session behaviour in its own TUI.
Shepherd renders the terminal screen and forwards terminal input. The PTY is
temporary and sits outside the task lifecycle, worktrees, transcript, inspector
layout, and persisted terminal records. The terminal fills the conversation
pane. **Back to chat** in the window header closes it. Use mocked terminal IPC
and a local shell fixture for tests. Test suites must never launch Freebuff.

### 4.5 ACP path

Used for Claude Code (`claude-code-acp`) and any agent that ships ACP without a
native adapter. `libs/acp-client` owns the protocol; Shepherd owns supervision,
presentation and persistence. It has no GPUI dependency, asks the core for fs,
terminal and permission work through traits, and compiles its fake agent only
under `test-support`.

- **Registry and detection.** Exact executable and args are configurable; the app
  never assumes a binary. Detection resolves the command, runs `--version` where
  supported, then the adapter's handshake. A failed probe shows captured stderr
  and a retry, not a silent failure.
- **Lifecycle.** Spawn with piped stdio in the task worktree; capture stderr to a
  ring buffer for diagnostics; `initialize` with client capabilities (fs,
  terminal); store advertised capabilities, auth methods and modes; open
  `session/new` or `session/load`; only then accept prompts.
- **Sessions.** A task stores the agent session ID. On restore, sessions in a
  resumable state get `session/load` and fall back to a new session with an
  explicit notice. Models, modes and commands come from session config; the app
  never invents names, and hides pickers the agent does not advertise.
- **Turns.** Prompts carry text, resource links and images when advertised.
  Cancel is always available and shows "cancelling" until the prompt resolves.
  Follow-ups queue by default; steering interrupts only where the agent supports
  it.
- **Updates → transcript.** Message and thought chunks become rows; tool calls
  become one row per call with status, output and diff; plans become a checklist;
  usage becomes a footer when reported; permission requests become inline cards.
  Successful tool rows collapse; failed ones expand.
- **Permissions.** Allow once, allow for session, always allow for project,
  reject — as offered. Sticky decisions are per project and profile, listed with
  a reset. A pending request never blocks other tasks.
- **Filesystem.** fs requests resolve through `SessionRoots`, confined to the task
  worktree; writes are atomic and preserve trailing newlines; refusals are logged
  and surfaced.
- **Agent terminals.** Agent-created terminals are real PTYs returned by ID; they
  appear as "agent: <title>" panes, are killed when the task ends if unreleased,
  and released terminals close on turn end.
- **Failures.** Missing binary, failed initialize, mid-turn exit, protocol error
  and rejected prompt each have a defined state and a visible cause (§4.7).

### 4.6 Registry and profiles

| Label | Tier | Entry (verified at connect) |
| --- | --- | --- |
| Codex | native | `codex app-server` from the resolved executable — profile path, `PATH`, or the macOS app bundle — with `codex exec --json` as the fallback |
| OpenCode | native | `opencode serve` or ACP mode, whichever the version exposes |
| Pi | native | Pi RPC mode (flag verified at connect), with the bundled approval bridge |
| Claude Code | ACP | `claude-code-acp` |
| Custom | user choice | any command, any tier |

Adding an agent is a form: label, command, args, environment, tier, working
directory policy, and **Test connection**, which probes and lists what the
adapter will actually enable. Wrapper manifests are not editable here; a mismatch
is reported instead. Environment values may be literals or `!command`
substitutions resolved at spawn (§11.1); resolved values are never stored.

### 4.7 Failure modes

| Failure | Behaviour |
| --- | --- |
| executable missing | task not started; error row with install hint |
| probe fails / protocol unsupported | tier downgraded with the reason and the next tier offered |
| initialize or handshake fails | error row with captured stderr and retry |
| Pi approval bridge unavailable | RPC continues; task labelled **unreviewed tools**, diagnostics name the missing bridge, and no approval is claimed |
| agent exits mid-turn | state `failed`, observed exit code and stderr tail shown, recovery offered |
| protocol error | error row with the protocol failure, connection closed if fatal |
| prompt rejected (auth/limits) | error row with the agent's message and advertised sign-in methods when available |
| app quits or crashes | task `interrupted`; resume offered; leftover process offered for termination |
| wrapper manifest missing | plain pane with a notice, no state badge |

The task saves the latest failure message so it can be shown after reopening.
Launch failures allow another prompt after the problem is resolved. For failed
turns, the recovery action reflects the capabilities of the recorded session.

## 5. Terminal service

### 5.1 Scope

A terminal is a PTY, VT state and scrollback, owned by the app for its lifetime.
Closing a view, switching tasks or switching projects never kills a process;
quitting the app does, by design (§2.5). A terminal's scope is either the project
(shared by every task) or one task.

### 5.2 PTY

- Spawn `$SHELL` (or an explicit command) in the active task worktree for a task
  terminal, or the project root for a shared terminal.
- Size follows the pane, applied with a debounce through winsize; new panes use
  the last known size to avoid a reflow flash.
- Environment: inherited, plus `TERM=xterm-256color`, `COLORTERM=truecolor`, and
  `SHEPHERD_TERMINAL_ID` so integrations can address a pane.
- Exit is reported with its code and a restart button; nothing auto-restarts.
- Each terminal runs in its own session. Stop and Quit signal the shell and
  the foreground and background process groups in that session. Groups that
  survive the grace period are stopped after checking their session membership
  again. After a shell exits, Quit can still stop its background jobs if a
  process recorded at shell exit is verifiably still in the same session.
  Processes in separately detached sessions keep running.

### 5.3 Parsing and rendering

- `libghostty-vt` parses into terminal state with scrollback (default 10,000
  lines), reflow on resize and key/mouse/focus/paste encoders, behind one
  `TerminalEngine` trait so `alacritty_terminal` can replace it. The library is
  not `Send`/`Sync`, so each terminal runs on its own emulation thread and talks
  to the core over channels.
- Both shells render the same `TerminalFrame`: dirty row runs with a style table,
  monospace cell layout, theme-mapped ANSI colours, cursor styles, selection,
  copy/paste with bracketed paste, search with highlighting, hyperlinks,
  alternate screen and mouse reporting. v1 paints on a `<canvas>` fed by a Tauri
  channel of binary frames; v2 paints on a GPUI canvas.
- Terminal search keeps the highlight and Next/Previous anchored to the
  matched text as output moves into scrollback. Scrolling the viewport away
  hides the highlight and preserves the navigation anchor. Replaced text or
  a column-width reflow discards the anchor.
- Keys and IME commits arrive from the shell as structured input events. The core
  first resolves app chords against its registry; only unclaimed chords are
  encoded into escape sequences with the engine's encoders, so both shells behave
  identically and terminal apps keep Ctrl-C and readline keys. **Send next key to
  terminal** in the palette bypasses registry resolution for one chord.
- Correctness priorities: alternate screen, wide characters, reflow on resize,
  Kitty keyboard protocol.

### 5.4 Terminals tab

The inspector has exactly one Terminals tab. It contains the project's saved
terminal arrangement: a split tree of terminal panes whose slots are
project-shared or task-specific. Splits, focus and resize follow §2.2 and §2.3;
no terminal is shown outside this tab. A pane whose terminal has exited shows the
code, last command, scope and a restart action.

The selected task's Codex commands have a read-only section beside the
interactive terminals. It shows command status and streams the combined stdout
and stderr from Codex app-server in order. The exec fallback shows the output
it reports when a command ends. The section opens the latest command by default
and keeps a manually selected command visible while new output arrives. It
retains the last 64 KiB per command and marks omitted output. Command views
have no PTY, input, restart action, or slot in the saved layout.

### 5.5 Persistence and restart

- Terminal metadata (cwd, command, profile, project, task, scope, title,
  created/ended, exit code) is stored; scrollback is not.
- A shared terminal belongs to the project; a task terminal belongs to its task.
  The slot arrangement and each slot's scope live in the project terminal layout.
- Quitting ends every PTY. Next launch shows those terminals as **ended** in the
  Terminals tab of their task, or of every task for a shared terminal, with their
  last cwd and command and a one-click restart. This is stated in the UI, not
  presented as restoration.
- Closing a terminal view detaches it only; the PTY keeps running and the view
  can be reopened from the Terminals tab's instance menu.

### 5.6 Profiles

Named presets: label, command, args, cwd policy (worktree or project root), env,
optional prompt prefix. Shipped: shell, `codex`, `opencode`, `pi`, `claude`,
plus user-defined. Environment values may use `!command` substitution,
resolved at spawn and never stored.

## 6. Projects

- Add by picker or drop; store absolute path, name, colour, Git remote, default
  branch, last opened, last active task, preview URL.
- A missing path stays listed and marked; its tasks are read-only until relinked.
  Duplicate paths are rejected; relinking a moved repository keeps tasks.
- Git metadata is read lazily and cached; the UI never blocks on a Git command.
- Each project owns a terminal layout: slots, split sizes and scope. Shared
  terminals are project-owned and appear in every task's Terminals tab.
- Per-project defaults: last adapter profile, last model/mode, terminal profile,
  approval policy, terminal layout preset. New worktrees always start from the
  project's default branch.
- Deleting a project requires confirmation and never deletes files, worktrees or
  branches on disk; it removes Shepherd's records, including shared terminals and
  the terminal layout.

## 7. Tasks and dispatch

### 7.1 State machine

```
draft → starting → ready → working ⇄ waiting (approval or question)
                     ↑          ↓
                   queued     review ─┬─→ done (archived)
                                      ├─→ working (new prompt)
                                      └─→ failed | cancelled | interrupted
```

Stored states: `draft`, `starting`, `ready`, `working`, `waiting`, `queued`,
`review`, `done`, `failed`, `cancelled`, `interrupted`. A `ready` task accepts a
prompt at any time; a `waiting` task holds its queue until the approval is
answered; `review` means a completed turn changed HEAD, branch or worktree
status/diff since turn start and a human decision is pending, while a net-zero
turn returns to `ready`; `done` is archived; `interrupted` is resumable and
cleared on the next successful session. Archiving keeps the branch; an archived
worktree unused for 30 days is removed automatically, and reopening recreates it
from the branch or recorded tip (§7.2). An adapter that cannot resume says so and
offers transcript replay with a new session instead of pretending to continue.

### 7.2 Creating a task

1. Open a draft from the project's compose button or type into an empty
   project's chat. The draft opens with its chat and inspector. Pick an agent
   profile and review its tier and capabilities before sending. Shared terminals
   from the project layout are available from the start. The draft also appears
   in the sidebar when the project has multiple visible tasks.
2. Show the proposed worktree path and branch; neither exists yet.
3. Optionally pick model/mode if the adapter advertises them.
4. Send the first prompt. Creating the worktree, branch and task starts on send;
   a draft discarded before send leaves no Git trace.

Worktree rules:

- Resolve the repository's real Git directory with
  `git rev-parse --absolute-git-dir`, then create the worktree at
  `<git-dir>/shepherd/worktrees/<slug>` on branch `shepherd/<slug>` from the
  project's default branch. Verified against git: the path is accepted, appears
  in `git worktree list`, and is outside the working tree, so it needs no ignore
  rule.
- Resolving the Git directory rather than hard-coding `.git` handles linked
  worktrees and submodules, where `.git` is a file.
- A shared checkout is available per task with a visible warning that parallel
  agents will collide; it is never the default.
- Dirty worktrees are never deleted. Removing a task offers worktree removal,
  refuses when changes exist, and runs `git worktree prune` only after explicit
  confirmation.
- Archiving keeps the branch. A task open, prompt, Git action or terminal use in
  the worktree updates `worktree_last_used_at`. A startup pass removes archived
  worktrees unused for 30 days with `git worktree remove` then `git worktree prune`,
  skipping dirty ones and listing them with the reason. Before removal it records
  the branch tip in `branch_tip_sha`; branches are never deleted automatically.
  Reopening recreates a missing worktree from the branch, or from that tip when
  the branch is gone.
- Bringing work back is done with **squash-merge** in the Git tab (§9.3); it is
  the only merge action, and the task branch is kept.
- Startup reconciles recorded worktrees: a missing one marks the task read-only
  with a repair action.

### 7.3 Queueing and steering

- Follow-ups queue above the composer with reorder and remove.
- **Send now** interrupts where supported, otherwise cancels and re-prompts with a
  note that this is a correction.
- Cancelling a turn keeps the session and worktree alive.

### 7.4 Approvals

Default policy is **agent-judged**:

1. Reads and a local safe-command list are auto-allowed (file reads, `git status`,
   `git diff`, `git log`, `ls`, `rg`, `cat`). The list is visible per project.
2. Every other call goes to the agent's own review mode where the adapter exposes
   one (Codex approvals, Claude's permission modes, ACP `request_permission`, Pi
   through the bridge extension). Shepherd renders the decision, records it, and
   shows the agent's reason when one is provided.
3. If no judge is available — adapter without review, judge error, timeout, or a
   task in a mode that cannot decide — the call becomes a normal approval card
   and waits for the user. It is never silently allowed. A Pi session without its
   approval bridge is the exception: its tools run under Pi's configuration and
   the task is labelled **unreviewed tools**, because Shepherd has no request to
   judge.
4. Sticky decisions (allow for session, always allow for project) are per project
   and profile, listed with a reset. Every decision is written to the transcript
   with its source (safe list, agent, user).
   For ACP, session grants match the reported operation and input within the
   live connection. Shepherd uses the agent's one-time option for each match,
   keeping the grant lifetime under Shepherd's control.
   Project grants use the same match and are scoped to the project and the
   configured executable, arguments, and launch environment overrides.
   They survive Shepherd restarts. Settings → Projects lists saved project
   grants and live session grants by task, with a reset for each grant.

- Project overrides may only tighten this policy: they can remove commands from
  the safe list or require approval where the global default auto-allows. Adding
  safe commands or relaxing approvals is a global change. Explicit per-call
  decisions remain separate.

- A card shows the tool, its arguments, and the affected diff inline when the
  call touches files, so a decision does not require a detour to the Git tab.
- Cards waiting on the same task can be approved together; the batch is one
  explicit action and is recorded as one decision with its count.

There is no Shepherd-owned model and no provider key; the judgment always comes
from the agent the user is already running.

### 7.5 Notifications

On transition to `waiting` (approval or question) while the task workspace is not
focused: opt-in system notification, a ring and `needs input` badge on the task
row, and a dot on the composer or the Terminals tab when the wait comes from a
terminal. `failed` updates the row. `review` adds a review badge and ring on the
task row and Review tab, and updates the status-bar count and dock badge, but
sends no system notification; `done` clears them. Chat has no tab, so its
attention lives on the task row and composer. Attention from a shared terminal
appears on the Terminals tab, not on a single task row. The dock badge is used
where the platform supports it (macOS); Linux relies on the status-bar count.
State comes from adapter events and process exit, never polling.

### 7.6 Review

- Only a completed turn whose HEAD, branch or worktree status/diff changed since
  turn start enters `review`; a net-zero or chat-only answer returns the task to
  `ready`.
- A task records files touched, from adapter tool diffs plus a worktree diff.
- "Changed in this task" lists them with status; opening one shows the worktree
  diff in the Review tab.
- The Git tab is the single place to stage and commit, so review and commit do
  not diverge.
- Review has no separate view or inbox: the task row and its Review tab badge,
  the status-bar count and the dock badge are the surface.
- **Mark reviewed and archive** is one action and moves the task to `done`;
  **Reopen** returns it to `review` and recreates a collected worktree.
- **Squash-merge into base** is available from the Git tab; it is the only merge
  action and records the branch tip for recreation.
- **Archive selected** in the task list handles several tasks at once after a
  confirmation; review notes stay per task.
- Archiving keeps the branch; an archived worktree unused for 30 days is removed
  automatically (§7.2), and reopening recreates it from the branch or recorded
  tip. Dirty worktrees are skipped.
- Unreviewed tasks stay in `review` until acted on; nothing auto-archives.
- **Send review note** starts a new turn with the note as the prompt and the
  changed files attached as context; it queues when the task is still working and
  is recorded like any other turn.
- Staging stays file-level (§9.3); review does not introduce hunk staging.

## 8. Files view

- Tree from the project root or the task worktree, respecting `.gitignore` and
  `.shepherdignore`.
- Lazy expansion, dotfile toggle, name filter, and search: names immediately,
  contents on submit via a background, capped, cancellable scan.
- Viewer: read-only, syntax-highlighted, line numbers; **Open in external editor**
  and **Reveal in file manager** commands. Images and PDFs are later.
  Open in the desktop's default text editor. If the Linux file manager cannot
  select a file through its desktop interface, open the containing folder.
- Changed files carry added/modified/deleted/renamed badges from the Git service,
  refreshed by a debounced (150 ms) watcher.
- Opening a file focuses the Files tab; the tab can browse the project root or the
  task worktree, and its header names which.

### 8.1 Preview and annotation

- The Preview tab loads the project's configured loopback HTTP(S) URL in an
  isolated browser surface. Its setup card opens **Settings → Projects** to
  save the address. Saving an empty field clears it.
- **Annotate** freezes the page and lets the user click an element. The
  annotation captures the selector, a short text excerpt, the bounding box and an
  optional screenshot, plus the page URL.
- Annotations queue in the tab; **Add to prompt** inserts them into the composer as
  structured context (URL, selector, note) and clears the queue. The agent sees
  them as editable text, not an image-only blob.
- Element annotation requires script injection. If the preview navigates away
  from its configured origin, refuses framing, blocks injection, or is not
  HTML, the tab offers screenshot annotation and reports that element
  selection is unavailable.
  Open the page in a browser and choose a PNG or JPEG screenshot up to 50 MB.
  Mark a point on the image. **Add to prompt** saves the image in the task's
  attachment directory and inserts its local path, pixel coordinates, image
  dimensions, page URL, and note as editable text.
- Reload and open-in-browser are explicit; the preview never navigates the app
  shell.

## 9. Git view and commit

### 9.1 Decision: shell out to `git`

All Git operations run the user's `git` binary rather than libgit2: hooks,
signing, `core.autocrlf`, sparse checkouts and user config must behave exactly as
in a terminal, and libgit2 diverges on all of them. Reads use stable formats
(`status --porcelain=v2`, `diff --no-color`, `log --format`); parsing is
unit-tested against fixtures.

### 9.2 Read

- Status: branch, upstream ahead/behind, staged/unstaged/untracked, renames,
  conflicts, per task worktree and per project root.
- Diff: unified per file, word-level highlighting, staged and unstaged, binary
  files shown as size plus a notice.
- Log: last 50 commits on the branch, author, relative time, subject.
- Branches: local list, checkout action, detached HEAD stated explicitly.

### 9.3 Mutations

| Action | Command |
| --- | --- |
| stage file / all | `git add -A -- <path>` |
| unstage | `git restore --staged -- <path>` |
| discard | `git restore -- <path>` (confirm, shows the diff) |
| commit | `git commit -F -` with the message on stdin |
| amend | `git commit --amend -F -`, tip commit only |
| checkout branch | `git switch <branch>`, blocked when dirty unless confirmed |
| create branch | `git switch -c <name>` |
| squash-merge | `git merge --squash <task-branch>` then `git commit -F -`, project root only |

- Staging is file-level in v1: stage or unstage a whole path. Hunk-level and
  line-level staging are not offered, so review and commit agree on the unit.
- Commits run with the user's hooks and signing; failure output is shown verbatim
  and the message is preserved.
- **Squash-merge is the only merge action.** It requires the project root to be
  clean and on the task's base branch (the project default recorded when the task
  was created); otherwise it is disabled with the reason. On conflicts it stops,
  lists conflicted paths, and leaves resolution or abort to a terminal; there is
  no in-app conflict editor. The commit uses the task title and the user's hooks
  and signing, and the task branch is not deleted.
- Before merging, the task branch tip is recorded in `branch_tip_sha`.
- After a successful squash merge, the Git pane confirms the destination branch.
- No push, pull, rebase, reset --hard or force operations in v1.
- Mutations are serialized per project; the commit form refreshes status before
  committing and offers to include files changed since it opened.

### 9.4 Worktrees

Created and removed only by Shepherd, recorded in the database, reconciled on
startup, and located under the resolved Git directory (§7.2). Only archived
worktrees are collected: one unused for 30 days is removed automatically, its
branch tip recorded first, and a dirty one is skipped and listed. Reopening
recreates the worktree from the branch, or from the recorded tip when the branch
is gone. A worktree is never removed while dirty, and a task's branch is never
deleted automatically.

## 10. Desktop agent-app parity

| Feature | v1 | Notes |
| --- | --- | --- |
| Chat threads with history | yes | tasks, per project, searchable across tasks |
| Streaming markdown, code blocks, copy | yes | shared transcript renderer |
| Tool activity display | yes | structured for native and ACP tiers |
| Attachments: files, images | yes | images only when advertised |
| `@file` mentions | yes | resource links |
| Slash commands | yes | agent-advertised plus app commands |
| Model/mode picker | yes | advertised only |
| Command palette, rebindable keys | yes | one core registry |
| Search across tasks | yes | Turso FTS (Tantivy-backed) |
| Notifications | yes | opt-in system notifications |
| Light/dark theme, accent, density | yes | token-based |
| Worktree per task | yes | default, under the Git directory |
| Terminals survive closing a pane | yes | same session only |
| Terminals survive quitting | no | ended and restartable; stated in the UI |
| Multi-window | no | one window, always |
| Export/share | later | markdown export |
| Usage and cost | later | when an adapter reports it |
| Memory / custom instructions | later | per project text |
| MCP servers | later | pass through to ACP and native adapters |
| Remote projects over SSH | later | would need a host-side helper; out of scope for the single-process app |
| Voice, canvas, schedules | no | |

## 11. Persistence

One Turso database (in-process, SQLite-compatible) at the platform data dir, via
`toasty` + `toasty-driver-turso`. Migrations are numbered SQL files embedded in
the binary (`0001_init.sql`, …), applied at startup in a transaction after a
pre-migration backup, with the applied version recorded in a migration table. The
core is the only writer.

| Table | Columns (abridged) |
| --- | --- |
| `projects` | id, path (unique), name, color, git_remote, default_branch, last_task_id, preview_url, created_at, last_opened_at |
| `agent_profiles` | id, label, command, args_json, env_json, tier, builtin |
| `tasks` | id, project_id, agent_profile_id, title, state, tier, worktree_path, branch, base_branch, model, mode, approval_policy, pid, pid_started_at, reviewed_at, worktree_last_used_at, branch_tip_sha, created_at, updated_at, error |
| `sessions` | id, task_id, adapter, protocol_version, agent_version, session_id, capabilities_json, config_json, resumed_from |
| `entries` | id, session_id, seq, kind, role, text, tool_json, diff_json, decision_json, created_at |
| `entries_search` | Turso full-text index (Tantivy-backed) over entry text, task title and project name |
| `permissions` | id, project_id, agent_profile_id, tool_key, decision, scope, source, created_at |
| `terminals` | id, project_id, task_id (null for shared), scope, title, cwd, command, profile, created_at, ended_at, exit_code |
| `attachments` | id, task_id, entry_id, relative_path, mime, bytes, created_at |
| `terminal_profiles` | id, label, command, args_json, cwd_policy, env_json, prompt_prefix |
| `terminal_layouts` | project_id (unique), layout_json, updated_at |
| `layouts` | task_id (unique), workspace_json, updated_at |
| `git_snapshots` | id, task_id, commit_sha, dirty, captured_at, summary_json |

Rules: entry writes are batched per settled message or tool call, never per token; deleting a task deletes its entries, sessions, attachments, task-scoped terminals and workspace layout but never its worktree or branch without confirmation; deleting a project removes its shared terminals and terminal layout but no files; attachments are copied under the app data directory, capped at 50 MB each (or the project override), and deleted with the task; environment values beginning with `!` are resolved at spawn (not at save) with `/bin/sh -c`, a 5 s timeout and one trailing newline trimmed, failure blocks the spawn, and the output is never stored or logged; migrations never drop data within a major version.

`workspace_json` stores chat/inspector widths and chat scroll;
`terminal_layouts.layout_json` stores the terminal split tree, the selected
inspector tab and each slot's scope. A terminal row with scope `project` has a
null `task_id`; scope `task` has a task id.

### 11.1 Settings files

`settings.json` and `keybindings.json` live in the app config directory and are
the only source of truth for user settings; there is no settings table. The
in-memory model is still scoped key/value pairs, serialized per scope:

```json
{ "version": 1,
  "app": { "theme": "dark", "terminal.scrollback_lines": 10000 },
  "projects": { "/Users/me/src/repo": { "terminal.scrollback_lines": 20000 } } }
```

- Both files carry an integer `version` and are written atomically with
  owner-only permissions. Migrations run on load and keep a backup first; a
  version newer than the binary refuses edits instead of downgrading.
- Project overrides are keyed by absolute path, so the file stays readable; a
  moved project loses its overrides. Keybindings are app-scoped, one chord per
  command; sequences are rejected.
- `app["agents.opencode.executable"]` points to an existing OpenCode executable
  by absolute path. A stable profile symlink such as
  `/etc/profiles/per-user/me/bin/opencode` can follow Nix profile updates.
- `app["agents.claude.executable"]` points to an existing Claude Code ACP
  executable by absolute path. `app["agents.claude.args"]` is an optional
  array of arguments passed in order.
- Resolution is per key: project > app > built-in, with whole-value replacement
  and no deep merge. Reset deletes the override.
- Unknown keys are preserved on rewrite. A missing file means defaults; a
  malformed file is copied to `.bak`, defaults load, and a banner names the path.
- In-app edits apply immediately and are journaled per JSON path with the
  previous and new value. Before writing, the file is re-read: if unchanged, the
  journal is applied and the file rewritten atomically. If it changed externally,
  entries whose previous value still matches are replayed; entries whose previous
  value no longer matches are reported as conflicts, kept pending, and never
  silently overwritten. A backup is kept. External edits otherwise load through
  **Reload configuration**; v1 has no filesystem watcher.

## 12. Security and privacy

- No network requests except those agents make. No telemetry, analytics or crash
  reporting; logs are local files the user can open.
- Agents inherit the user's permissions and are not sandboxed; the approval flow
  and the visible spawn command are the control surface.
- fs capability is confined to the task worktree or project root; traversal
  outside is refused and surfaced.
- No secret values are stored: agents keep their own credentials and Shepherd has
  no provider keys. An environment value beginning with `!` is a shell command
  resolved at spawn (for example `!cat ~/.secrets/token`); v1 allows this only in
  environment maps. Only the command is stored, its output is never written to the
  database, logs or UI, and a failure blocks the spawn with the command's error.
  Commands run with the user's permissions, a 5 s timeout and the project root (or
  home for app-scoped values) as cwd.
- Worktree removal refuses dirty trees; destructive Git operations are absent
  rather than merely confirmed.
- There is no local server or socket to authenticate; the app has no listening
  port.

## 13. Performance budgets

| Path | Budget |
| --- | --- |
| Cold start to interactive | < 1.2 s warm cache, < 4 s cold |
| Keystroke to paint in composer | < 16 ms at 2k-entry transcript |
| Streaming, 3 tasks | 60 fps, no frame > 32 ms |
| Grid with 8 panes, 2 live terminals | 60 fps while resizing |
| Terminal burst (1 MB) | parse and paint < 100 ms, bounded memory |
| Git status, 20k files | < 150 ms, debounced, off the UI thread |
| Cross-task search, 50k entries | < 250 ms p95, cancellable |
| Quit with 3 agents running | < 2 s from confirm to process exit |
| Memory, 3 agents + 4 terminals | < 400 MB |

No filesystem, Git, database or VT work on the shell's foreground thread; every buffer
is bounded (scrollback, stderr, search results, per-task event queue).

## 14. Accessibility and theming

- Full keyboard operation, visible focus ring, focus order follows the sidebar,
  chat, inspector tabs and terminal splits.
- Pane focus is announced and reachable without a mouse; every inspector tab and
  terminal pane header is a button target, and the pane switcher is a labelled
  tablist whose rings and badges are also exposed as text.
- Respect reduced motion, increased contrast and the system light/dark
  preference.
- Tokens only: no literal colours in components; themes define surface, text,
  accent, status and ANSI palettes, all meeting WCAG AA against their surface.
- Screen-reader labels on icon buttons, task states, diff rows and pane kinds.
- The canvas terminal keeps an off-screen text mirror in the accessibility tree
  and supports a keyboard copy mode, so reading and copying never require a mouse.
- Themes are built in for v1 (light, dark and the system preference); custom
  theme creation is deferred. Keybindings are edited in-app; invalid values,
  conflicting chords and multi-chord sequences are rejected with a reason and
  cannot prevent startup.

## 15. Testing

- **Adapters**: contract tests with recorded fixtures per native adapter, the ACP
  fake agent, and wrapper manifests against captured screen output; live smoke
  tests gated on the CLI being installed. Pi fixtures cover LF-only framing,
  record-family probing, and a missing extension bridge surfacing
  **unreviewed tools**.
- **Harness contract**: a fake adapter drives `TurnStarted`/`Delta`/`ToolCall`/
  `TurnFinished`, approval requests with and without a patch, and a process exit
  without a finish; the core must not infer completion. Out-of-order adapter
  updates map to one canonical entry, and ordering holds after reconnect.
- **Commands**: registry metadata, per-platform defaults, conflict rejection,
  focus-dependent routing, terminal passthrough for unclaimed keys, and the
  send-next-key escape bypassing resolution once.
- **ACP ports**: fs, terminal and permission requests through fake ports, with the
  fake agent built under `test-support`.
- **Lifecycle**: quit with a working fake agent and a live PTY (confirmation,
  termination within budget, interrupted state); quitting with only idle
  terminals does not prompt; crash simulation leaves a recorded PID that the
  orphan check offers to stop; a reused PID is not killed.
- **Review trigger**: edit-then-revert stays `ready`; a commit, branch change or
  content change in an already-dirty file enters `review`; an adapter-reported
  path with no net change does not.
- **Terminals**: PTY fixtures for colours, alternate screen, resize and large
  output; shared and task scope across task switches; task terminals restored to
  their task; assert parser state, not pixels.
- **Git**: temporary repositories covering status parsing, diffs, failing hooks,
  signing disabled, rename detection, worktree add/remove under the resolved Git
  directory (including a `.git` file case), dirty-tree refusal, squash-merge from
  a clean root, a dirty-root refusal, conflicts, and archived-worktree
  collection/recreation. A new worktree uses the project's default branch even
  when the root is on another branch.
- **Persistence**: migrations from empty and previous schema; task round-trip with
  entries, task workspace and project terminal layout; shared and task terminals;
  each project restores its last active task; a missing last task opens a draft;
  search-index rebuild.
- **Settings**: load defaults, migrate an older file, preserve unknown keys, reject
  a malformed file with a banner and backup, resolve project overrides per key,
  reject keybinding conflicts, replay an external edit, and report a value
  conflict without overwriting it.
- **UI**: v1 Playwright tests against the Tauri webview for the palette, flat task
  list, project compose and archive controls, project switching, fixed
  chat/inspector regions, the Terminals tab and its project layout, a shared
  terminal staying mounted across task switches, inspector tabs, attention
  rings/dots/badges, preview annotation and its screenshot fallback, the draft
  sidebar row, and pane focus; v2 GPUI tests for the same flows, with GPUI
  executor timers as required by the repository rules.
- **CI**: `cargo check`, `cargo test`, formatting and clippy across the crates.

## 16. Milestones and acceptance

| Milestone | Deliverable | Acceptance |
| --- | --- | --- |
| **M0** | Task-workspace shell, project registry, schema, palette, status bar, settings files, quit flow | Add two projects and restart to verify both persist with their last task workspaces and trees. Compose opens a draft in the chosen project. Its menu can reveal archived tasks. The selected project shows a flat list and switching projects collapses the previous list. A missing last task opens a draft. Settings and keybindings survive restart in the config directory. A malformed file falls back to defaults with a banner. The palette lists commands with their bindings and disabled reasons. Quitting with nothing running is instant. A missing path is flagged. |
| **M1** | Fixed task workspace, terminal service, project terminal layout, worktree per task | Split the Terminals tab and resize it; switch tasks and keep a shared terminal mounted; a task terminal disappears with its task and returns on reselect; the terminal layout is saved per project; chat and inspector cannot be torn apart; closing a terminal view keeps it running; quitting with a working agent warns and stops it within 2 s; restart shows ended terminals in their task workspaces and restarts them; the first send creates a worktree under the Git directory |
| **M2** | Codex native adapter end to end | A prompt streams into the transcript with tool rows; approvals follow the agent's review mode; cancel stops the turn; resume works by thread ID; a finished turn with changes lands in **needs review**, a chat-only turn returns to **ready**, and both archive and review-note actions work; approval cards show a diff when the adapter or worktree provides one and say when they cannot; adapter item IDs map to canonical entries and ordering holds after reconnect; an archived worktree unused for 30 days is removed and reopens from its branch or tip; quitting mid-turn marks the task interrupted |
| **M3** | OpenCode adapter, ACP path (Claude Code), standalone Freebuff terminal | OpenCode and ACP produce a correct transcript. A fixture verifies that Freebuff opens a plain terminal in the conversation region. |
| **M4** | Files, Git and preview panes | Tree respects ignore rules; badges update; stage, unstage, commit with hooks and signing, history and branch checkout work; a failing hook preserves the message; squash-merge is disabled on a dirty or off-base root and lists conflict paths with a terminal action; preview loads the project URL and element annotations add structured context, with screenshot fallback when injection is blocked |
| **M5** | Pi RPC adapter, attachments, mentions, model/mode pickers, cross-task search, notifications | Pi runs through RPC, uses its approval bridge when available, and is labelled **unreviewed tools** when it is not; a version without RPC drops to a pane with a reason; images and files reach the agent; `@file` inserts a link; search finds text in old tasks |
| **M6** | v2 GPUI shell over the same core: gpui-kit panes, GPUI canvas terminal, v1 shell deleted after flow parity. Starts only when v1 is feature complete, with no calendar date | Every v1 flow passes on GPUI; keybindings, settings, transcripts and search history carry over; theme and accessibility parity and the platform floor are tracked as accepted debt at deletion; `cargo tree -p shepherd-core` has no Tauri or GPUI crate; frames and input behave identically |

## 17. Resolved decisions

No interview questions remain open. The final five agent/worktree decisions and
all UI follow-up answers are recorded here for traceability.

- **Pi approvals:** ship a minimal extension-UI bridge that turns Pi tool calls
  into normal approval cards. Without it, the task is labelled
  **unreviewed tools** and Shepherd makes no approval claim (§4.3, §7.4).
- **Pi RPC gate:** probe LF framing and the record families the installed binary
  emits, and enable only what the probe supports rather than trusting a version
  number (§4.3, Appendix B).
- **Quit with idle terminals:** quit without confirmation; confirm only when a
  task is working or waiting (§2.5).
- **Crash orphans:** offer to stop a surviving process using its recorded PID and
  start time, and never kill an uncertain match (§3.5, §4.7).
- **Worktree base:** create from the project's default branch, not the currently
  checked-out branch (§7.2).

**UI organization**

- The project list shows a flat task list under the selected project when more
  than one task is visible. **Show archived** includes `done` tasks in that list.
  The unfinished count excludes `done` and `cancelled` (§2.1).
- The workspace has a fixed middle region for chat, Settings, or the standalone
  Freebuff terminal and a fixed inspector on the right. Switching tasks swaps
  chat and task-specific terminal slots while the inspector tab and shared
  terminals stay mounted (§2.2).
- **Inspector:** always-on right region, resizable but never closed; tabs are
  Terminals, Preview, Review, Git and Files; terminal splits live only inside
  Terminals (§2.1, §2.2).
- **Terminals:** one Terminals tab per workspace; its arrangement is saved per
  project; slots are project-shared or task-specific; no global terminal list
  (§2.2, §5.4).
- **Attention:** rings, dots and text badges on task rows and inspector tabs;
  chat attention stays on the task row and composer; shared-terminal attention
  stays on the Terminals tab (§2.3, §7.5).
- **Preview:** browser preview with element annotation; annotations add structured
  context to the composer; screenshot fallback when injection is blocked (§8.1).
- **Task browser:** sidebar list only; no board or separate review view in v1
  (§2.2).
- **Density:** dense shell chrome with a readable transcript measure (§2.1).
- **Landing:** restore the last project and last task; if that task is gone, open
  a new draft, never an older task (§2.2).
- **New task:** a draft opens in its workspace. Its worktree is created on the
  first send (§7.2).

Earlier resolutions — attachment cap 50 MB, the default FTS tokenizer with prefix
queries, core-assigned event ordering and item IDs, and `libs/shepherd-core` as
the shared core from v1 — are folded into §3.2, §4.2, §10 and §11.

## 18. Non-goals

- Model proxying, hosted sandboxes, deploy, bots, mobile, accounts, telemetry.
- A daemon, background service, remote runtime or multi-window attach.
- A command-line interface in v1.
- Hunk-level or line-level staging.
- Tracking subagents that do not own a terminal.
- Being an editor, IDE, LSP host or debugger.
- Push/pull/rebase UIs, PR review, issue trackers, and in-app conflict resolution.
- Update checks or auto-update in v1.
- Agent orchestration beyond parallel tasks; agents may still spawn their own
  subprocesses.
- Programmable UI, plugin API or scripting surface.

## Appendix A — ACP surface used

Client → agent: `initialize`, `session/new`, `session/load`, `session/prompt`,
`session/cancel`, `authenticate`.
Agent → client: `session/request_permission`, `fs/read_text_file`,
`fs/write_text_file`, `terminal/create`, `terminal/output`,
`terminal/wait_for_exit`, `terminal/kill`, `terminal/release`.
Agent → client notifications: `session/update` (message chunks, thought chunks,
tool calls and updates, plan, commands, mode/config changes, usage).

`libs/acp-client` owns the protocol types; Shepherd owns supervision,
persistence and presentation.

## Appendix B — Adapter commands and verification

| Agent | Tier | Default entry | Fallback | Verify at connect |
| --- | --- | --- | --- | --- |
| Codex | native | `codex app-server` | `codex exec --json` | version, method surface, approval kinds |
| OpenCode | native | `opencode serve` | `opencode acp` | server mode, session control, models |
| Pi | native | `pi --mode rpc` | terminal `pi` | LF framing and record families, `agent_settled`, extension-UI methods; approval bridge (degraded label if absent) |
| Claude Code | ACP | `claude-code-acp` | terminal `claude` | initialize, session/load, modes |
| Custom | any | user command | terminal | chosen adapter's handshake |

No command line in this table is a stable contract, and neither is its location:
Codex may exist only inside the ChatGPT app bundle, so finding the executable is
part of connecting and the chosen path is shown in diagnostics. The registry
probes the installed version, and a mismatch downgrades the tier with a visible
reason rather than failing silently.

## Appendix C — Default keymap

Defaults live in the core's command registry; user overrides live in
`keybindings.json`. One chord per command; sequences are rejected. On Linux the
app uses Ctrl Shift where the plain chord is a standard readline key, so the
focused terminal keeps Ctrl-K, Ctrl-P, Ctrl-W and friends. Only these chords (or
user overrides of them) are claimed while a terminal has focus. **Send next key
to terminal** bypasses that claim for one chord.

| Action | macOS | Linux |
| --- | --- | --- |
| Command palette | `⌘K` | `Ctrl Shift K` |
| Project / task switcher | `⌘P` / `⌘O` | `Ctrl Shift P` / `Ctrl Shift O` |
| New task | `⌘T` | `Ctrl Shift T` |
| New terminal | `⌘⇧T` | `Ctrl Shift N` |
| Toggle sidebar | `⌘B` | `Ctrl Shift B` |
| Split right / down | `⌘⇧D` / `⌘⇧E` | `Ctrl Shift D` / `Ctrl Shift E` |
| Focus next / previous pane | `⌘\`` / `⌘⇧\`` | `Ctrl \`` / `Ctrl Shift \`` |
| Close pane | `⌘W` | `Ctrl Shift W` |
| Cancel turn | `⌘.` | `Ctrl .` |
| Send prompt | `⌘Enter` | `Ctrl Enter` |
| Commit | `⌘Enter` in the commit form | `Ctrl Enter` |

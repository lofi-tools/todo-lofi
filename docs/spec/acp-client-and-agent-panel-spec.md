# ACP client crate + todo-2 agent panel — spec

## 1. Intent

Add an AI coding-agent panel to the **todo-2** app (`apps/todo-2`). The panel talks to an
agent subprocess over the **Agent Client Protocol (ACP)** using a new workspace crate
`libs/acp-client`, whose implementation follows **Zed's ACP client** as its blueprint.

The panel is scoped to **directory-based projects**. The agent process is launched with the
project's directory as its working root, and `opencode` (in ACP mode) is the first — and for
now only — registered agent.

Two deliverables:

1. `libs/acp-client` — a reusable ACP **client** crate (process launch, JSON-RPC connection,
   session lifecycle, thread/transcript model, fs + terminal client capabilities).
2. The **Agent panel** in `apps/todo-2` — a right-hand pane with prompt input, streaming
   transcript, model/mode/command controls, permissions, and per-project session continuity.

---

## 2. Interpretation of "copy the acp client code from the zed codebase, exactly as it is"

Zed's ACP client cannot be copied verbatim into this workspace, and the interview resolved the
tension explicitly:

- **License wins over literalism.** Zed is `GPL-3.0-or-later` and this repo has no LICENSE
  file and no `license` field in any `Cargo.toml`. Verbatim copying would make the derived
  code GPL. Decision: **reimplement, using Zed as the blueprint** — same architecture, same
  behaviours, same protocol coverage — with no copied source text.
- "Exactly as it is" therefore means **behavioural fidelity**: the same client-side surface
  Zed provides to its agent panel, reproduced against todo-2's stack.
- The ACP protocol layer itself is not hand-copied either: we depend on the official Rust SDK
  (`agent-client-protocol`).

### 2.1 Zed reference material

Zed checkout used: `/Users/me/src/contrib/zed` @ `cc409210c151716aed980b67b4f186768a2599f3`
(May 15 2026). This is the reference, not a source of copy-paste.

| Zed path | Lines | What to mirror |
| --- | --- | --- |
| `crates/agent_servers/src/acp.rs` | ~3797 | `AcpConnection`/`AcpSession`, process launch + `current_dir`, `initialize` (ProtocolVersion::V1), `session/new`, `session/load`, `session/prompt`, `session/cancel`, session modes, model selector, session config options, `available_commands` handling, stderr capture/debug log, `UnsupportedVersion` handling, foreground-work dispatch for client-side requests |
| `crates/acp_thread/src/acp_thread.rs` | ~5852 | Thread model: streamed `session/update` handling (agent/thought chunks, tool calls, tool-call updates, plan, available commands, mode/config changes), turn state, permission requests, auth-required state |
| `crates/acp_thread/src/connection.rs` | ~1036 | `AgentConnection` trait surface: new/load/resume/close session, prompt, cancel, authenticate, modes, model selector, capabilities |
| `crates/acp_thread/src/diff.rs` | ~454 | Diff extraction/rendering for file-edit tool calls |
| `crates/acp_thread/src/mention.rs` | ~1073 | `@`-mention plumbing (reference only; see non-goals) |
| `crates/acp_thread/src/terminal.rs` | ~255 | Terminal provider contract for `terminal/*` client requests |

Zed's pinned ACP version is `agent-client-protocol = "=0.11.1"`; we are **not** matching that
pin (see §3).

---

## 3. Protocol crate and version

- Dependency: **`agent-client-protocol` 2.1.0** (current, Sept 2026). Zed's `0.11.1` pin is two
  majors behind and its API (`Agent`/`Client`/`ConnectionTo`/`Lines`/`Responder`) does not
  exist in 2.x, so Zed's call sites must be translated rather than reused.
- Target the **stable v1 protocol** (`ProtocolVersion::V1`) — that is what `opencode acp`
  speaks. The draft v2 surface in 2.1.0 is out of scope.
- SDK client shape (from 2.1.0 docs): `Client.builder().name("...").connect_with(transport, ...)`,
  `ActiveSession` for prompt/update flow, typed handlers for inbound client requests.
- `NewSessionRequest` in 2.1.0 carries exactly what the multi-dir requirement needs:
  - `cwd: PathBuf` — session base, must be absolute
  - `additional_directories: Vec<PathBuf>` — extra workspace roots, each absolute
  - `mcp_servers: Vec<McpServer>` — always empty for v1 of this feature
  - `meta` — reserved; unused
- Protocol quirks to preserve from Zed: agent version capture from `initialize`, capability
  reporting, `UnsupportedVersion` surfaced as a distinct error, trailing-stderr extraction for
  startup failures, and dropping in-flight responses when the connection ends.

---

## 4. `libs/acp-client` crate

### 4.1 Shape and conventions

- New workspace member `libs/acp-client` (covered by the existing `members = ["apps/*", "libs/*"]`).
- Per repo convention (AGENTS.md), specify the library root explicitly:
  `[lib] path = "src/acp_client.rs"` — **not** `lib.rs`, and no `mod.rs` paths.
- Dependencies: `agent-client-protocol`, `gpui` (as `gpui-pre`, workspace-style pin matching
  todo-2), `gpui_tokio`, `tokio`, `serde`/`serde_json`, `anyhow`, `tracing`.
- No license field additions or GPL text: this is a reimplementation, not a copy.
- Error handling follows AGENTS.md: `?` over `unwrap()`, no `let _ =` on fallible calls, errors
  surfaced to the UI layer, no comments that merely restate code.

### 4.2 Modules (all `src/<name>.rs`)

- `acp_client.rs` — crate root: public re-exports, shared error types
  (`AcpError::{UnsupportedVersion, LaunchFailed, InitializationFailed, AuthRequired, ...}`).
- `agent.rs` — the **agent registry**: a small `AgentServer`-style trait (id, display name,
  logo/icon, command resolution, launch) plus the single implementation for opencode. Adding a
  second agent is a new impl, no panel changes.
- `connection.rs` — process launch and connection lifecycle: resolve the binary on `PATH`,
  spawn with `cwd`, wire stdin/stdout as the JSON-RPC transport, capture stderr, run
  `initialize` with v1, expose `new_session` / `load_session` / `prompt` / `cancel` /
  `authenticate`, advertise client capabilities, dispatch inbound requests, drop cleanly.
- `thread.rs` — the thread/transcript model: applies `session/update` notifications into an
  ordered list of entries (user messages, agent text, thought text, tool calls with args and
  status, diffs, plan updates, terminals, notices), tracks turn state, pause/stop handling,
  and available commands / current mode / config options / model list.
- `terminal.rs` — terminal provider implementing the `terminal/*` client requests by spawning
  child processes, streaming their output, and supporting wait/kill/release.
- `fs.rs` — filesystem provider implementing `fs/read_text_file` / `fs/write_text_file`
  directly against the session's roots.
- `permissions.rs` — permission-request handling: translates `session/request_permission` into
  a pending decision, with an auto-approve policy input.
- `session_store.rs` — durable session-id persistence (see §6.3).
- `fake_agent.rs` (test-support only) — an in-process fake ACP agent used by the crate's tests.

### 4.3 Capabilities advertised in `initialize` (`clientCapabilities`)

Both are enabled, matching Zed's level of support:

- **Filesystem: yes.** Serve `fs/read_text_file` and `fs/write_text_file` straight against the
  session's directories. Requests outside the session's roots are refused with an error rather
  than served (no path escapes; canonicalize before comparing).
- **Terminal: yes.** Serve `terminal/create`, `terminal/output`, `terminal/wait_for_exit`,
  `terminal/kill`, `terminal/release` by spawning real child processes. Terminals are killed
  and released when their session ends or the app exits.

Neither capability is advertised when the corresponding provider fails to initialize; the
agent then falls back to doing the work itself.

### 4.4 Process + async integration

- Process spawning and all blocking I/O run on the Tokio runtime via
  `gpui_tokio::Tokio::spawn_result` — never on GPUI's foreground executor.
- Every background task is held in a struct field (`Task<()>` / `Option<Task<()>>`) or
  explicitly detached, so it is not cancelled by being dropped.
- On app/session teardown the child process is killed and reaped; no orphaned `opencode`
  processes survive the app.

---

## 5. The Agent panel in todo-2

> **Companion:** `docs/spec/todo2-agent-pane-ui-spec.md` holds the implementation-facing gpui UI
> detail for this panel — verified component inventory, element trees, transcript row rendering,
> styling tokens, interaction and performance rules. §5 here owns the behaviour.

### 5.1 Where it lives

- A **right-hand split pane**, to the right of the task list — the same region `TaskDetails`
  occupies today.
- The divider between task list and right pane is a **draggable split**; the width is
  remembered per app run.
- The pane is available **only in the Tasks panel** (`NavPanel::Tasks`). Navigating to
  Integrations / Automations / Workflows / Settings hides the pane while leaving sessions
  running in the background.
- When no task is selected, the pane shows the agent full height rather than an empty details
  area.

### 5.2 Panel switching footer

- A **thin, window-wide footer strip** at the bottom of the layout holds **only the pane
  switchers**: tiny `Details` and `Agent` buttons, right-aligned. This is the sole switch
  between the two right-hand panes, in the spirit of Zed's panel footer buttons.
- The navbar keeps its existing footer rows (Automations / Integrations / Workflows /
  Settings) **unchanged**. No Agent row is added to the navbar footer.

### 5.3 Activation rules ("active only on directory-based projects")

- A selected tag is a **directory-backed project** when it is a project tag
  (`Tag::is_project()`, i.e. the `project:{abs-path}` naming) **or** it has a non-empty
  `tag_settings.dirs` list.
- With such a tag selected, the `Agent` switcher is enabled and the pane can be opened.
- Otherwise (All Tasks, plain tags, Todoist-synced tags without dirs): the `Agent` switcher is
  **visible but disabled**, with a tooltip explaining the panel needs a directory-backed
  project. It is not hidden.
- Switching to a tag that is not directory-backed while the Agent pane is open falls back to
  the Details pane; the previously running session keeps running (§5.7).

### 5.4 Launch directory resolution

- Candidate directories, in order:
  1. the path encoded in the tag name (`project:{abs-path}`), when present,
  2. then each entry of `tag_settings.dirs`, in list order.
- **`cwd` = the first candidate that resolves to an existing directory**; candidates that do
  not exist are skipped.
- **`additional_directories` = the remaining existing candidates** (canonicalized, absolute).
- If no candidate exists on disk, the pane shows a clear error and does not spawn anything.

### 5.5 Agent process and session lifecycle

- **One process per directory project, kept alive.** The first time a project's Agent pane
  opens, `opencode acp` is spawned for it; the process and its session stay alive until the app
  exits (or the project's tag is deleted — see §9).
- **Launch command: `opencode acp`, hardcoded**, resolved via `PATH`. This matches opencode's
  documented ACP entry point (the user's `opencode --acp` phrasing is superseded by this
  decision). No settings override and no probe-and-remember in v1; the command lives in the
  registry impl so it is a one-line change later.
- The process is spawned **in the project directory** via the resolved `cwd`.
- Process launch happens on first pane open, never at app startup.
- Model/mode/config selections are applied to the session after creation and re-applied on
  resume where the agent advertises support.

### 5.6 Session continuity (`resume across restarts`)

- Session ids are persisted so a restart can attempt `session/load` instead of losing the
  conversation.
- **Store: `~/.config/my-todo/acp-sessions.json`**, the same config dir the Todoist token uses
  (`todoist_auth::config_dir()`, honouring the `MY_TODO_CONFIG_DIR` override). The storage DB is
  `turso::memory:` and is not used for this.
- Record shape (JSON, versioned): per project — project path (primary key), tag id (display),
  session id, updated-at, and the project's auto-approve preference (§5.8). Keying on the
  **path** keeps entries stable when a tag is renamed.
- On pane open: if a stored id exists **and** the agent advertises `load_session`, attempt
  `session/load`; otherwise `session/new`.
- **If `session/load` fails** (unsupported, stale id, agent reinstalled): start a new session
  and **surface the error prominently** in the panel — never silently. The stale id is
  replaced by the new one.
- A **"New session" button in the transcript header** starts a fresh `session/new`, replaces
  the stored id, and resets the transcript. The header also shows the current session id/title.

### 5.7 Background turns and the busy badge

- Switching to another project while a turn is streaming does **not** cancel it. The turn keeps
  running and its transcript is kept warm for when you navigate back.
- The busy project's **navbar tag row shows an animated dot** while a turn runs.
- **Clicking a busy project's row opens that project with the Agent pane focused.**
- The dot is per-row, at whatever depth the project tag sits (the navbar already renders
  per-tag decorations such as the provider sync badge, so this follows that pattern).
- Stop is always explicit: the pane's Stop control sends `session/cancel` for that session.

### 5.8 Permissions

- `session/request_permission` becomes an inline decision in the transcript, rendered from the
  **options the agent offered** (`PermissionOption { option_id, name, kind }`), not from
  hardcoded Allow/Deny buttons (see §9.5). Nothing proceeds until a decision is made.
- An **"Auto-approve tool calls" toggle** sits with the pane controls. Default is **off**
  (prompt), and the setting is **persisted per project** in the same config file as the
  session ids — a trusted project stays auto-approved.
- When the toggle is on, requests are answered automatically with the strongest allow option
  the agent offered; the transcript still records that an approval was auto-granted.

### 5.9 Transcript rendering

Full Zed-style richness, built on our own thread model:

- Streamed agent text and thought text (thoughts visually de-emphasized).
- **Tool calls** as entries with an icon, title/kind, a one-line summary, expandable
  arguments/result, and a status (pending / running / completed / failed).
- **Unified diffs** for file-edit tool calls, rendered in the diff style of Zed's
  `acp_thread/diff.rs`.
- **Inline embedded terminals**: `terminal/*` output renders as a live, per-command terminal
  widget inside the transcript entry (this is why terminal support is advertised — see §4.3).
- Plan updates, mode changes, and session config changes render as compact notices.
- Notices for auth-required, resume failures, and launch errors (§5.10).
- Markdown is used for agent text rendering, consistent with the app's gpui-component UI.

### 5.10 Errors and failures

- **Launch/startup failures** (opencode not on `PATH`, exits before `initialize` completes,
  protocol version unsupported) show an **inline error state in the panel body** including the
  captured stderr, plus a **Retry** button.
- The same failures are **also reported through a window notification (toast)**, because the
  panel is not necessarily the visible surface at the time.
- **Auth required** (`auth_required` from the agent, or an `authenticate` handshake) renders as
  its own in-panel state with the agent's advertised auth methods.
- **No auto-retry with backoff** in v1: one spawn attempt, then the error state.

### 5.11 Controls

All of the following are in scope for v1:

- **Prompt input** — multi-line text box, send on Enter (shift+Enter for newline), queued
  messages while a turn is running, and a **Stop** control that issues `session/cancel`.
- **Model selector** — driven by the agent's model-shaped session config option, since ACP
  2.1.0 has no `session/set_model` (§9.2); other select-valued config options render as
  additional controls.
- **Slash commands** — opencode's advertised commands (`available_commands_update`, §9.2):
  - typing `/` at the **start of the input** opens a filtered dropdown above the input;
  - picking a command fills the input so arguments can be appended before sending.
- **Session modes** — the agent's advertised modes (e.g. plan/build), shown as a switch, with
  the current mode reflected in the pane.
- **Auth** — the auth-required flow from §5.10.
- **New session** — the header button from §5.6.

### 5.12 Task context ("only one task at a time, designed for more")

- There is **no automatic context injection**. The agent is a plain coding agent by default.
- An explicit **"Send task to agent"** action exists in **two places**:
  1. a small button in the TaskDetails pane, and
  2. a small "attach task" button next to the prompt input.
- Both **fill the prompt box** with the currently selected task's context; neither sends
  immediately, so the message can be edited first.
- The context is the **current task only** (title plus its details/notes/subtasks as
  available). The design keeps context assembly behind a single "build task context" seam so
  multiple tasks (or project-wide task lists) can be added later without restructuring.

---

## 6. Testing and verification

- `libs/acp-client` ships **unit tests backed by an in-process fake ACP agent**
  (`fake_agent.rs`, mirroring Zed's `FakeAcpAgentServer` approach). Coverage:
  - `initialize` handshake incl. unsupported-version failure,
  - `session/new` and `session/load`, incl. load-failure fallback,
  - prompt turns: streaming agent text, thought chunks, tool calls, tool-call updates,
  - `session/request_permission` → allow / deny / auto-approve,
  - `session/cancel` mid-turn,
  - terminal client requests (`terminal/create` → output → wait → kill/release),
  - fs client requests, including a refused out-of-root path,
  - process exit mid-session surfacing as an error state.
- Pure-logic tests for: launch-directory resolution (first existing candidate →
  `cwd`, rest → `additional_directories`, missing dirs skipped) and session-id persistence
  (write/read/update, resume-failure replacement).
- GPUI tests follow the repo's timing rule: `cx.background_executor().timer(...)` for waits
  that `run_until_parked()` must observe — not `smol::Timer`.
- Manual verification path: run todo-2, select a `project:{path}` tag, open the Agent pane,
  confirm `opencode acp` spawns with the right cwd, stream a prompt, exercise permissions,
  restart the app, confirm the session resumes.
- Optional dev aid (not a deliverable): this repo's own `apps/agent-cli --acp` implements an
  ACP v1 server and can serve as a second local test target for the client.

---

## 7. Non-goals (v1)

- No second agent implementation; opencode only, behind a registry trait.
- No ACP-registry install/discovery flow (Zed's "install agent from registry" UX).
- No verbatim Zed source, and no `patched/` vendoring of Zed crates.
- No `@`-mention / file-reference plumbing from `acp_thread/mention.rs`.
- No MCP servers attached to sessions (`mcp_servers` stays empty).
- No agent awareness of anything beyond the single currently-selected task.
- No headless/no-UI terminal mode (terminals are rendered inline per §5.9).
- No terminal rendering outside the transcript, and no standalone terminal pane.
- No pane availability outside the Tasks panel.
- No configurable launch command or agent list in Settings.
- No auto-retry/backoff on launch failure.
- No path override for the session store.
- No persisted pane width or other UI geometry (see §9.4).
- No protocol/debug message view (Zed's `AcpDebugMessage` streaming UI); captured stderr is
  surfaced only through the error state (see §9.7).
- No stdin forwarding into agent terminals; terminals are output-only.

---

## 8. Implementation notes / constraints

- Respect the repo's GPUI/AEGIS conventions in AGENTS.md: `Context`/`Window` conventions,
  `cx.listener` for handlers, `cx.notify()` on state change, `Subscription` held in fields,
  entities updated through the closure's `cx`, `_async_task: Option<Task<()>>` for owned work.
- Do not update an entity while it is already being updated (GPUI panics).
- Long-running per-project sessions must be keyed by entity/`WeakEntity` handles and dropped
  when a project's tag is deleted, otherwise sessions leak.
- The pane's switch buttons live in the layout footer, so the Layout must own the pane-state
  (which right pane is active, per project) and propagate focus to the agent view.
- The busy-dot state belongs to the navbar, so the agent panel needs to notify the navbar (via
  an event subscription, like `IntegrationsEvent::Changed` → `refresh_tags`) when a turn starts
  and ends.

---

## 9. Decisions on the previously open questions

Every question from the first revision is decided here. Each entry gives the rule, then why it
went that way. Anything that remains genuinely unresolved is in §9.8.

### 9.1 Tag deletion / directory removal → tear the session down

Rules:

- The pane **re-resolves the directory candidates every time it opens**, and again whenever the
  navbar tag tree refreshes.
- **No candidate resolves to an existing directory** (§5.4): show the error state and, if a
  session for that project is live, send `session/cancel`, close the session, kill the process,
  and delete the project's record from the session store.
- **Some directories vanish but at least one remains**: keep the session and process alive. The
  change only affects the *next* session created for that project, because `cwd` and
  `additional_directories` are fixed at `session/new` / `session/load` time and cannot be
  mutated mid-session.
- **The project's tag row disappears** (deleted): the same teardown as "no candidate resolves",
  driven by the navbar refresh path rather than by an explicit delete callback.

Why: a session whose working directory no longer exists cannot do anything useful, and leaving
orphaned agent processes that keep editing a removed project is strictly worse than losing the
transcript. Note that todo-2 has **no tag-deletion UI today** (only trip cleanup calls
`storage::delete_tag`), so this rule is mostly future-proofing plus the directory-removal case,
which *is* reachable through `tag_settings.dirs`.

### 9.2 Zed parity mapped onto ACP 2.1.0

Zed's `0.11.1` call sites do not all survive into `2.1.0`. The concrete mapping:

| Concern | Decision |
| --- | --- |
| Session modes | `SetSessionModeRequest`; reflect changes from `SessionUpdate::CurrentModeUpdate` |
| Model selection | **There is no `session/set_model` in 2.1.0.** Models are a **session config option**: `SessionConfigOption` + `SetSessionConfigOptionRequest`, changes arriving as `SessionUpdate::ConfigOptionUpdate` |
| Config options UI | Render **all** select-valued config options generically; present the one that is model-shaped as the "model" control and the rest as additional controls |
| Slash commands | `SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate { available_commands })`, stored on the thread and feeding the `/` picker |
| Context/cost | Render `SessionUpdate::UsageUpdate` as a compact right-aligned label near the prompt box |
| Session title | `SessionUpdate::SessionInfoUpdate` supplies the transcript header title and timestamps |
| Plans | Stable `SessionUpdate::Plan` only |
| Unstable variants | `PlanUpdate`, `PlanRemoved`, `CompactionUpdate`, `CompactionSummaryChunk` are feature-gated — **do not enable `unstable_*` features**; ignore the variants with a wildcard arm |
| Auth | Handle both advertised method shapes: `AuthMethodAgent` (agent authenticates itself via the `authenticate` handshake) and `AuthMethodTerminal` (client must run a command) |
| Auth terminal | Reuse the embedded terminal widget (§5.9) to run the advertised auth command instead of just showing text |

Model ids are opaque and provider-qualified (opencode uses ids like
`opencode/minimax-m2.5-free`), so display them verbatim rather than reformatting.

### 9.3 Terminal shell and environment → mirror Zed's behaviour

- The `terminal/create` command string is executed **through the user's shell**, preferring
  bash: `$SHELL` when set and executable, else `bash` on `PATH`, else `/bin/sh`. Windows uses
  `cmd /C` (macOS is the current target, so the Windows path just needs to not be wrong).
- `cwd` comes from the request and must canonicalize **inside one of the session's roots**;
  requests pointing outside are refused with an error, same policy as the fs provider (§4.3).
- Environment = inherited app environment, plus `PAGER=""` and `GIT_PAGER=cat`, with the
  request's own `env` entries applied last. This mirrors Zed and prevents agent commands from
  hanging behind an interactive pager.
- stdin is not forwarded: terminals are output-only in v1. Output streams into the embedded
  terminal widget; `terminal/kill` terminates, `terminal/release` reaps.

### 9.4 Split width → in-memory only

- The draggable split's width is remembered **for the app run only**, exactly as interviewed.
- It is deliberately **not** written to `acp-sessions.json`: that file is a session/approval
  store, and mixing window geometry into it would couple unrelated concerns. If the Settings
  panel grows a home for window layout later, move it there.
- **Resolved: no hand-rolled drag handle.** `gpui-component` 0.6.1 does ship a splitter —
  `gpui_component::{h_resizable, resizable_panel, ResizableState, ResizablePanelEvent}`, which it
  re-exports from `gpui-base` (verified in the vendored crate sources). It provides the divider,
  hit area, cursor and drag itself.
- **The "app run" lifetime needs no code.** When no state entity is supplied, the group keeps
  its state in `window.use_keyed_state(id, cx, …)`, i.e. keyed state that lives as long as the
  window — which is exactly the interviewed behaviour. An explicit `Entity<ResizableState>` may
  be held on `Layout` for nameability and future `on_resize` use; either way nothing is written
  to disk.
- See `todo2-agent-pane-ui-spec.md` §2.2 for the concrete element tree (two fixed panels, the
  right one collapsed with `.visible(false)` so keyed state stays coherent).

### 9.5 Auto-approve granularity → driven by the agent's own options

- Decision buttons are the **agent's offered options**, rendered by their `name` and styled by
  their kind: `AllowOnce`, `AllowAlways`, `RejectOnce`, `RejectAlways`.
- **Toggle on**: auto-reply with the first option whose kind is `AllowAlways`, falling back to
  `AllowOnce`. If the agent offers **no** allow option, surface the request to the user anyway —
  never auto-reject.
- **Toggle off**: the user picks exactly one offered option.
- **No client-side sticky deny.** Sticky semantics already exist in the protocol as
  `RejectAlways` / `AllowAlways`, which the *agent* remembers. Duplicating that client-side
  would double-remember and drift out of sync. The per-project auto-approve toggle stays the
  only client-side approval memory.

### 9.6 Prompt queue → cap 10, session-scoped, cleared by Stop

- Maximum **10** queued messages, held in memory and scoped to the **session** (not the pane),
  drained FIFO automatically when a turn completes.
- At the cap, Send is disabled with a "queue full" hint.
- **Stop (`session/cancel`) clears the queue** and posts a transcript notice reporting how many
  queued messages were dropped.
- The queue is never persisted; a session resumed after a restart starts with an empty queue.
- Closing the pane does not clear it (the session is still alive); tearing the session down
  does.

### 9.7 Stderr capture → 250-line ring buffer, error state only

- Each session keeps a bounded ring buffer of the **last 250 stderr lines** (Zed retains 2000;
  we only need enough recent context to explain a failure, and we render it in exactly one
  place).
- The buffer is rendered in full in the launch/session **error state** (§5.10). The toast shows
  the first line plus a pointer to the panel.
- No protocol message log and no debug view in v1 (Zed's `AcpDebugMessage` streaming UI is a
  non-goal, §7). The buffer lives behind a small API in `connection.rs` so a debug view stays
  cheap to add later.

### 9.8 Still open at implementation time (non-blocking)

- Whether opencode actually exercises the client's terminal capability: Zed's own tracker has
  users reporting that opencode runs commands internally instead of asking the client to, so
  the embedded terminal may be exercised mainly by other agents and by auth flows. Terminal
  support stays advertised regardless — it is capability-gated and harmless.
- Exact rendering of `SessionConfigOption` value types beyond selects (boolean/number) if
  opencode advertises any; v1 can render only selects and hide the rest.

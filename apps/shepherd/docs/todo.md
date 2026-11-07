# Shepherd implementation plan

Status: proposed, 2026-11-04.

Specs: [goal.md](./spec/goal.md), [master-spec.md](./spec/master-spec.md),
[stack.md](./spec/stack.md), [workspace-ui.md](./spec/workspace-ui.md). This
file gives the working order. The specs govern behaviour and technology.
Check the workspace UI decisions when implementing a slice that touches the
shell.

## Next feature

- [x] Import the primary folders of local Codex projects from the ChatGPT desktop
  app. Offer batch import in the sidebar when the Projects list is empty. A
  protocol fixture covers pagination, and the browser smoke covers batch import.

## The ordering rule

This plan is ordered for **evidence**, not for architecture layers.

The most expensive mistakes are not visual: they are the task lifecycle, the
agent protocol, the worktree/review loop and the terminal engine. Those are the
parts that can invalidate the product or force a rewrite. Build one thin path
through them first, then deepen it.

Each slice ends with a gate:

1. an automated proof,
2. a short manual desktop script on a disposable repository,
3. a written go/no-go decision.

Do not start the next slice because the code compiles. Start it because the gate
passed.

If only four slices can be built, build **S0–S3**. That is the smallest version
of the product that can answer: “Can I run a real agent, review what it changed,
and merge it without leaving Shepherd?”

## Current starting point

| Area | Current state |
| --- | --- |
| Workspace | Root Cargo workspace includes `apps/*`, `libs/*` and `demos/*`. |
| `apps/shepherd` | Tauri 2 shell: `src-tauri/` (window, generated desktop icons, default capability, command bridge, single event channel, durable core in the app data dir) and `ui/` (SolidJS + TypeScript + Vite, typed client in `src/bridge.ts`). `pnpm dev:shepherd` starts Vite on port 1420 and opens the window. At startup the shell detects Codex (configured path, `PATH`, then the macOS app bundle) and runs it through the `app-server` adapter; when detection finds nothing, prompting fails with the detection report instead of falling back to the fake. The scripted adapter remains for the shell's tests. The fixed regions are not built yet. |
| `libs/shepherd-core` | Exists: command/event API, snapshots, task lifecycle, shutdown, harness session, Git worktree, terminal trait boundary, embedded migrations (`0001_init.sql`, `0002_workspace.sql`, `0003_sessions.sql`), a durable `TursoStore`, shared fixtures, a `test-support` scenario trace and the dependency-boundary script. 36 focused tests pass plus two ignored live runs, including the full fake-task lifecycle: send, approval, cancel, quit, reload and resume. `Core::load` opens durable records, marks every live task `interrupted`, and keeps the newest agent session per task so resume asks the adapter for the thread the previous run opened; transcript rows are still not reloaded. |
| `libs/agent-harness` | Exists: adapter contract, event model, transcript assembly and a deterministic fake harness. 38 focused tests pass, two of them ignored live runs against the installed Codex. |
| `libs/acp-client` | Exists, with protocol types, permissions, fs/terminal capabilities and a fake agent behind `test-support`. Reuse it; do not rewrite ACP. |
| `libs/storage` | Existing reference for `toasty` + Turso wiring and embedded migrations. Shepherd gets its own database and schema. |
| Frontend | Shepherd UI package exists at `apps/shepherd/ui` (SolidJS + Vite 8); `apps/shepherd` and `apps/shepherd/ui` are pnpm workspace members. Root uses `pnpm` 11.25. |
| Tests | `agent-harness` has 38 focused tests (two are ignored live runs against the installed Codex; the other 36 run without a CLI, and 20 of them drive the Codex adapter through stub binaries; three replay recorded wire fixtures from `libs/agent-harness/fixtures/codex` with no CLI at all; the rest cover probe tiers, install detection and its fallbacks, message and tool mapping including a restore that reattaches an item, an approval round trip including a file-change diff, an interrupt, a resume that names the stored thread ID, the `exec --json` fallback with its stream mapping and cancel, and a denied approval as `decline`); `shepherd-core` has 37 plus two ignored live runs (the four extra cover the project scan: repository discovery and descent stopping, chain collapsing, branch grouping, and skipped folders; two more cover resuming the stored session after a restart and the explicit fallback when the adapter cannot resume it; three more cover the failure rows for a missing binary, a rejected prompt and a mid-turn exit; one covers the diagnostics naming the command and protocol, one covers denying an approval, and one covers the pump that folds adapter events arriving between commands); the Tauri shell has 9 tests (bridge commands, snapshot serialization, durable load, the PTY engine's output/exit code/close, and the VT parser's text, SGR and resize behaviour), plus 5 Playwright smokes in `apps/shepherd/ui/tests`. The existing ACP tests remain the closest adapter harness. `scripts/check-shepherd-boundary.sh` enforces the core boundary. |

### Toolchain check (2026-11-04)

| Tool | Status | What to do |
| --- | --- | --- |
| `cargo` | 1.95.0 works | Keep. |
| `rustc` / `rustfmt` | Not directly on `PATH`; the Nix Cargo wrapper supplies `rustc`, and `rustfmt` is available in the Nix store | ACP tests and Shepherd checks pass through the wrapper. Use the Nix dev shell for direct compiler and formatter commands before S0. |
| `pnpm` | 11.25.0 | Use it for the v1 UI. |
| Node | 24.21.0 | Fine for Vite/Tauri tooling. |
| `zig` | In the flake's build dependencies (`pkgs.zig`) | Builds `libghostty-vt` at build time only. The shipped app statically links the archive, so end users install nothing. Confirm `pkgs.zig` satisfies the bindings' required Zig version before the engine spike. |
| `codex` | Not on `PATH`. The ChatGPT app bundle (`com.openai.codex`) ships codex-cli 0.166.0-alpha.6 at `/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex`; the older `Contents/Resources/codex` path is absent. | Adapter resolution must find the bundle binary. S2 can use recorded fixtures and a fake server, but the S2 gate requires a live `codex` run. |
| Git | 2.50.1 | Use temporary repositories for every automated Git test. |

## Priority map

| Priority | Slice | Why it matters |
| --- | --- | --- |
| P0 | S0 — behavioural core, no window | Proves the state machine, persistence and shutdown before UI work. |
| P0 | S1 — clickable walking skeleton | Proves the shell/core bridge, draft task, fixed regions and one PTY. |
| P0 | S2 — real Codex task | Proves the highest-risk protocol and the real agent loop. |
| P0 | S3 — worktree, review and Git | Proves the product differentiator: an agent’s work is reviewable and mergeable. |
| P1 | S4 — terminal multiplexer depth | Proves the app is a real terminal multiplexer, not one embedded shell. |
| P2 | S5 — adapter breadth | Proves the adapter model beyond one CLI. |
| P2 | S6 — workspace breadth | Files, preview, search, attachments, notifications, settings. |
| P3 | S7 — hardening and release | Crash recovery, GC, performance, packaging. |

S0–S3 deliberately pull the Codex adapter and review/Git loop ahead of the full
terminal layout. One terminal in S1 is enough to prove the PTY/canvas path. The
full project terminal arrangement is not on the critical path to proving that a
task can produce reviewable work.

## Sequence at a glance

| Slice | User-visible proof | Main automated proof | Gate |
| --- | --- | --- | --- |
| S0 | No window; a fake task runs and survives restart in a test | `cargo test -p shepherd-core`, fake harness, temp Git repo | A complete fake task lifecycle passes headlessly. |
| S1 | Add a repo, create a draft, send a fake task, open a terminal | Core integration tests + Playwright shell smoke | The walking skeleton is usable without a crash. |
| S2 | A real Codex task streams, approves, cancels and resumes | Codex fixtures + fake server + live smoke | Real Codex works on a disposable repo. |
| S3 | A Codex task reaches review, commits and squash-merges | Temp-repo Git tests + review-trigger tests | One real task goes prompt → review → merge in Shepherd. |
| S4 | Multiple terminals, shared and task-scoped, survive task switches | Parser fixtures, layout round-trip, terminal tab UI tests | Terminal use is daily-driver quality. |
| S5 | OpenCode, Pi and Claude/ACP work or degrade honestly. Freebuff opens a plain terminal. | Adapter contract and live smokes for integrated agents, plus a terminal fixture for Freebuff | Each adapter reports only the state it can observe. |
| S6 | Files, preview/annotation, search, attachments, notifications, settings | UI and feature tests | The v1 feature checklist is complete. |
| S7 | Crash recovery, bounded resources, packaging and release checks | Chaos, performance, migration and packaging tests | v1 is trustworthy on the target platforms. |

---

## S0 — Behavioural core, no window

**Goal:** a fake task can be created, streamed, approved, cancelled, quit,
restarted and resumed entirely in tests.

### Build

- [x] Create `libs/agent-harness` with `[lib] path = "src/agent_harness.rs"`:
  `AgentHarness`, `HarnessEvent`, capabilities, transcript entry types.
- [x] Create `libs/shepherd-core` with `[lib] path = "src/shepherd_core.rs"`:
  `Core`, `Command`, `Event`, snapshots, lifecycle and shutdown state.
- [x] Implement `projects`, `tasks`, `harness`, `git`, `store` and `input`
  boundaries. Keep terminal behind a trait; do not build the full terminal UI.
- [x] Implement the task state machine: `draft → starting → ready → working ⇄
  waiting → review → done | failed | cancelled | interrupted`.
- [x] Add `0001_init.sql` for projects, tasks, sessions, entries and indexes.
  Add the workspace/terminal tables in S1, when the first terminal is persisted.
  S4 extends that layout; it does not invent a second persistence model.
- [x] Add a deterministic fake harness that can emit `TurnStarted`, deltas, a
  tool call, `ApprovalRequested`, `TurnFinished` and `Exited`.
- [x] Add temp-directory and temp-Git-repository fixtures. Every test must opt
  into its own data directory; no test writes to the real app data directory.
- [x] Add a test-support feature for scenario traces so a failed test prints the
  command/event order, not just an assertion.
- [x] Add the boundary check: `cargo tree -p shepherd-core` must not contain
  `tauri`, `gpui` or a webview crate.

### Proof

Run:

```sh
cargo fmt --check
cargo test -p agent-harness
cargo test -p shepherd-core
cargo test -p acp-client --features test-support
direnv exec . ./scripts/check-shepherd-boundary.sh
```

The core scenarios must cover:

- [x] add project, duplicate path rejected, missing path marked;
- [x] new draft appears as a task, creates no worktree and no branch;
- [x] first send creates the worktree and starts the task;
- [x] fake stream assembles into transcript entries with stable IDs and order;
- [x] approval moves the task to `waiting`, and a decision resumes it;
- [x] cancel keeps the session and worktree;
- [x] quit with a live fake agent marks the task `interrupted`;
- [x] restart resumes the stored agent session and says plainly when it is gone;
- [x] a chat-only turn returns to `ready`;
- [x] the state machine can enter `review` from a completed changed turn (the
  real diff-based trigger is tested in S3).

### Gate

**Pass:** the headless fake task completes the full lifecycle, including a
simulated quit and restart.

**If it fails:** stop. Do not build the Tauri shell around a state machine that
cannot survive a restart. Fix the command/event contract, persistence or
shutdown first.

**Course-correction note:** if the core needs shell types to express fixed
regions, terminal scope or task switching, the boundary is wrong. Fix it here.

---

## S1 — Clickable walking skeleton

**Goal:** a real window where a user can add a project, create a draft, send a
fake task, read the stream and use one terminal.

### Build

- [x] Scaffold `apps/shepherd/src-tauri/` for Tauri 2, and `apps/shepherd/ui/`
  for SolidJS + TypeScript + Vite.
- [x] Add the UI package to `pnpm-workspace.yaml`; document one command that
  starts the app.
- [x] Wire one `#[tauri::command]` per core command and one Tauri channel for
  events. Use typed snapshots; do not build a second frontend model.
- [x] Build the fixed shell: sidebar, required chat, always-on inspector, status
  bar. The inspector has tabs, but only Terminals is enabled; Preview, Review,
  Git and Files are named placeholders.
- [x] Implement project add/list, draft creation, send prompt, task select and
  quit.
- [x] Add `0002_workspace.sql` for terminals, project terminal layouts and task
  workspace layouts; the one terminal must survive an app restart as an ended
  terminal. (The restart clause is proven by the terminal spike below.)
- [x] Implement the first terminal spike: `TerminalEngine` boundary,
  `portable-pty`, one PTY and a canvas. Do not build splits or the project
  terminal layout yet.
- [x] Enforce draft rules: the draft appears immediately, shows its proposed
  branch and worktree path, creates neither, and is discarded without Git
  traces.
- [x] Implement quit confirmation for a working/waiting fake task; mark
  interrupted on restart; show a terminal as ended with a restart action.
- [x] Offer **Resume** in the conversation header for an interrupted task.
  The browser smoke verifies the state change and the composer becoming ready.
- [x] Add a project directory picker, modeled on taskstream's
  (`apps/taskstream-desktop/src/projects.rs` for the scan,
  `src/ui_parts/project_picker.rs` for the dialog). Replace the sidebar's path
  text input with an **Add project** button that opens a picker. Scan `~` for
  Git repositories as the app starts, on a background task: bounded depth,
  skipping junk (`node_modules`, `target`, `.cargo`, `Library`, `.Trash`, …),
  excluding the home directories `~/.claude`, `~/.codex` and `~/ComfyUI-Installs`,
  and stopping at a repository root so repos nested inside a checkout are not
  listed twice. Cluster the results into a tree rather than a flat list:
  collapse chains of single-child directories, skip levels with a lone folder,
  and list all siblings. Expand the first level by default and let folders
  expand and collapse in place. Add a filter field, arrow-key navigation,
  Enter to open and Esc
  to cancel; cache the scan for the session and show loading, empty and error
  states plus a re-scan. Selecting a directory calls the existing `add_project`
  command with its path, and the project is named after the directory.
  Proof: a unit test for the scan and clustering over temp directories, and a
  Playwright smoke that opens the picker, filters, and adds a project.

**macOS scan caveat:** a scan rooted at `~` can reach TCC-protected folders
(Documents, Desktop, Downloads, Pictures, Music, Movies), which makes a bundled
app prompt for access. Taskstream avoids that by rooting at `~/src`; the
Shepherd scan must either skip those folders explicitly or keep a configurable
root. Paths outside `~` are not reachable from the picker once the text input is
replaced, so that trade-off is recorded here rather than discovered later.

**Terminal risk decision:** the `libghostty-vt` spike comes before the canvas UI
is treated as done. If the pinned Zig build blocks on this machine, record the
reason and use `alacritty_terminal` behind `TerminalEngine` for the slice; that is
a validated fallback, not a silent swap.

**Recorded fallback:** `zig` is not installed on this machine (`zig version` is
not found), so the spike is built on `alacritty_terminal 0.26` behind the core's
`TerminalService` boundary, with `portable-pty` for the PTY.

### Proof

Automated:

- [x] Core integration test: add project → draft → send → stream → quit →
  restart.
- [x] Playwright smoke: add project, flat task row, draft row, send, Terminals
  tab, task switch.
- [x] Terminal fixture: feed bytes, assert parser state, input encoding and
  resize; no pixel assertions.

Manual script on a disposable repo:

1. Add the repo and confirm it appears in the flat task list.
2. Create a task; confirm the sidebar row exists and no worktree/branch exists.
3. Send a fake prompt; confirm streaming text and a tool row.
4. Open Terminals, run `pwd`, `printf 'hello\n'`, and `Ctrl-C`.
5. Switch to another task and back; the draft/transcript is still coherent.
6. Quit with a live fake turn; confirm the warning names the work.
7. Relaunch; confirm the interrupted state, the ended terminal and Resume.

### Gate

**Pass:** the skeleton is usable for the whole fake-task loop, and quitting is
honest.

The macOS desktop smoke drives this loop in a disposable repository across
two native launches. It confirms the quit warning while a task awaits approval
and uses the restored task's **Resume** action after relaunch.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh
```

**If it fails:** stop before Codex. This is where shell/core coupling, fixed
regions and draft semantics are cheapest to correct.

---

## S2 — Real Codex task end to end

**Goal:** a real installed Codex CLI produces a streamed transcript, a tool call,
an approval, a cancel and a resume inside Shepherd.

### Build

- [x] Implement `codex app-server` probing and JSON-RPC: thread start, turn
  start, streaming items, approvals, interrupt and thread resume.
- [x] Implement the `codex exec --json` fallback for older versions.
- [x] Resolve the Codex executable, since the ChatGPT app does not add it to
  `PATH`: the profile's path, then `PATH`, then the macOS bundle's packaged
  layouts, then report which installation kind was found — or that none is — with
  the path, version and verified tier.
- [x] Map Codex item IDs to canonical transcript IDs; preserve order after
  reconnect.
- [x] Render approval cards with tool, arguments and a diff when the adapter or
  worktree provides one.
- [x] Persist the session/thread ID; resume on restart; show an explicit
  fallback when the session is gone.
- [x] Implement failure rows for missing binary, unsupported version, auth
  required, protocol error and mid-turn exit.
- [x] Show the exact command, protocol version and stderr tail in diagnostics.
- [x] Add a fake Codex server and recorded fixtures so CI does not need the CLI.
- [x] Run the detected Codex CLI from the desktop shell, and fail with the
  detection report when no usable installation is found.
- [x] Stream adapter events between commands (a core pump plus a shell tick), so
  a live turn paints without waiting for another invoke.
- [x] Keep the turn live after the adapter accepts cancellation, until the
  adapter reports that the turn ended. Publish cancellation progress in the
  workspace snapshot.
- [x] Expose **Cancel turn** in the conversation header and through its
  keyboard shortcut. The browser smoke verifies the transition through
  **Cancelling…** and the composer re-enabling after the turn ends.
- [x] Retire unanswered Codex approvals when the server resolves the request
  or the turn ends. Mark those cards withdrawn and allow the next prompt.

### Proof

Automated:

```sh
cargo test -p shepherd-core codex
cargo test -p shepherd-core --features test-support
```

Cover:

- [x] probe succeeds and advertises only supported methods;
- [x] probe fails with a useful reason and offers the fallback tier;
- [x] streamed message/tool items map to one canonical entry each;
- [x] approval allow/deny and cancel;
- [x] mid-turn exit becomes `failed`, not `done`;
- [x] resume by thread ID;
- [x] missing binary and auth failure are visible.

Manual script on a throwaway repository:

1. Prompt Codex to change one file.
2. Confirm text streams and the tool row appears.
3. Approve a tool call; confirm the decision is recorded.
4. Start another turn and cancel it.
5. Kill the Codex process mid-turn; confirm `failed` and the stderr tail.
   Covered by the core exit test and the native process-exit smoke below.
6. Restart Shepherd and resume the session.
   Covered by the core restart test and the two-launch native smoke below.

### Gate

**Pass:** a real Codex task works end to end on a disposable repository.

The network approval scenario passed live with GPT-6.1 Terra at `low` effort.
Codex asked before running the command, Shepherd recorded the denial, and the
turn completed. A second run passed by replaying the saved response sequence in
`.cache/shepherd-live/codex/terra-low-v1/network-approval.json`.

The macOS desktop smoke replays that same exchange through the real adapter
and native webview. It sends a prompt from the composer, checks the approval
card shows the command arguments, clicks **Deny**, and waits for the recorded
decision and a ready task. It uses a disposable project and database.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-approval
```

The desktop smoke requires an existing capture. With `--features live-codex`, the ignored
`live_codex_requests_approval_for_a_network_command` test in `agent-harness`
can create it with a ChatGPT-authenticated Codex CLI.

The remaining desktop Codex smokes use their existing captures by default.
Append `--record` to allow a live run to populate a missing capture.

The live macOS smoke covers the desktop approval and cancel steps. It clicks
**Allow** on the network approval, cancels a running `sleep 25` tool, and sends
a follow-up in the same chat. The capture contains three turns and one
`turn/interrupt`, with completion statuses `completed`, `interrupted`, and
`completed`. The first successful run records the exchange with GPT-6.1 Terra
at `low` effort in
`.cache/shepherd-live/codex/terra-low-v1/desktop-approval-cancel.json`. Later
runs replay it in a fresh disposable project and database.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-live
```

The macOS worktree edit smoke sends a prompt from the composer and checks the
streamed text, tool row, and one-file diff in the Review pane. The live run
captured 70 message deltas and the verified patch in
`.cache/shepherd-live/codex/terra-low-v1/desktop-worktree-edit.json`. Replay
applies the captured patch to a disposable worktree before the turn completes,
so the core computes the review state from Git in both runs.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-edit
```

The native restart smoke closes Shepherd during a live command, confirms the
quit warning, and relaunches against the same disposable database. The
interrupted task opens with its original branch and worktree. **Resume**
reopens the original Codex thread and a new turn completes. Both launches
record at GPT-6.1 Terra `low` on their first successful run and replay from
`desktop-before-restart.json` and `desktop-after-restart.json` thereafter.
The captures match the thread opened before quitting, the ID requested on
resume, and the thread returned by Codex.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-restart
```

The native process-exit smoke terminates its own Codex child with SIGKILL
during a running tool call. It verifies the failed task, the **Agent exited
mid-turn** row, and the stderr tail in Diagnostics. The verified response
sequence and exit are recorded in `desktop-mid-turn-exit.json` and replayed
after the smoke requests the exit.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-exit
```

**If it fails:** do not build more adapters or UI breadth. Record the exact
protocol gap and decide: patch the adapter, use the `exec` fallback, or lower
Codex’s advertised capability. The product must not pretend a broken protocol is
working.

---

## S3 — Worktree, review and Git loop

**Goal:** a real task can change files, enter review, show the diff, commit and
squash-merge into the base from inside Shepherd.

### Build

- [x] Resolve the real Git directory and create worktrees at
  `<git-dir>/shepherd/worktrees/<slug>` from the project’s default branch.
- [x] Implement missing-worktree repair, dirty-worktree refusal and serialized
  Git mutations.
- [x] Capture the turn baseline (HEAD/status/diff) and implement the review
  trigger for real changes only.
- [x] Add the Review tab: changed files, status, worktree diff and review notes.
- [x] Add the Git tab: status, stage/unstage, commit, branch display, checkout,
  squash-merge, dirty-root refusal and conflict paths.
- [x] Add review badges on the task row and Review tab, plus the status-bar
  review count.
- [x] Implement **Mark reviewed and archive**, **Reopen** and **Send review
  note**.
- [x] Record the branch tip before worktree collection; never delete branches
  automatically.

### Proof

Automated, all in temporary repositories. Coverage is in the core and Git
service tests, including one workflow that reviews two files, sends a note,
stages, commits, squash-merges, archives, and reopens a collected worktree.
The automated core suite ran on 2026-11-05 with 52 passes. The desktop
review and merge gate is tracked separately.

- [x] Edit-then-revert stays `ready`. The core fixture edits and restores a
  file during the same turn.
- [x] A commit, branch change or content change in an already-dirty file enters
  `review`. Git fixtures verify HEAD and branch baseline changes. The core
  applies the same baseline comparison and verifies the dirty-file transition.
- [x] Worktree creation appears in `git worktree list` and uses the default
  branch even when the root is on another branch. Both behaviours pass in
  temporary Git repositories.
- [x] Stage, unstage and commit in a temporary repository. Signing is disabled
  and hooks are isolated to the fixture. The workflow covers a normal commit
  and the Git test checks a fixture hook and rejection of a stale staging revision.
- [x] A failing hook preserves the attempted commit message and staged revision
  for retry. The Git pane clears the draft message after a successful commit.
- [x] Squash-merge from a clean root. The Git fixture checks the merged contents,
  single parent, commit subject and retained task branch.
- [x] Dirty and off-base roots refuse squash-merge with distinct reasons. The
  fixture also checks that the untracked file survives the refusal.
- [x] Squash-merge reports conflict paths. The Git fixture checks the unresolved
  path and verifies the base tip stays in place.
- [x] Missing worktrees can be repaired without dropping other registrations.
  Dirty worktrees refuse removal, including when the only change is an untracked file.

Manual script on a throwaway repository:

1. Ask Codex to edit two files.
2. Confirm the task enters `review` and the Review tab shows both files.
3. Open a diff, send a review note and confirm a new turn starts.
4. Stage and commit in the Git tab.
5. Squash-merge into the base.
6. Archive the task; confirm the branch remains.
7. Reopen; confirm the worktree is recreated from the branch or recorded tip.

### Gate

**Pass:** one real task goes prompt → review → commit → squash-merge, and the
result is visible without leaving Shepherd.

The native macOS smoke drives both Codex turns, opens the two worktree diffs,
sends the review note, stages and commits in the Git pane, and checks the
confirmation after squash-merging. The fixture checks both file contents on
`main`, the squash commit's single parent, and the retained task commit.
The first successful run captures GPT-6.1 Terra at `low` effort and the two
worktree patches in `desktop-review-merge.json` for replay.

The same smoke archives the task, removes its clean disposable worktree
between launches, and checks that the task branch still points to its commit.
The second launch uses **Show archived** and **Reopen** to recreate the
worktree at that exact commit.

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --codex-review-merge
```

**If it fails:** stop. Do not add terminal breadth or adapters until the review
and merge path is correct. This is the product’s main differentiator.

**Safety:** every destructive manual test uses a disposable repository. Never
run squash-merge or worktree removal against a real working tree.

---

## S4 — Terminal multiplexer depth

**Goal:** the Terminals tab is a daily-driver terminal workspace: split, shared,
task-scoped, persistent and fast enough.

### Build

- [ ] Complete `TerminalEngine`: scrollback, alternate screen, wide characters,
  reflow, Kitty keyboard protocol, selection, copy/paste, search, mouse and
  hyperlinks. Kitty modes negotiate independently for each screen, including
  when the screen switch is part of a synchronized update. Fixtures cover
  reflow and key encoding. The native smoke drives Vim, less and
  composition events. Native PTY probes verify Kitty negotiation and key
  reports, including the distinct replies on each side of a synchronized
  screen switch, plus bracketed paste and SGR mouse reports. Interactive fish verifies
  distinct Ctrl+I and Tab bindings. Native IBus Pinyin verifies candidate
  selection and placement on Ubuntu. A candidate-based input source still
  needs a pass on macOS.
  The native PTY probe checks Kitty releases after lifting a shortcut modifier
  and after leaving the terminal. A browser regression checks that focus loss
  releases every reported key once.
  The PTY probe verifies that enhanced Kitty mode preserves modifiers and
  reports releases for Enter, Tab and Backspace shortcuts. Their unmodified
  forms still send the legacy bytes when all-key reporting is off. The browser
  checks Ctrl+Backspace and Alt+Shift+Tab in legacy mode.
  The legacy input regression checks the ASCII control aliases, including
  Ctrl+/ for shell undo, and verifies that non-ASCII keys keep their text.
  A browser regression checks that new output preserves a scrollback selection
  while its visible cells stay in place, and clears it when those cells change.
  Double-click selection follows words across soft wraps, including wide
  glyphs at the wrap, and stops at a hard line break.
  A native macOS probe confirmed that Command+C dispatches a trusted copy
  event from the focused terminal. Browser regressions intercept the copied
  text for Command+C and Ctrl+Shift+C, and verify that Ctrl+C still sends an
  interrupt byte to the PTY.
  Terminal search follows the matched text while output scrolls. The native
  smoke checks the canvas highlight and searches in both directions after a
  100-line PTY burst.
  Overwriting the match discards its anchor, including when synchronized
  output flushes. Redrawing the same text later leaves the old highlight cleared.
  The [pinned Alacritty fix](https://github.com/alacritty/alacritty/commit/ede2ac144da4dec4c075bfa803aacf3b3739bce6)
  caps combining marks at nine per grid cell. The regression feeds 16,384
  marks in separate chunks and checks the grid, the frame and the following
  wide glyph. The pinned parser passed all 36 regular Rust tests and both
  desktop launches on macOS and emulated Ubuntu x86-64. The Ubuntu run also
  passed the native IBus probe.
- [x] Add the binary frame protocol, dirty-row rendering and a frame smoke test.
- [x] Build the Terminals tab split tree, resize, focus and presets.
  Single and Quad save through the project layout, preserve slots for other
  tasks, and keep existing terminal processes available.
- [x] Add the project terminal layout: slots, sizes, order and scope; save it
  per project.
- [x] Store the inspector width independently for each task using its workspace
  layout. The Turso round-trip covers an existing layout and a task with default
  dimensions.
- [x] Resize the inspector with the pointer or arrow keys. The terminal adopts
  the new width, and switching tasks restores their individual widths. The
  native smoke verifies that the canvas and PTY agree on the column count
  before resizing, after widening and after restoring the original width.
- [x] Implement shared terminals (one PTY across tasks) and task terminals
  (swap with the active task; keep running while hidden).
- [x] Show tracked Codex commands alongside interactive terminals in the
  Terminals tab as read-only terminals. Stream stdout and stderr live so users
  can follow each app-server command as it runs. The protocol stub covers
  ordered chunks and final output, and the browser fixture verifies the live
  pane alongside a shared shell across task switches.
- [x] Add **New task terminal**, **New shared terminal**, the instance menu,
  ended-terminal restart and close/reopen.
- [x] Save the selected inspector tab with the project layout. Shared terminals
  stay in view across task switches, and each project restores its own tab when
  selected again or reopened after a relaunch.
- [x] Route terminal focus and keyboard input. Show bell attention on the
  Terminals tab and on the owning task for task-scoped terminals.
  The native smoke rings a hidden task terminal. The browser regression
  verifies that returning to the window clears the badge for its focused PTY.
  Command+Shift+T on macOS and Ctrl+Shift+N on Linux open and focus a new
  terminal in the selected task's worktree.
  Command+Backquote on macOS and Ctrl+Backquote on Linux cycle through the
  Terminals tab and live splits. Shift reverses the cycle. Left and Right move
  between enabled inspector tabs. Down from the Terminals tab enters the
  live split headers, where the arrow keys follow the split geometry. The
  Quad browser regression skips ended panes and checks that Down in the
  terminal input still reaches the PTY.
  The split and hide shortcuts use the platform's documented modifier. The
  native smoke checks those actions and reads an unclaimed control chord
  directly from the PTY.
- [x] Persist terminal metadata and layout; restore ended terminals honestly.
  Restart returns keyboard focus to the terminal. The native smoke types
  directly into both restarted PTYs. Terminal list replies and events carry
  the core's revision. Browser regressions verify that an immediate exit
  stays ended when an older start or restart reply arrives after the exit.
  A missing terminal record restores as a removable placeholder alongside
  the other panes in the saved split.
  An unreadable layout falls back to the default for that project and leaves
  other projects available. The startup warning identifies the affected layout.
- [x] Measure parser and encoder throughput on a 1 MiB output burst.
- [x] Measure delivery and paint latency for a 1 MiB burst in the desktop
  webview before considering WebGL. On macOS, optimizing the debug builds of
  Shepherd and `alacritty_terminal` reduced parser and encoder time from
  126 ms to 43 ms. The native smoke painted the burst tail in 67 ms, down
  from 135 ms on the same machine, and painted all 24 paced markers.
  Inline storage for short cell text reduced the time to build 1,000
  80 × 24 snapshots from 140 ms to 28–30 ms on macOS. The parser and
  encoder benchmark produced 45,632 bytes across four frames in both builds.
  The native 1 MiB burst painted in 36 ms with the inline storage, compared
  with 37–40 ms across three baseline runs. All 24 paced markers appeared.
- [x] Bound PTY queues and allow one frame in flight per webview subscription.
  Coalesce paints and defer canvas work while the inspector tab or window is
  hidden. The browser regression verifies that the terminal skips a queued
  paint when the window hides and draws the latest frame on return. The native
  `--macos-visibility` smoke minimizes the window, observes new PTY output
  with zero hidden paints, then restores it and verifies the canvas paints
  that output. Rate-limit frame delivery from the previous send so an idle
  pane can update immediately.
  If a suspended webview wakes after its frame subscription expires, it
  reconnects to the same PTY. The headless regression checks that a fresh
  snapshot and subsequent output reach the pane without restarting the shell.
  Each terminal view limits queued input to 128 operations and 1 MiB of
  payload. Kitty key presses and reported mouse buttons reserve room for their
  releases. The browser fixture stalls a write, checks the limit, and verifies
  input resumes in order. It checks that an accepted mouse press gets its
  release even when the queue fills and a rejected press leaves no capture.
  Leaving the terminal during a drag releases the button at its last position.
  The browser fixture verifies that the later pointer-up adds no second release.
  Horizontal wheel motion is reported to mouse-aware terminal programs. The
  browser fixture covers fractional motion, both axes and Shift bypass.
  The Ubuntu Xvfb smoke verifies left, right and down reports in the live PTY.
  If a later chunk of a bracketed paste fails, the input queue attempts to
  end the paste before sending subsequent keys and reports the write error.

The default macOS native smoke runs with a hidden, unfocused window. It checks
PTY I/O, switching, shutdown and restore. Use `--macos-window` for the full
window and canvas checks, `--macos-visibility` for minimize and restore, and
`--macos-ime` for the system candidate picker when a change affects them.

### Proof

Automated:

- [x] Parser fixtures for colours, alternate screen, resize, wide characters,
  reflow, large output and Kitty mode negotiation.
- [x] Terminal layout round-trip through Turso and task terminal restart through
  the PTY bridge.
- [x] Playwright with mocked IPC covers the shared terminal's single
  subscription across task switches, task pane visibility, split resize,
  hide and reopen, key encoding and keyboard copy mode.
- [x] Native macOS smoke saves a Vim edit to disk, pages through less and
  selects a filtered result in full-screen fzf with terminal keyboard input.
  It also covers alternate screen restoration, synthetic
  composition from scrollback, Kitty and fish key reports, bracketed paste,
  SGR mouse input, OSC 8 hyperlinks, burst and paced paint, scrollback search, PTY split
  resize, task and project switching, reattachment, quit and restart of shared
  and task terminals. The quit warning counts the live task and all three
  running terminals. The smoke verifies that Stop process and Quit both end
  foreground and background jobs that ignore SIGHUP, and that the stopped task
  terminal can restart. Both actions give terminal groups a 200 ms grace
  period, then stop groups with members that still belong to their original
  PTY sessions.
  A PTY regression also lets the shell exit normally with a background job
  still running. The shell reports its exit code, and Quit stops the job
  after verifying its process identity and session membership.
  On macOS, Command+Q and the Quit menu item use the guarded window-close
  path. With a waiting scripted task and one live terminal, Command+Q showed
  both counts. Keep working preserved the task and shell. Confirming quit
  restored the task as interrupted and the terminal as ended on relaunch.
  Command+Q closed directly when only an idle terminal was running.
  Switching projects preserves the original PTY's shell
  state and output received while hidden. The hyperlink check waits for the
  test-only desktop opener to record the exact URL before sending the next
  terminal command. It reports any link error at that step.
  The find bar navigates literal matches in both directions and returns
  keyboard focus to the shell when dismissed. The browser smoke checks that
  Escape during IME composition leaves the find bar open.
  The native smoke also copies a wide glyph from either cell and copies the
  punctuation next to it independently.

Manual script:

1. Open two terminals in a split; resize both.
2. Run `vim`, `less` and full-screen `fzf`; check alternate screen and keys.
3. Run a long-output command; confirm scrolling and no frame drops.
4. Create a shared terminal and a task terminal.
5. Switch tasks; shared stays, task-specific swaps.
6. Close a terminal view and reopen it from the instance menu.
7. Quit and relaunch; both appear ended with restart actions.
8. With a system input method enabled, compose text in a terminal, move the
   candidate selection and commit it. Verify that the shell receives the
   committed text once and the candidate UI follows the terminal cursor.

On 2026-11-06, the disposable macOS app split a shared shell with
Command+Shift+D. After dragging the divider, `stty size` reported 14 columns
instead of 20 in the first pane and 18 instead of 12 in the second. Hiding and
reopening a pane kept its shell live. Vim saved an edit to disk, Space paged
forward in less, and fzf returned `gamma` after filtering with `g`. The
terminal scrolled from the end of `seq 1 160` back to rows 28–42.

At 35 columns, pasting the fzf command reproduced a garbled `alpha` in both
the canvas and the accessibility mirror before Return. The recorded zsh
output first printed the command correctly, then used `CSI 20 C` during a
highlight redraw that needed to move down to the next row. Replaying the
same bytes in a separate tmux PTY reproduced the overwrite. With a minimal
zsh profile, an equivalent highlight redraw used cursor-down and cursor-left
sequences and preserved the command. This isolates the observed overwrite
to the redraw emitted with the original shell profile.

Command+Shift+T opened a task shell in the scripted task's Git worktree.
Both shared shells stayed visible when switching chats. The task shell hid
until its owning chat was selected again, and both shell scopes retained
their session variables. After relaunch, all three shells appeared ended
with Restart actions. Restarting the task shell opened its original worktree
and focused terminal input.

With the existing British layout, the packaged macOS test app recorded one
trusted composition event and one matching PTY input at each of two cursor
positions. Option+E followed by E produced `é`, and Option+U followed by U
produced `ü`. At a third position, Option+3 produced one trusted text input
event and one literal `#` in the shell. Browser regressions cover macOS legacy
and Kitty encoding, including the Option+Left terminal chord, and Linux
Alt encoding. Run the native check with
`./scripts/smoke-shepherd-terminal.sh --macos-ime british` from the Nix shell.
The British dead keys exercise character composition. Candidate selection
and candidate-window placement still need a separate macOS check.
With the same layout, holding `e` for 1.5 seconds in the disposable Shepherd
terminal sent a plain `e` to the shell without showing a candidate panel.

Headless Chromium drives trusted preedit updates through the DevTools input
API. The regression checks that the terminal writes only the committed text,
once, and discards a cancelled composition before the next ordinary key.

Run the candidate check from the Nix shell with
`./scripts/smoke-shepherd-terminal.sh --macos-ime`. Use the system
**Pinyin - Simplified** input source. At each of the two prompts, type
`ni` and check that the candidate panel opens next to the current cursor.
Commit the first candidate at the first prompt and a different candidate
at the second. Press Return after each commit to submit it to the shell.
The smoke checks that a trusted composition event and the PTY's received
text match on both attempts. The operator checks placement visually.

On Ubuntu 24.04.5 ARM64, a disposable Xvfb session used IBus Pinyin through
GTK. After the PTY cursor moved, the candidate panel initially opened by the
previous caret. Refreshing the focused, empty input after moving it fixed the
anchor. At two cursor positions, the panel opened within one row of the current
caret. Space committed `你`. Down then Space chose `尼` on the second attempt.
The shell received each selection once, matching the webview's trusted
composition events.
Run the repeatable check with `./scripts/smoke-shepherd-terminal.sh --linux-ime`
inside the Nix development shell on Ubuntu. The
[terminal smoke workflow](../../../.github/workflows/shepherd-terminal-smoke.yml)
sets the Mesa and GTK module paths and installs the required packages. This
mode starts its own Xvfb display, session bus, and IBus daemon with disposable
settings. The probe waits for the focused IBus client to report Pinyin before
typing. Three fresh sessions passed both candidate selections. Removing the
caret refresh reproduces the misplaced popup and fails the check.

The native smoke waits for the composition prompt's cursor row to paint before
scrolling back. It starts synthetic composition from scrollback and checks
that the input anchor returns to the live cursor before committing text once.
In a macOS run where the webview became hidden, the composition prompt was
present in the text mirror and a fresh subscription reported the expected
cursor, but the canvas still showed an earlier frame. The smoke now waits for
a visible window before starting and reports when a wait ends in a hidden
webview.

The **Shepherd terminal smoke** workflow uses an Ubuntu 24.04 runner and Xvfb
for the same two native launches. A push to `codex/shepherd-terminal-smoke`
starts the run. At `4e75638`, both launches and the native IBus Pinyin check
passed on the hosted x86-64 runner. The popup followed the caret at both
positions, and each selection reached the shell once. The 1 MiB burst tail
painted in 105 ms, and all 24 paced markers appeared. The optional fish
probe was skipped. [Hosted smoke run](https://github.com/lofi-tools/todo-lofi/actions/runs/42213055757)
The hosted rerun at `505ffe6` passed both desktop launches and the IBus
check. The 1 MiB burst tail painted in 79 ms, under the 100 ms budget, and
all 24 paced markers appeared. The optional fish probe was skipped.
[Hosted rerun](https://github.com/lofi-tools/todo-lofi/actions/runs/42215694509)
The locked Nix shell evaluates for Ubuntu and macOS without a local checkout
override. On a minimal Ubuntu 24.04 guest, the debug build succeeded. Start
the session bus with Ubuntu's D-Bus binaries so the daemon reads the distro's
session configuration before entering the Nix shell. Use the locked Nix Mesa
EGL provider under Xvfb. Both native launches passed on Ubuntu 24.04.5 ARM64.
The search fixture starts its first marker on a new line because `/bin/sh`
can leave its prompt immediately before the marker. The guest painted the
tail of the 1 MiB burst in 119 ms and all 24 paced markers. The optional
interactive fish probe was skipped on this guest.
With the tuned debug build at `f6addee`, both launches passed again in a
fresh Ubuntu ARM64 checkout. The burst tail painted in 57 ms and all 24 paced
markers appeared. The native PTY probe also verified Kitty releases after a
shortcut modifier was lifted and after the terminal lost focus.

At `7fe0b13`, Ubuntu 24.04 ARM64 passed all 33 regular Rust tests, both
desktop smoke launches and the IBus Pinyin probe. Stop and Quit each ended
the foreground and background fixture jobs that ignored SIGHUP. The search
smoke preserved its match through a 100-line burst. The optional fish probe
was skipped on this guest. The shutdown fixture now waits for both process
groups before emitting one readiness marker so it fits in a small viewport.

Both terminal launches and the IBus Pinyin probe passed on an Ubuntu 24.04.5
amd64 guest under OrbStack's QEMU user-mode emulation. QEMU rejected Nix's
private user and mount namespace. Binding the guest's cached store at `/nix`
temporarily and disabling Nix's nested build sandbox for this invocation let
the locked shell build Shepherd as an x86-64 executable. The guest's Nix
`stty` rejected a PTY that Ubuntu's `stty` could read, so the smoke now reads
the dimensions with Python's `os.get_terminal_size`. Both Pinyin candidates
appeared by the caret and reached the shell once each. The 554 ms burst
result measures the emulated guest.

At `5075c5c`, the Ubuntu 24.04.5 x86-64 guest passed all 33 regular Rust
tests, both desktop smoke phases and the IBus candidate probe. The tested
binary was ELF64 x86-64. The desktop run verified foreground and background
job shutdown, search during new output and the terminal hyperlink opener.
It painted the burst tail in 438 ms under QEMU and showed all 24 paced
markers. The optional fish probe was skipped.

At `4e75638`, a fresh Ubuntu 24.04.5 x86-64 checkout passed all 35 regular
Rust tests, both desktop launches and the native IBus probe under QEMU.
The Rust regression confirmed that Quit stops a background job after its
shell exits. The desktop run exercised independent Kitty mode replies
through a synchronized screen switch and restarted both terminal scopes
using the versioned terminal list. IBus committed `你` and `尼` once each,
with the candidate popup by the caret at both positions. The 1 MiB burst
tail painted in 307 ms and all 24 paced markers appeared. The optional fish
probe was skipped.

### Gate

**Pass:** a developer can use Shepherd’s terminals for a normal workday without
falling back to a separate terminal app.

Start the trial from the repository with `direnv exec . shep`. It preserves
projects and terminal layout across relaunches in `.cache/test-db/shepherd.db`.
Use the shell and projects you normally work in. At the end of the day, record
the commit, OS, shell, and whether you switched to another terminal. For any
fallback, record the command or shortcut and what Shepherd did. Include the
pane width if terminal output wrapped or became garbled.

**If it fails:** fix the terminal engine, IME or frame path before S5. Adapter
breadth will not compensate for an unusable terminal.

---

## S5 — Adapter breadth and honest tiers

**Goal:** the adapter model works beyond Codex; every tier either delivers
structured state or says plainly what it cannot do.

### Order

Follow the spec’s native-first order:

1. **OpenCode** — server mode when available, ACP mode otherwise.
2. **Pi RPC** — strict LF JSONL, extension UI, approval bridge and
   **unreviewed tools** fallback.
3. **Claude Code / ACP** — reuse `libs/acp-client`; permissions, fs/terminal
   capabilities and session load.
4. **Freebuff terminal** runs the CLI directly in the conversation region.

If Pi’s RPC probe is blocked, proceed with ACP while resolving it.

### Build

- [x] OpenCode probe, session control and transcript mapping.
  Shepherd reads `app["agents.opencode.executable"]` and the optional
  `app["agents.opencode.config_file"]` from `settings.json` and persists the
  agent on each task. The opt-in OpenCode 2.0.14 smokes verify
  session creation and resume after restart, queued prompt cancellation, and
  a complete streamed turn through a loopback model fixture. Contract fixtures
  cover text, reasoning, tool rows and permission replies.
- [ ] Pi byte-oriented LF reader, ID correlation, `agent_settled`, extension UI
  bridge and approval bridge.
  The stdio transport and ID-based command and dialog replies are implemented.
  The installed Nix-profile Pi 0.75.4 passed the isolated dialog smoke, but its
  prompt reply omitted `disposition` and its shipped RPC/session code lacks
  `agent_settled`. Continue with ACP while the Pi lifecycle probe is blocked.
  The Pi reference checkout documents the required contract in
  `packages/coding-agent/docs/rpc.md` at `a7a75a9dc6891281eed58906c07c55929e7b6c13`.
- [ ] ACP adapter using `libs/acp-client`; no duplicate protocol client.
  Use this adapter for OpenCode when its native protocol is unavailable.
  The shared client accepts an exact configured command, validates ACP v1,
  and reports child exit independently of wire shutdown. The installed
  OpenCode 2.0.14 smoke verifies initialization, shutdown and session load.
  Shepherd maps ACP history, text, thoughts, tools, plan, usage, approvals
  and turn outcomes. The fixture covers session replay, one-time approval,
  cancellation and worktree-confined atomic writes. OpenCode uses its configured
  executable for both tiers and records ACP when the native probe fails. The
  installed fallback smoke opens and reloads a session, and a subprocess
  fixture verifies a complete fallback turn. Claude Code uses the same adapter
  through its configured executable and arguments. ACP approvals can remember
  an exact request for the live session and record when they reuse a grant.
  Project-scoped grants persist per configured agent. Settings → Projects
  lists saved grants and live session grants by task and can reset either.
  ACP terminals use Shepherd's PTY engine and appear in the selected task's
  inspector. Fixtures cover input, resize, exit status, release, and cleanup
  on disconnect. A live Claude Code smoke remains open.
- [x] Freebuff runs in a standalone PTY in place of the conversation pane.
- [x] Tier downgrade UI, capability gates and diagnostics per adapter.
  The task shows its negotiated tier and fallback reason. Diagnostics list the
  command, protocol, capabilities and stderr. Cancellation, approval actions
  and agent panes follow the reported capabilities. Reopening distinguishes
  verified resume, legacy reconnect and a fresh session. The desktop launches
  the detected Codex exec fallback when app-server is unavailable.
- [x] Missing binary, auth failure, protocol failure and mid-turn exit all have
  visible outcomes for the Codex, OpenCode and ACP task adapters. The task keeps
  the agent's reason after reopening. Startup failures leave the task ready to
  retry. Failed turns offer recovery based on the stored session capabilities.

### Proof

- [x] Contract fixtures for the implemented Codex, OpenCode and ACP adapters
  and Pi RPC transport. The OpenCode fixture negotiates the native HTTP API
  and admits only the matching prompt before streaming its session events. The
  Pi subprocess fixture verifies the startup probe, dialog reply correlation,
  handled command disposition, and process exit with stderr kept off the wire.
- [ ] Pi lifecycle and approval bridge contract fixtures when those paths are
  implemented.
- [ ] One live smoke per installed OpenCode, Pi and Claude/ACP integration,
  gated in CI.
  The opt-in Shepherd adapter smoke workflow selects one preinstalled agent on
  a configured Nix runner. OpenCode has native and ACP coverage and the Pi
  smoke exercises its RPC dialog. The Claude ACP command can be tested through
  the same settings file as the desktop. A run on a configured CI runner and a
  live Claude Code run remain to be verified.
- [x] Approval, cancel, resume and failure paths where the implemented tier
  supports them. The installed native OpenCode smoke reloads a session, allows
  and denies a shell call, and withdraws a pending approval on cancellation.
  Its loopback provider also verifies sign-in failures. Codex app-server and
  ACP fixtures exercise approval, cancel, resume and failure. The Codex exec
  fixture verifies that a failed run reports its exit code and stderr.
- [x] A missing capability hides the corresponding session action. Headless
  tests verify approval and cancellation controls, the cancel shortcut,
  interactive agent terminals, and recovery for unsupported or unknown
  capabilities. The shell approval picker appears after a legacy session
  reconnects and reports support.
- [x] Mocked IPC verifies that the Freebuff terminal replaces the conversation
  pane. A local shell fixture verifies the standalone PTY path. Test suites
  must never launch Freebuff.

### Gate

**Pass:** each supported agent either works or degrades with a visible reason,
and the task header never overstates the tier.

---

## S6 — Workspace breadth and v1 feature completeness

**Goal:** complete the spec’s remaining v1 surface without reopening the core.

### Build

- [x] Files tab: tree, ignore rules, filter/search, viewer, changed-file badges,
  external editor.
  The Files tab browses a project root or task worktree with lazy folder
  expansion, a dotfile toggle, `.gitignore` and `.shepherdignore`. The
  read-only UTF-8 viewer shows line numbers and syntax highlighting within
  preview limits. Filename filtering searches the selected root as the user
  types. Submitted content searches run in the background and support cancel,
  and opening a result jumps to its line. Scans cap entries, content bytes and
  results, and label incomplete results. A debounced watcher refreshes Git
  badges for the selected root, including changes to the index and HEAD. A
  separate list shows deleted paths. The preview opens files in the default
  desktop text editor and reveals them in the file manager. Git and Review
  diffs can focus the Files tab and open the selected worktree file.
- [x] Preview tab: project URL, element annotation, structured prompt context,
  screenshot fallback when framing or injection is blocked.
  Settings → Projects saves a loopback URL per project. The Preview tab loads
  it in a sandboxed frame with explicit reload and browser actions. The
  injected element picker queues notes with the URL, selector, text excerpt
  and bounds, and **Add to prompt** appends them as editable context.
  When the element picker is unavailable, choose a screenshot taken in the
  browser and mark a point. The composer includes the task-scoped image path,
  pixel coordinates, dimensions, URL and note. Discarding a draft removes its
  screenshot copies.
- [x] Cross-task search over Turso FTS.
  Task transcripts now persist admitted prompts and batch settled entries in
  Turso. Opening a task restores its saved entry IDs and order. The native FTS
  index covers entry text, task titles and project names. Ranked queries return
  up to 50 hits with a highlighted snippet and stable entry ID. **Search tasks**
  in the header expands on focus and shows its shortcut when collapsed.
  Choosing a result focuses the matching transcript entry, including archived
  entries across projects. Replacing or dismissing a search cancels
  the pending request. A local macOS run measured 26.9 ms p95 for 50 Turso queries
  over 50,000 entries, excluding the typing debounce and desktop rendering.
- [x] Attachments and images, `@file` mentions, model/mode pickers.
  The composer queues removable local files and copies them into the task
  attachment cache on Send. Prompts retain the copied paths. Codex sends local
  images through app-server or through exec when its help advertises images.
  Native OpenCode sends structured file URIs when the API advertises them.
  ACP receives image bytes when negotiated and file links otherwise. ACP can
  read the selected linked text files through Shepherd's bridge during the
  sending turn. Turso saves composer attachment metadata with the prompt
  entry and restores the attachment order when reopening a task.
  `attachments.max_file_bytes` supports app defaults and project overrides
  for composer files and preview screenshots. The composer searches workspace
  paths from `@`, inserts a reference, and resolves it against the task
  worktree on Send. The Codex picker lists the app-server's models and
  per-model reasoning efforts and applies a choice on the next turn.
  Connected ACP and native OpenCode sessions expose their advertised mode
  choices beside Send and accept changes between turns. Native OpenCode
  drafts load the project mode catalog before creating the worktree and
  apply a selection before sending the first prompt.
- [x] Notifications, dock/status counts and review batch actions.
  Workspace settings opt in to system notifications for newly waiting tasks
  outside the focused task. The status bar shows running and review totals
  across projects, and macOS mirrors the review total in the Dock.
  The review count filters the Projects sidebar. Users can select rows across
  projects and confirm **Archive selected**. The core validates the selection
  and persists the archived states in one transaction. Headless fixtures
  cover the notification gate, Dock count requests and batch selection.
- [x] Command palette and default keymap.
  The core registry supplies command labels, categories, platform-specific
  chords and disabled reasons. The icon rail and keyboard open the palette
  and its project and task switchers. App chords dispatch by input scope.
  Terminal focus keeps unclaimed chords, and the palette can forward one
  claimed chord to the focused terminal. Existing terminal layout, Git
  staging, composer and diagnostics actions are available from the palette.
- [x] Keybinding editor and versioned keybindings file.
  Settings records a chord, validates overlapping scopes and applies a saved
  binding immediately. Reset restores the platform default. The file
  preserves other edits and unknown entries, rejects a stale edit to the
  same command, and backs up invalid content while defaults remain active.
- [x] General settings file and project overrides.
  Workspace and project preferences use versioned JSON with atomic writes,
  backups and per-setting conflict checks. Workspace defaults and project
  overrides govern archive visibility, attachment limits and terminal
  scrollback. Notifications and the Send accent are stored at app scope.
- [x] Built-in themes and density.
  Appearance saves System, Light or Dark, plus Compact or Comfortable
  density. Shell and terminal tokens follow the active theme and increased
  contrast. Terminal palette updates preserve explicit colors. Chromium
  checks color contrast and saved appearance.
- [x] Accessibility.
  Tab moves from Projects and the conversation into the selected inspector
  tab and its named panel. Git and Review expose file change status in words
  and let keyboard users focus the diff. Terminal copy mode keeps a selected
  snapshot and provides a way to leave the shell by keyboard.
- [x] Review and archive edge cases.
  A changed selection needs fresh confirmation and a failed archive keeps
  eligible rows selected. Turso tests reject stale batches, roll back partial
  writes and verify that retry preserves session exit information. Reopen
  only accepts archived tasks. The 30-day worktree collection is tracked in S7.
- [x] Status bar, window title and empty states.
  The footer shares the Files watcher for the current worktree's branch and
  changed file count. The native title follows the selected project and
  working count. Missing folders keep the transcript readable with input
  disabled. A deleted last task opens a draft, and empty projects keep their
  first-prompt headline.

### Proof

- [x] Playwright flows for files, preview, search, settings and the palette.
  The headless Chromium suite exercises the Files tree and live Git badges,
  Preview loading and annotation, header search, persisted settings, and
  palette commands. Run it with `direnv exec . pnpm --dir apps/shepherd/ui test:e2e`.
  The file mention fixture verifies keyboard selection, stale search
  cancellation, and project-root search before a draft has a worktree.
  The background desktop smoke selects a project file before the draft has a
  worktree, keeps focus in the composer, and sends the reference through the
  desktop bridge. Run it with `direnv exec . ./scripts/smoke-shepherd-terminal.sh`.
  Chromium verifies a Codex model and effort choice reaches the first turn.
  The Codex wire fixture checks the catalog and outgoing turn parameters.
  Chromium verifies the mode menu updates and disables choices during a
  turn. Wire fixtures cover ACP configuration selectors and legacy modes.
  The installed OpenCode smoke switches to Plan and reads it after reconnect.
  It also discovers Plan before creating an OpenCode session. A core fixture
  verifies mode selection precedes the prompt and a rejected mode leaves
  the transcript unchanged. Chromium covers choosing a mode before the first
  draft and keeping it through Send.
  The cross-task search fixture verifies cancellation, keyboard selection,
  and focus and highlighting on a cross-project archived result.
  After restarting with the same temporary database, the background desktop
  smoke finds the saved prompt through header search and verifies focus and
  highlighting in the restored transcript.
  Chromium checks fuzzy command search, disabled reasons, switcher navigation,
  a forwarded terminal chord, and scoped Send and Commit shortcuts.
  The shortcut editor fixture checks recording, conflict recovery, immediate
  dispatch, reload and reset.
  Settings fixtures verify project overrides, reset, pending edits after
  reload, and migration of the saved Send accent. The hidden macOS smoke
  verifies a project attachment limit after restarting Shepherd.
  The hidden macOS WebKit smoke opens the palette with the native registry
  while the terminal is mounted, then records a shortcut in Settings and
  checks that the binding survives restart.
- [x] Preview fixture: annotation adds URL/selector/note to the composer;
  blocked injection falls back to a screenshot note.
  Headless Chromium verifies both paths, including a page that denies framing
  and a failed screenshot save that preserves the queue for retry.
  Hidden macOS WebKit smokes verify element injection and saving a screenshot
  from the blocked-frame fallback through the desktop bridge.
- [x] Attachment cap, redirect and deletion tests.
  Rust fixtures cover the default file cap, task isolation, symlink rejection
  and draft cleanup, including isolation from another draft's cached files.
  Headless Chromium checks the composer queue and upload.
  The bridge applies the project cap to uploads, screenshots, and cached
  files. Headless Chromium verifies the chooser uses the selected project.
  A hidden macOS WebKit smoke verifies sending with a file alone and preserving
  the filename and bytes through the real bridge.
  Wire fixtures cover Codex image paths and ACP image capability negotiation.
  The configured OpenCode executable delivers the image to a loopback model
  fixture with matching bytes.
  ACP fixtures verify turn-scoped reads, write denial, and isolation from
  neighboring cache files and symlink redirects.
  Turso fixtures verify attachment metadata after reopen, rollback on an
  invalid path, and removal with the task record.
  Memory and Turso fixtures also verify task-scoped session, terminal, and
  layout records are removed while another task and shared terminal remain.
  Draft discard stops its task terminals and removes their panes while leaving
  shared terminals running; core and PTY fixtures cover this cleanup.
  Core saved-task deletion rejects active turns, closes an idle agent session,
  removes its durable records, and preserves the worktree and branch. The desktop
  bridge stages and deletes the attachment cache, restoring it if core deletion
  fails; fixtures verify saved-task cleanup and isolation. The desktop deletion
  dialog keeps the worktree by default and offers removal of a clean worktree;
  core fixtures verify dirty refusal and branch preservation, and Chromium
  verifies confirmation and retry behaviour.
- [x] Corrupt settings file falls back with a banner and backup.
  Rust verifies the exact backup bytes and read-only fallback. Chromium
  verifies the warning and that defaults are usable.
- [x] Search ranking and prefix-query tests against Turso FTS, not SQLite FTS5.
  The query plan uses the FTS index. Database fixtures cover exact-term ranking,
  Unicode prefix queries, legacy rows, edits, reopen and task deletion.
- [x] Accessibility: focus order, tablist labels, terminal text mirror and
  keyboard copy mode.
  Chromium verifies Tab order in both directions, panel and attention
  labels, terminal output in the accessibility tree, and clipboard copy while
  the mirror receives fresh output. Hidden macOS WebKit verifies header
  alignment and copy selection and focus restoration with a real PTY.

### Gate

**Pass:** the v1 feature checklist in the master spec is complete, with no
placeholder tabs and no unearned state badges.

---

## S7 — Hardening, recovery and release

**Goal:** v1 is trustworthy when things go wrong and fast enough to use all day.

### Build

- [ ] Crash and kill tests: interrupted state, orphan detection, recorded PID
  plus start time, reused-PID refusal.
  A headless subprocess fixture is killed with SIGKILL while a scripted agent
  waits for approval; reopening its database restores the prompt and marks the
  task interrupted without inventing the unfinished answer.
  PTY process-group shutdown checks each member's recorded start time before
  signalling; a fixture rejects a matching PID with a different start time.
  A headless SIGKILL fixture verifies that a surviving PTY process is recognised
  from its persisted identity after restart, then cleans up that test process.
  Project and task terminal launches now persist PID, start time, process group,
  session, and boot identity per launch. On exit, remaining background jobs
  are recorded before the leader record is removed; PTY fixtures verify
  persistence and cleanup. Startup snapshots earlier records for read-only
  inspection against boot, start time, process group, and session identities;
  a match does not establish that another Shepherd instance has released it.
  ACP terminal launches also record and clear their process identities. A
  startup notice shows matching and uncertain records and allows a recheck.
  Running direct ACP terminals now signal their process group on kill, release
  and shutdown, and report signal termination in the ACP exit status. Agent
  Codex app-server now runs in its own process group and stops that group on
  normal shutdown; a local fixture checks a descendant cannot outlive shutdown.
  Agent process identities and stopping surviving processes remain.
- [ ] 30-day archived-worktree collection, dirty skip, branch-tip recreation.
  Startup preserves dirty worktrees and lists collection failures with their
  reasons; clean collection retains the tip and prunes Git worktree metadata.
  Opening a task, using its terminal, and accessing its files or Git views
  refresh a separate last-use timestamp, coalescing repeated input and reads
  to at most one write per minute per activity source. Older databases
  initialize it from task update time. ACP agent terminals carry their task
  identity; their output and user input refresh that task's worktree usage.
  Collection records its intent before removal. If the final database write
  fails, startup verifies the retained Git reference before finishing the
  missing-path record. Fixtures cover this recovery, a missing retained ref,
  an unrelated missing checkout, and a worktree that became dirty while a
  collection was interrupted.
- [ ] Database backup, migration checks, malformed migration handling and
  transactional writes.
  Existing file databases receive a retained Turso snapshot in a private
  `<database>.pre-migration-<id>/snapshot.db` directory before pending
  migrations; a failed backup, unknown migration or gap in migration history
  blocks the migration before schema changes. A Unix fixture denies backup
  directory creation, verifies the old history remains intact, then retries
  successfully after access is restored.
  Turso's parser validates migration statements before changes, and a failed
  statement rolls back its migration and history entry. Broader transaction
  checks remain. Transcript batches reject missing or mismatched task/session
  owners; fixtures verify failed attachment updates roll back the whole batch.
  Normal task-state saves and interrupted shutdowns commit eligible transcript
  entries in the same transaction. Both transcript write paths explicitly
  roll back failures and permit immediate retry. A failed shutdown save retains
  its session and can be retried while draining. Multi-task archive rolls back on a
  mid-batch database error and can be retried. Task deletion clears the project's
  last-task reference and changed terminal layout in the same transaction and
  rolls back a mid-deletion failure, preserving those records, transcripts and
  attachments for retry. Draft creation commits the project selection and new
  task together, with a rollback and retry fixture for a failed task write.
- [ ] Bounded transcript, stderr, search and frame buffers.
  Codex, OpenCode and ACP stderr readers cap each line while draining the
  process, including newline-free output. OpenCode omits overlong lines to
  avoid exposing fragments of its authentication secret. ACP protocol lines
  are rejected above 8 MiB while reading, with a protocol error. Event queues
  and other buffer limits still need review. Codex app-server, exec and
  handshake protocol lines have the same limit; oversized exec output fails
  the turn and stops its process. Exec keeps draining output while it makes
  progress after process exit, with a 10-second limit and prompt cancellation.
  It fails the turn if the output pipe stalls or exceeds that limit instead of
  reporting incomplete output as successful. Codex version and help
  probes limit both stdout and stderr and time out if the command does not
  finish. ACP keeps
  only 64 unread stderr notifications while retaining its latest diagnostic
  tail. ACP terminal output honors smaller requested limits and caps larger
  requests at 1 MiB per terminal. ACP terminal output has a shared 64-event
  backpressure limit with 8 KiB chunks; release or shutdown unblocks a stalled
  pump. Output permits remain held through the harness queue until the core
  consumes the events. A direct terminal's exit wait completes even if a
  descendant keeps an output pipe open, and shutdown rejects new terminals.
  OpenCode decodes event-stream chunks one event at a time instead of
  queuing every decoded event from a network read. Event folding handles at
  most 256 events per session per pass, including during commands, and
  reschedules remaining events so one busy agent cannot monopolize other
  sessions. Shutdown drains the remaining events. Critical event
  queues still need backpressure.
  Codex exec and app-server bound completed command output to the transcript's
  existing 64 KiB tail before placing it on the event queue, avoiding larger
  queued copies without changing the retained output.
- [ ] Multiple agents and projects: three agents across two projects, no state
  bleed, no polling.
  A three-session fixture holds Codex, OpenCode and Claude tasks at approvals
  across two projects, resolves identical approval IDs independently, and
  verifies worktree and persisted transcript isolation. Agent events wake the
  shell directly instead of using a periodic poll; fixtures cover multiple
  receivers and delivery between commands. The event pump exits on shutdown
  or shell disposal without retaining the core. Live adapters remain unverified.
- [ ] Performance: cold start under 1.2 s, streaming while a large diff renders,
  terminal burst budget.
  Review and Git retain in-flight diff requests across transcript-only updates;
  a headless fixture streams through 1,000 transcript entries while a 12,000-line
  diff loads. Project snapshots no longer re-query an unchanged attachment limit,
  and a fixture checks that streaming preserves the Files path and preview.
  Git and Review reads run outside the core lock on blocking workers. A real
  worktree fixture verifies an agent approval can finish during a stalled read,
  then checks both ends of a 12,000-line diff.
  End-to-end timing with a real worktree remains unmeasured.
- [ ] Path confinement, environment `!command` timeout, no secret values in
  logs or storage.
  File preview, directory listing and search open each path component without
  following symlinks on Unix. Fixtures cover traversal, static symlinks and a
  directory replaced by a symlink after opening. External editor paths and the
  remaining filesystem operations still need a confinement audit. ACP file reads
  and atomic writes also use descriptor-relative operations on Unix, with
  fixtures for directory substitution and preserved file permissions. ACP
  attachment grants open every path component without following symlinks and
  retain the opened file for the duration of the prompt. Agent launch environment
  commands resolve at spawn with a five-second timeout and bounded output; errors
  exclude their output. Oversized output and failed commands with inherited
  stdout fail promptly; any process-group cleanup failure is reported.
  Cancellation also stops the command's process group on Unix; a local fixture
  checks that a descendant cannot write after the resolver is dropped. Settings
  supply per-agent maps with project overrides
  for task launches and model and mode discovery. A scripted task fixture
  verifies that a local process receives the resolved value while the workspace
  snapshot and reopened database files do not contain it. Agent, ACP and terminal
  launch-spec debug output omits environment values. Core sessions with launch environment values
  hide adapter stderr and failure details while preserving failure categories;
  a fixture covers exit events and errors. Discovery failures also hide details
  when environment values are present, with local Codex and OpenCode fixtures.
  ACP connection, OpenCode cleanup and Codex exec warnings omit agent-controlled
  error text; a log-capture fixture checks the Codex exec warning.
  Other adapter-internal logs and live-adapter behavior remain to be audited.
  The headless agent-harness suite passed 87 tests (two live tests ignored) and
  the core suite passed 100 tests (three live tests ignored) on macOS after
  these confinement and environment changes.
- [ ] Packaging and CI: macOS 13+, Ubuntu 24.04+, pinned Zig, Tauri build,
  platform smoke.
  The local macOS debug app bundle declares macOS 13.0 in both Info.plist and
  its executable build metadata. The Nix shell defaults to macOS 14.0; the app's
  build script sets 13.0 before compiling. Rebuilding the two cached dependencies
  that triggered deployment-target warnings produced a bundle without those
  warnings. A full clean build and the release checklist on macOS 13 are still
  needed before claiming compatibility. The terminal
  uses the recorded Alacritty fallback rather than the planned Zig engine.
  A manually dispatched `shepherd-package-smoke` workflow builds a macOS app
  and Ubuntu Debian package on clean runners and checks their metadata. It
  rejects a packaged executable whose interpreter or dynamic paths point into
  the Nix store; it has not yet been run, and package portability remains
  unverified.
- [ ] v2/GPUI explicitly not started until v1 is feature complete.

### Proof

- [ ] Chaos: kill -9 mid-turn, restart, resume or fail honestly.
  A local scripted approval-wait fixture covers SIGKILL and interrupted recovery;
  live-agent crash and resume behavior still needs verification.
- [x] Missing worktree repair; archived worktree collection; dirty skip.
  A task fixture deletes its checkout between turns and verifies restoration of
  the same branch and transcript across restart. The review lifecycle fixture
  collects an old clean worktree and restores its recorded tip; the startup
  fixture retains dirty and recently used worktrees.
- [ ] Large transcript and large diff with no dropped state.
  The headless UI fixture checks all streamed chunks and both ends of a large
  mocked diff while 1,000 earlier transcript entries remain present.
- [ ] Three-agent/two-project concurrency test.
  The local scripted fixture covers overlapping approval waits and independent
  worktree writes; a live multi-agent run remains unverified.
- [ ] Release checklist on both target platforms.

### Gate

**Pass:** the release checklist passes on a clean machine, not only the
developer machine.

---

## Parallel work rules

- S0 must finish before any UI or adapter work.
- The terminal engine spike and the Codex fixture capture can run in parallel
  after S0; only one user-visible slice is integrated at a time.
- S2 and S3 may overlap only in test fixtures. The S2 gate must pass before S3
  is judged on real Codex behaviour.
- Do not parallelize two gates. A failed gate needs a decision, not more
  branches.
- Keep one disposable repository per manual script; never test destructive Git
  operations on a real checkout.

## What not to build early

- No GPUI or v2 shell.
- No board, separate review inbox, remote projects, SSH, MCP or updater.
- No full adapter breadth before Codex and the review loop work.
- No terminal WebGL, custom chrome, animation or theme polish.
- No 30-day GC, plugin API, CLI or multi-window.
- No preview/search/attachments before the task review path is proven.

## Spec milestone mapping

The spec milestones are acceptance buckets, not the build order.

| Spec milestone | Covered by |
| --- | --- |
| M0 — shell, registry, schema, status, quit | S1 for the thin shell; S6 for palette/settings completeness |
| M1 — fixed workspace, project terminal layout, worktree | S1 for one terminal; S3 for worktree; S4 for full layout |
| M2 — Codex adapter | S2 |
| M3 — OpenCode, ACP, Freebuff | S5 |
| M4 — Files, Git and preview | S3 for Git/review; S6 for Files/preview |
| M5 — Pi, attachments, search, notifications | S5 for Pi; S6 for the rest |
| M6 — GPUI | Unscheduled; only after v1 parity |

## Gate log

Record every gate result here. Do not silently mark a gate passed.

| Date | Slice | Result | Evidence / decision |
| --- | --- | --- | --- |
| 2026-11-04 | S0 | Pass | Headless fake-task lifecycle in temporary repositories and databases: draft creates no worktree, first send creates one, approval pauses at `waiting` and resumes, cancel keeps worktree and session, quit marks the task `interrupted`, reload offers resume with a new session (never claiming the old process survived), a chat-only turn returns to `ready`, and a changed turn enters `review`. `cargo test -p shepherd-core` (18), `cargo test -p agent-harness` (9), `cargo test -p acp-client --features test-support` and `./scripts/check-shepherd-boundary.sh` pass. |
| 2026-11-05 | S1 | Pass | The two-launch macOS smoke added the repository through the picker, created a draft with no branch or worktree, displayed scripted text and a tool row, ran `pwd` and `printf`, and sent Ctrl+C to a foreground command. Task switching preserved the draft, transcript, and shared terminal. Closing the window warned about one waiting task. After confirming and relaunching, the task was interrupted, terminal records were ended, and **Resume** reattached the scripted session. The 18-test browser suite also passed. |
| 2026-11-05 | S2 | Pass | The native macOS window ran the installed Codex CLI at GPT-6.1 Terra low effort on disposable repositories. It streamed a one-file edit into Review, allowed an approval, interrupted a running command, accepted a follow-up, rendered the failure row and stderr after a mid-turn SIGKILL, and resumed the same thread and worktree after relaunch. All four native scenarios replayed their captures in fresh disposable data. The headless adapter and core suites also cover protocol probing, tool mapping, fallback, failure, and session recovery. |
| 2026-11-05 | S3 | Pass | The live desktop reviewed two files, applied a review note in a second turn, staged and committed, and confirmed the squash merge in the Git pane. The cached replay covered the same steps, archived the task, and reopened it in a second native launch after the fixture removed the clean worktree. Git verified the base contents, single parent, retained task branch, and the recreated worktree at the original task commit. |
| 2026-11-05 | S4 | Partial | The two-launch macOS smoke drove Vim, less and fzf, distinguished Ctrl+I from Tab in interactive fish, and checked composition from scrollback, bracketed paste, SGR mouse reports, and OSC 8 hyperlink dispatch. It verified PTY size changes from both the inspector and split dividers, preserved a shared terminal across task and project switches, and restarted both shared and task terminals after relaunch. After removing the fixed delay on idle frame delivery, a debug run painted the tail of a 1 MiB burst in 102 ms and all 24 paced markers, with a 74 ms maximum gap between first paints. Run it with `direnv exec . ./scripts/smoke-shepherd-terminal.sh --macos-window`. Native dead-key input passed in the packaged app. IME candidate selection and placement, Ubuntu, and the manual workday gate remain open. |
| 2026-11-06 | S4 | Partial | Both scripted native launches passed on Ubuntu 24.04.5 ARM64 with Xvfb. The hosted x86-64 smoke, system IME candidate selection and placement, and the workday trial remain open. |
| 2026-11-06 | S4 | Partial | Native IBus Pinyin passed after correcting the WebKitGTK caret position. The hosted x86-64 smoke, candidate-based input on macOS, and the workday trial remain open. |
| 2026-11-06 | S4 | Partial | Both native desktop launches and the IBus candidate check passed on hosted Ubuntu x86-64 at `4e75638`. Candidate-based input on macOS and the workday trial remain open. [Hosted smoke run](https://github.com/lofi-tools/todo-lofi/actions/runs/42213055757) |
| 2026-11-06 | S4 | Partial | The hosted Ubuntu x86-64 rerun at `505ffe6` passed both launches and IBus. The 1 MiB burst painted in 79 ms. [Hosted rerun](https://github.com/lofi-tools/todo-lofi/actions/runs/42215694509) |
| 2026-11-06 | S4 | Partial | The exact tree at `a1c7964` passed both native launches and IBus Pinyin on Ubuntu 24.04 ARM64 under Xvfb in a disposable guest. The 1 MiB burst painted in 51 ms and all 24 paced markers appeared. The candidate panel followed the caret at both positions, and `你` and `尼` each reached the shell once. Fish was skipped. |
| 2026-11-06 | S4 | Partial | Both launches of the default macOS smoke and the cached Codex approval replay passed with the window hidden and unfocused. The full Linux suite still passed both launches under Xvfb. The macOS window and candidate checks are opt-in. |
|  | S5 |  |  |
|  | S6 |  |  |
|  | S7 |  |  |

## Known unknowns to resolve at the gate

| Unknown | Resolve by |
| --- | --- |
| Does `libghostty-vt` build with the pinned Zig on macOS and Ubuntu? | S1 spike; fallback to `alacritty_terminal` behind the trait if not. |
| Does Codex `app-server` expose the methods Shepherd needs on the installed version? | Resolved on macOS with 0.166.0-alpha.6. The native S2 smokes verified thread start, turn start, streamed items, approvals, interrupt, mid-turn exit, and thread resume. |
| Does Tauri’s system webview support the preview iframe and annotation injection? | S6 preview fixture; screenshot fallback if not. |
| Does Turso FTS behave closely enough to the search spec? | S6 query/ranking tests; keep search syntax behind `search.rs`. |
| Does squash-merge behave correctly with the project’s hooks and signing? | S3 temp-repo tests; never test on a real checkout. |
| Is terminal frame throughput sufficient without WebGL? | S4 burst measurement; WebGL only if measured. |

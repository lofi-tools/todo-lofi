# Shepherd implementation plan

Status: proposed, 2026-11-04.

Specs: [goal.md](./spec/goal.md), [master-spec.md](./spec/master-spec.md),
[stack.md](./spec/stack.md). This file is the working order; the specs remain the
source of truth for behaviour and technology.

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
| S5 | OpenCode, Pi, Claude/ACP and Freebuff each work or degrade honestly | Adapter contract tests + live smoke per tier | No adapter fakes structured state. |
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
- [x] Add a project directory picker, modeled on taskstream's
  (`apps/taskstream-desktop/src/projects.rs` for the scan,
  `src/ui_parts/project_picker.rs` for the dialog). Replace the sidebar's path
  text input with an **Add project** button that opens a picker. Scan `~` for
  Git repositories as the app starts, on a background task: bounded depth,
  skipping junk (`node_modules`, `target`, `.cargo`, `Library`, `.Trash`, …),
  and stopping at a repository root so repos nested inside a checkout are not
  listed twice. Cluster the results into a tree rather than a flat list:
  collapse chains of single-child directories, and keep each level to roughly
  eight branches by grouping the remainder, so deep `~/src/me/...` trees stay
  readable. Add a filter field, arrow-key navigation, Enter to select and Esc
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
   Covered by the ignored live test: the real process is SIGKILLed mid-turn and
   the task ends `failed` with a failure row and the stderr tail.
6. Restart Shepherd and resume the session.
   Covered at the core level by the ignored live restart test: quitting mid-turn,
   reloading the same database, resuming the recorded thread and finishing
   another turn.

### Gate

**Pass:** a real Codex task works end to end on a disposable repository.

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

Automated, all in temporary repositories:

- [ ] edit-then-revert stays `ready`;
- [ ] a commit, branch change or content change in an already-dirty file enters
  `review`;
- [ ] worktree creation appears in `git worktree list` and uses the default
  branch even when the root is on another branch;
- [ ] stage, unstage, commit with hooks and signing disabled;
- [ ] failing hook preserves the commit message;
- [ ] squash-merge from a clean root;
- [ ] dirty root and off-base root refuse with a reason;
- [ ] conflict paths are listed;
- [ ] missing and dirty worktrees are not silently removed.

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
  hyperlinks.
- [ ] Add the binary frame protocol, dirty-row rendering and a frame smoke test.
- [ ] Build the Terminals tab split tree, resize, focus and presets.
- [ ] Add the project terminal layout: slots, sizes, order and scope; save it
  per project.
- [ ] Implement shared terminals (one PTY across tasks) and task terminals
  (swap with the active task; keep running while hidden).
- [ ] Add **New task terminal**, **New shared terminal**, the instance menu,
  ended-terminal restart and close/reopen.
- [ ] Make the inspector tab project-level so shared terminals stay in view
  across task switches.
- [ ] Add attention dots, terminal focus routing and keyboard passthrough.
- [ ] Persist terminal metadata and layout; restore ended terminals honestly.
- [ ] Measure frame throughput before considering WebGL; keep the 1 MB burst
  budget and dirty-row coalescing.

### Proof

Automated:

- [ ] PTY fixtures: colours, alternate screen, resize, wide chars, large output;
- [ ] parser state and input encoding, not pixels;
- [ ] terminal layout round-trip;
- [ ] shared terminal remains the same PTY across task switches;
- [ ] task terminal disappears with its task and returns on reselect;
- [ ] ended terminal restarts in the right scope;
- [ ] Playwright: tabs, split resize, task switch and instance menu.

Manual script:

1. Open two terminals in a split; resize both.
2. Run `vim`, `less` and a full-screen TUI; check alternate screen and keys.
3. Run a long-output command; confirm scrolling and no frame drops.
4. Create a shared terminal and a task terminal.
5. Switch tasks; shared stays, task-specific swaps.
6. Close a terminal view and reopen it from the instance menu.
7. Quit and relaunch; both appear ended with restart actions.

### Gate

**Pass:** a developer can use Shepherd’s terminals for a normal workday without
falling back to a separate terminal app.

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
4. **Freebuff terminal wrapper** — bundled manifests, coarse state, plain
   terminal fallback.

If Pi’s RPC probe is blocked, ACP and Freebuff may proceed in parallel while
that is resolved; do not fake Pi’s structured state.

### Build

- [ ] OpenCode probe, session control and transcript mapping.
- [ ] Pi byte-oriented LF reader, ID correlation, `agent_settled`, extension UI
  bridge and approval bridge.
- [ ] ACP adapter using `libs/acp-client`; no duplicate protocol client.
- [ ] Freebuff PTY wrapper and manifest fixtures.
- [ ] Tier downgrade UI, capability gates and diagnostics per adapter.
- [ ] Missing binary, auth failure, protocol failure and mid-turn exit all have
  visible outcomes.

### Proof

- [ ] Contract fixtures per adapter.
- [ ] One live smoke per installed CLI, gated in CI.
- [ ] Approval, cancel, resume and failure paths where the tier supports them.
- [ ] A missing capability hides the control instead of showing a dead button.
- [ ] Freebuff with no matching manifest shows a plain terminal with a notice.

### Gate

**Pass:** each supported agent either works or degrades with a visible reason,
and the task header never overstates the tier.

---

## S6 — Workspace breadth and v1 feature completeness

**Goal:** complete the spec’s remaining v1 surface without reopening the core.

### Build

- [ ] Files tab: tree, ignore rules, filter/search, viewer, changed-file badges,
  external editor.
- [ ] Preview tab: project URL, element annotation, structured prompt context,
  screenshot fallback when framing or injection is blocked.
- [ ] Cross-task search over Turso FTS.
- [ ] Attachments and images, `@file` mentions, model/mode pickers.
- [ ] Notifications, dock/status counts and review batch actions.
- [ ] Command palette, keybinding editor, settings files, project overrides,
  themes and accessibility.
- [ ] Review/archive edge cases; leave the 30-day worktree collection to S7.
- [ ] Status bar, window title and empty states.

### Proof

- [ ] Playwright flows for files, preview, search, settings and the palette.
- [ ] Preview fixture: annotation adds URL/selector/note to the composer;
  blocked injection falls back to a screenshot note.
- [ ] Attachment cap, redirect and deletion tests.
- [ ] Corrupt settings file falls back with a banner and backup.
- [ ] Search ranking and prefix-query tests against Turso FTS, not SQLite FTS5.
- [ ] Accessibility: focus order, tablist labels, terminal text mirror and
  keyboard copy mode.

### Gate

**Pass:** the v1 feature checklist in the master spec is complete, with no
placeholder tabs and no unearned state badges.

---

## S7 — Hardening, recovery and release

**Goal:** v1 is trustworthy when things go wrong and fast enough to use all day.

### Build

- [ ] Crash and kill tests: interrupted state, orphan detection, recorded PID
  plus start time, reused-PID refusal.
- [ ] 30-day archived-worktree collection, dirty skip, branch-tip recreation.
- [ ] Database backup, migration checks, malformed migration handling and
  transactional writes.
- [ ] Bounded transcript, stderr, search and frame buffers.
- [ ] Multiple agents and projects: three agents across two projects, no state
  bleed, no polling.
- [ ] Performance: cold start under 1.2 s, streaming while a large diff renders,
  terminal burst budget.
- [ ] Path confinement, environment `!command` timeout, no secret values in
  logs or storage.
- [ ] Packaging and CI: macOS 13+, Ubuntu 24.04+, pinned Zig, Tauri build,
  platform smoke.
- [ ] v2/GPUI explicitly not started until v1 is feature complete.

### Proof

- [ ] Chaos: kill -9 mid-turn, restart, resume or fail honestly.
- [ ] Missing worktree repair; archived worktree collection; dirty skip.
- [ ] Large transcript and large diff with no dropped state.
- [ ] Three-agent/two-project concurrency test.
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
|  | S1 |  |  |
| 2026-11-05 | S2 | Partial | A live headless run on a disposable repository (`codex-cli 0.166.0-alpha.6`, ignored test) streamed 32 deltas, reported two tool calls, changed the file, reopened the same thread after restarting the process, and cancelled a running turn. It found and fixed one protocol gap: `thread/start` takes `workspace-write`, not `workspaceWrite`. A mid-turn SIGKILL of the real process is proven through the core (ignored test: task `failed`, failure row, stderr tail). A live approval request is also proven at the adapter: Codex asked before a network command, the request carried the command and its decisions, the denial was recorded, and the turn completed. Restart-resume is proven through the core too: an ignored test quits mid-turn, reloads the same database, sees the task interrupted, resumes the recorded thread and finishes another turn. Still unverified before a pass: the desktop window, including how the approval card renders there. The core now pumps adapter events between commands (covered by a focused test), so a live turn can stream into the window. The desktop shell was also built and launched from the Nix shell on 2026-11-05 (Vite on 1420 answered 200 and the Tauri binary ran), but the window was not inspected and no task was driven through it. Automated suites: 38 `agent-harness` (2 live, ignored), 37 `shepherd-core` plus 2 ignored live runs, 9 shell. |
|  | S3 |  |  |
|  | S4 |  |  |
|  | S5 |  |  |
|  | S6 |  |  |
|  | S7 |  |  |

## Known unknowns to resolve at the gate

| Unknown | Resolve by |
| --- | --- |
| Does `libghostty-vt` build with the pinned Zig on macOS and Ubuntu? | S1 spike; fallback to `alacritty_terminal` behind the trait if not. |
| Does Codex `app-server` expose the methods Shepherd needs on the installed version? | Resolved for 0.166.0-alpha.6: thread start, turn start, streaming items, interrupt and resume all answered live (see Gate log). The desktop window was not exercised live; an approval request, a mid-turn kill and a restart-and-resume through the core were. |
| Does Tauri’s system webview support the preview iframe and annotation injection? | S6 preview fixture; screenshot fallback if not. |
| Does Turso FTS behave closely enough to the search spec? | S6 query/ranking tests; keep search syntax behind `search.rs`. |
| Does squash-merge behave correctly with the project’s hooks and signing? | S3 temp-repo tests; never test on a real checkout. |
| Is terminal frame throughput sufficient without WebGL? | S4 burst measurement; WebGL only if measured. |

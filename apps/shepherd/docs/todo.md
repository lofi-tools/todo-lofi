# Shepherd implementation plan

Status: in progress. The initial plan was written on 2026-11-04; the slice
checklists and evidence below record subsequent work.

Specs: [goal.md](./spec/goal.md), [master-spec.md](./spec/master-spec.md),
[stack.md](./spec/stack.md), [workspace-ui.md](./spec/workspace-ui.md). This
file gives the working order. The specs govern behaviour and technology.
Check the workspace UI decisions when implementing a slice that touches the
shell.

## Current progress

The desktop shell, durable core, Codex task flow, worktree review, terminal
interface, preview relay, site, catalog, release matrix, downloads page and
agent pool are implemented and have scripted coverage. Their individual proof
and gate sections describe what is and is not verified.

The S4 normal-workday trial and macOS candidate-input check remain open. The
user asked to skip those blocked checks for now and continue with later tasks;
that does not count as passing the S4 gate. Live adapter checks and the clean
machine release checklist also remain open. Use the dated evidence in each
slice rather than treating an older test count or platform result as current.

The pre-existing unchecked entries are blocked: the release checklist and
packaged-artifact verification need clean macOS 13+ and Ubuntu 24.04 machines
with installed agents, signing needs Developer ID and Authenticode credentials,
and the scrape and snapshot refresh need approved sources, network access and a
published catalog. The catalog/agent-pool parent is otherwise implemented; its
only unchecked part is the deferred snapshot refresh. The agent model pickers
and login diagnostics are now implemented.

## Next feature

- [ ] **Freebuff interactive CLI adapter** — implement the agreed
  [spec](./spec/freebuff-adapter.md), keeping the existing standalone terminal.
  - [ ] **P0 — Characterize the installed runtime:** resolve a configured
    executable then PATH; distinguish the Nix launcher from its runtime,
    identify version and update behavior, and document observed prompt,
    streaming, tool, approval, question, interrupt and resume interactions.
    Do not install Freebuff. Surface detectable launcher updates and reject
    versions or screen states that cannot be identified reliably.
    - [ ] Resolve a configured executable followed by PATH without launching
      Freebuff or triggering its updater; report missing or unusable candidates.
      An offline discovery primitive reads `agents.freebuff.executable` from
      agent settings; wire it into the desktop factory and verify its fixtures
      when the selected shard includes them before marking this item complete.
      Missing and unusable-candidate fixtures passed in shard 6/8; configured
      precedence and settings fixtures passed in shard 4/8.
    - [ ] Inspect the installed launcher and runtime metadata without sending
      project data; record version provenance and the update lifecycle. A
      bounded metadata reader is implemented; its valid-metadata fixture passed
      in shard 7/8, and its malformed/oversized fixture passed in shard 4/8.
      Finish verification and wire it into update/restart handling; do not treat
      metadata as proof that the runtime is supported.
    - [ ] With user-approved live messages only, capture the terminal states
      needed to establish a version-specific interaction contract. Prefer
      question-only messages requiring tools from the user's ChatGPT “tools”
      project, using the corresponding Shepherd project at
      `/Users/me/src/me/tools`. Confirm each exact message and its context
      before submission; request examples if the ChatGPT project is inaccessible.
      Candidate supplied by the user: “give me the dotenv fragment to override /
      ignore battery / wifi and run ar3 regardless”. It is not yet approved.
  - [ ] **P0 — Capture fixtures and define contracts:** create sanitized,
    versioned PTY/screen fixtures and a deterministic fake CLI. Map terminal
    states to harness events; identify necessary contract changes for
    questions and transcript corrections. Cover split bytes, ANSI/VT redraws,
    reflow, prompt boundaries, ambiguous output and process exits.
    - [ ] Implement and verify ordered, private capture storage for PTY input,
      output, screen states, runtime changes, responses and worktree effects;
      connect recording and replay once the transport exists. The storage
      primitive compiles; its invalid-name and ordering fixture passed in shard
      8/8, while its round-trip fixture has not yet been selected.
      A writer now refuses further records after an I/O failure; its failure
      fixture was not selected in shard 2/8. The transport must stop live
      submissions on that error.
    - [ ] Serialize PTY operations and resulting screen records with the
      capture session. A fake-process fixture passed in agent-harness shard
      3/8, including failure propagation. The PTY service now offers an opt-in
      recording path for input, output, resize and screen frames; its fake
      interactive PTY fixture passed in Shepherd shard 4/8. A failure monitor
      now requests normal terminal cleanup and reports
      cleanup errors; its failure fixture was also outside shard 1/8. Connect
      this path and its error channel to the Freebuff terminal host. Recorded Unix PTYs now
      use nonblocking writes: each successful write and its recorded bytes are
      serialized with output capture, and readiness is polled outside the lock.
      The backpressure fixture passed in Shepherd shard 3/8. The capture
      primitive's nonblocking fixture passed in agent-harness shard 5/8.
      Failed sessions must never be treated as complete replay.
      A failed process spawn clears its recording attachment and invalidates
      the capture; this error path has not been exercised since the no-build request.
      The waiter now invalidates captures if PTY output cannot be confirmed
      drained; its new fixture was outside Shepherd shard 7/8. The live runner
      must wait for that drain result before finalizing effects.
    - [ ] Feed recorded screen frames to the integrated adapter. An opt-in
      bounded observer now publishes each frame after capture; the interactive
      fake-PTY fixture verified observer and capture order in Shepherd shard
      4/8; the queue-full fixture passed in shard 3/8. The earlier shard's other
      tests passed except `watcher_error_survives_later_file_events_and_is_reported_once`,
      which was interrupted after running for over a minute.
      The terminal host can now prepare a recorded PTY; its host fixture was
      outside shard 3/8. The shell's task-terminal registration path now also
      accepts a recording, preserving its existing process and terminal
      lifecycle. A fake-PTY fixture now checks that a recorded task terminal
      appears in the project list and exposes a screen; it has not yet run.
      Connect it and its error channel to the integrated adapter and capture
      runner.
    - [ ] Finalize captures only after terminal output, responses, and worktree
      effects are drained; require a completion marker for successful replay.
      The capture API rejects missing or trailing records; its completion
      fixture passed in agent-harness shard 5/8. The terminal host and
      replay callers still need to use this API; partial captures remain
      available for diagnostics.
      A worktree-effect collector records specified regular files, deletion,
      and file modes without following symlinks, invalidating unsupported or
      oversized captures. Its fixtures were outside shepherd-core shard 7/8.
      Git status now supplies tracked, untracked and rename paths; staged or
      ignored files are rejected rather than omitted. The unstaged/untracked
      discovery fixture passed in shepherd-core shard 1/8; the staged/ignored
      fixture remains unverified. Effect collection now compares HEAD,
      branch, index entries and index flags against a pre-run identity; its
      branch-change fixture was outside shard 5/8. Detect or reject other Git
      metadata changes and handle other effect types. Replay planning validates
      complete captures, modes, size limits and duplicate paths; its fixture
      passed in shepherd-core shard 8/8. Unix replay now applies supported file
      effects using confined directory handles after checking a clean worktree
      against the identity recorded before capture. Replay planning rejects
      missing, duplicated or late identities; its updated fixture passed in
      shard 8/8. The application fixture was outside that shard. Replay now
      reports the failing path and warns that the worktree may be partially
      modified; its partial-failure fixture was outside core shard 7/8.
      Connect capture and replay before live use.
      A capture lifecycle now records the clean base and, after the caller
      confirms terminal drain, collects effects before finalizing. Its combined
      capture/replay fixture was outside shepherd-core shard 1/8; connect the
      shared session to a runner that proves process termination and drain.
      An ignored `approved_freebuff_capture` runner is feature-gated and
      compile-checked. Its new offline fake-CLI end-to-end fixture was outside
      Shepherd shard 5/8 and must pass before any approved live use. The runner
      waits for operator send/stop markers and does not infer readiness or
      completion.
    - [ ] Replay capture records in order through a streaming consumer. An
      offline synthetic capture replays split output and a resize through the
      shell's VT parser (Shepherd shard 8/8 passed); the consumer's own fixture
      was not selected in shard 6/8. A verifier now compares recorded frames
      with screens reconstructed from captured bytes and resizes, and the fake
      runner verifies its saved frames before writing the completion marker, so
      a mismatch leaves an incomplete capture. Its new
      fixture and the updated fake runner remain unverified; the user requested
      no builds. Verified screen replay now exposes each reconstructed frame and
      its capture timestamp in order to a visitor, which must defer external
      effects until replay succeeds. Connect it to versioned Freebuff screen fixtures, the screen
      classifier and worktree effects; make verification mandatory for replay.
    - [ ] Version the capture format and reject incompatible headers. The
      header rejection fixture passed in shard 4/8; the capture round-trip
      fixture has not yet been selected.
    - [ ] Translate verified redraws into transcript text replacements. The
      harness now supports replacing an existing streamed row, with its fixture
      passing in agent-harness shard 5/8. A bounded Freebuff text reconciler
      emits deltas, replacements and no events for repeated snapshots; its
      fixture passed in agent-harness shard 7/8. Its publisher bounds queued
      events and releases blocked producers when the receiver closes; the
      publisher fixture was not selected in shard 7/8. Connect it to a verified
      screen classifier and the adapter event stream.
  - [ ] **P1 — Implement the integrated adapter:** launch the installed CLI in
    the task worktree with the existing login, forward input with normal CLI
    terminal interpretation, stream and reconcile text, map tool activity,
    completion and errors, and persist normalized transcript events. Bound
    buffers, clean up the PTY/process tree, and fail visibly on uncertainty.
  - [ ] **P1 — Bridge interactions and lifecycle:** connect approvals and
    questions to existing Shepherd controls, implement normal interruption and
    prompt-readiness confirmation, support multi-turn sessions, and implement
    restart resume only if the runtime demonstrates it. Advertise only proven
    capabilities; use Freebuff defaults for model/agent initially.
    The current approval contract supports decisions rather than free-text
    answers; the question presentation and answer path await the user's choice.
  - [ ] **P1 — Integrate the picker:** expose the integrated agent separately
    from **Freebuff (terminal)**, preserve the terminal's existing behavior,
    and test task routing, persistence, diagnostics, and update status.
  - [ ] **P1 — Share the live terminal:** keep the integrated Freebuff PTY
    visible and interactive in the terminal panel. Open it automatically when
    the integrated task starts, but let the user switch inspector tabs. Record direct terminal
    prompts and replies in the conversation; serialize both input paths and
    replace an unfinished terminal prompt when the composer submits. Fail
    clearly if the input cannot be recognized or cleared reliably.
    - [ ] Emit recognized terminal-origin prompts through the harness's new
      transcript event; its deduplication and restore fixture passed in
      agent-harness shard 8/8. Admit terminal-origin turns before forwarding
      submit: establish the Git baseline, persist the prompt and transition the
      task state, with failure preventing submission. Avoid echoing
      composer-origin prompts. A later event alone is insufficient because the
      CLI may have already changed files before the core handles it.
      A session-level persistence step now saves a terminal prompt with a
      working task and rolls back its in-memory transcript on save failure;
      its fixture was not selected in shepherd-core shard 8/8. Core admission
      now establishes the baseline, persists the prompt and task, then updates
      the in-memory task state; its baseline and gate fixture passed in
      shepherd-core shard 5/8. An
      internal submit gate serializes admission and input, blocks forwarding on
      admission failure, and marks ambiguous write failures; its failed-admission
      fixture passed in shepherd-core shard 6/8. Connect every integrated Freebuff input
      path and the verified screen classifier to the gate before enabling it.
  - [ ] **P2 — Validate and document:** run offline fixture and fake-CLI
    coverage by default. Gate genuinely live probes with `live-freebuff`,
    disabled by default. Before each live submission, confirm the exact
    message, project/worktree and potentially sent files or context with the
    user. Use few metered queries, choosing useful tasks in a real user project
    and a disposable worktree with the existing login. Cache every response,
    including partial responses, raw PTY bytes, terminal-size changes and
    rendered terminal-state transitions in order, plus assertion-relevant
    effects under `.cache/` for replay. Record version,
    limitations, safe capture and replay instructions, and evidence for
    streaming, interactions, cancel and restart behavior.

- [ ] Add a scripted split-test workflow for small commits:
  - [x] Keep ordinary Rust tests on the standard harness; add a pinned
    `cargo-nextest` to the Nix development environment for runtime hash
    partitioning. Keep live-agent tests behind their existing per-agent gates
    and preserve their caching requirements.
  - [ ] Provide an explicit test command, with no Git hooks, that runs tests
    for affected packages plus one rotating shard of the remaining default
    Rust and Playwright suites. Define and document the changed-path mapping,
    including shared files, deletions and unknown paths; choose a conservative
    fallback so affected coverage is not silently omitted.
  - [ ] Default to eight shards and select a shard deterministically using a
    revision-derived value modulo the shard count. Selection need not cover
    every shard within any eight-commit cycle; tests should run regularly over
    time. Allow an explicit shard count and selection, and explain behavior for
    uncommitted changes, branches and rewritten history. A full default-suite
    run is due every eight commits; report when it is due rather than installing
    a hook. Run the complete default suite in pull-request CI as well.
  - [ ] Provide explicit full-suite and individual-test commands, plus a way
    to replay a revision's shard selection. Include Rust doctests and other
    default tests that nextest cannot execute, and preserve existing platform,
    native smoke and live-agent gates instead of silently expanding them.
  - [x] Record actual successful runs with the revision, selection and runner
    versions so a diagnostic command can identify when a test was last
    confirmed to run. Do not infer success from a scheduled shard. Document
    how to bisect a failure by running that test at each candidate, and how
    to handle missing history or tests that did not exist at older revisions.
  - [ ] Verify shard partitioning and selection, affected-path fallback, full-run
    cadence and replay with representative fixtures. Check Playwright's shard
    membership behavior rather than assuming it matches nextest's stable hash
    partitions, and document any limits on historical replay.

- [ ] Consider extracting reusable test-sharding pieces into my-nix. Wait until
  the implementation has been verified and the user has confirmed that the
  workflow works for them in practice. Then conduct a short interview about
  which pieces to move, such as runner orchestration, Nix dependencies,
  configurable test selection, run history, and agent instructions; agree on
  the shared and repository-specific boundaries before implementing extraction.

- [ ] Rework the ignored linked-worktree file-watcher test to be less dependent
  on filesystem state. It can block during macOS FSEvents registration before
  its refresh timeouts start; restore it to the default suite once reliable.

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
passed, unless the user explicitly directs work to continue past an open gate.
Record that exception and leave the gate open until its evidence is available.

If only four slices can be built, build **S0–S3**. That is the smallest version
of the product that can answer: “Can I run a real agent, review what it changed,
and merge it without leaving Shepherd?”

## Starting point on 2026-11-04

This table is the historical baseline for the plan. It does not describe the
current implementation or current test counts.

| Area | State at the start of the plan |
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

- [x] Complete `TerminalEngine`: scrollback, alternate screen, wide characters,
  reflow, Kitty keyboard protocol, selection, copy/paste, search, mouse and
  hyperlinks. Kitty modes negotiate independently for each screen, including
  when the screen switch is part of a synchronized update. Fixtures cover
  reflow and key encoding. The native smoke drives Vim, less and
  composition events. Native PTY probes verify Kitty negotiation and key
  reports, including the distinct replies on each side of a synchronized
  screen switch, plus bracketed paste and SGR mouse reports. Interactive fish verifies
  distinct Ctrl+I and Tab bindings. Native IBus Pinyin verifies candidate
  selection and placement on Ubuntu. The candidate-based macOS Pinyin smoke is
  human-gated; its repeatable procedure is
  [macos-ime-smoke.md](./macos-ime-smoke.md), and it has not been run here.
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
  passed the native IBus probe. Normal-suite tests also cover the configured
  scrollback tail, main-screen cursor restoration after leaving the alternate
  screen, and a combining mark split from its base and following wide glyph
  across process calls. At this commit, `cargo test -p shepherd --lib
  terminal_screen::tests` passed 28 tests, the full `cargo test -p shepherd
  --lib` suite passed 127, and `cargo test -p shepherd --test shell_scope`
  passed. The macOS Pinyin candidate smoke remains human-gated and is not
  claimed as passed.
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
  snapshot and subsequent output reach the pane without restarting the shell
  or showing an error; unrecoverable acknowledgement failures are reported.
  The two-launch hidden macOS terminal smoke also passed after this change,
  including task switching, terminal reattachment and restart.
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
- [x] Pi byte-oriented LF reader, ID correlation, `agent_settled`, extension UI
  bridge and approval bridge.
  The stdio transport and ID-based command and dialog replies are implemented.
  Pending RPC requests fail when output closes, without waiting for a lingering
  process to exit; a local subprocess fixture verifies the disconnect path.
  A failed stdin write also closes pending requests and stops the process group,
  even when the agent leaves stdout open; a local descendant fixture covers it.
  On Unix the RPC process has its own group; explicit shutdown or dropping an
  active transport stops that group, with a local descendant fixture. The
  supervisor also accepts shutdown while waiting after output closes and stops
  the group on a wait timeout; local disconnect fixtures cover prompt shutdown
  and automatic timeout cleanup of a descendant without a shutdown request.
  Shutdown reports process-group signal failures while retaining the unreaped
  leader, so another shutdown can safely retry; an injected-failure fixture
  verifies the leader remains available and the retry succeeds.
  A failed child wait is also reported and retried without signalling the
  group again; a local fixture verifies that the child handle is retained.
  Descendants surviving an already-reaped leader still need recovery. A macOS
  experiment that observed exit before reaping successfully stopped a surviving
  child, but signaling an empty exited group returned EPERM in existing tests;
  that approach was not retained pending a safe way to distinguish the cases.
  Startup waits for the exit report so it can retain process diagnostics.
  The installed Nix-profile Pi 0.75.4 passed the isolated dialog smoke, but its
  prompt reply omitted `disposition` and its shipped RPC/session code lacks
  `agent_settled`. Continue with ACP while the Pi lifecycle probe is blocked.
  The Pi reference checkout documents the required contract in
  `packages/coding-agent/docs/rpc.md` at `a7a75a9dc6891281eed58906c07c55929e7b6c13`.
  `PiHarness` now drives a Pi session through the shared adapter interface:
  `agent_start` opens a turn, `message_update` text and thinking deltas stream,
  tool execution start/update/end become one tool row, and `agent_settled`
  finishes it (cancelled when `aborted` or after an `abort` command). `cancel`
  sends `abort`; shutdown closes stdin and reaps the process group. A `confirm`
  extension dialog becomes an approval card answered by its ID through
  `extension_ui_response`; other dialog methods are logged rather than mapped to
  a decision. Reopening sends `switch_session` with the stored session file and
  refreshes the state, so the adapter session ID is that file; a session started
  with `--no-session` reports no resume support. The harness writes its bundled
  `pi-approval.ts` extension to a temporary directory, starts Pi with
  `--extension`, and fails startup if the extension cannot be installed. Every
  tool call then raises a confirm dialog that the approval cards allow or deny,
  so the task no longer reports `unreviewed tools`; a Pi started through
  `PiHarness::without_approval_extension` still does, and its header and
  diagnostics say the calls follow Pi's configuration. A live model turn and
  real extension approval recording were not run here; the human-gated
  procedure is [pi-live-smoke.md](./pi-live-smoke.md), and that criterion is
  not claimed as passed. The normal suite proves the transport and mapping:
  `preserves_unicode_separators_and_routes_reversed_responses` covers LF-only
  framing and reversed response IDs; `refuses_an_unterminated_record` rejects
  a missing final LF; `harness_streams_a_turn_and_settles_on_agent_settled`
  covers text/thinking deltas, one tool row and completion on
  `agent_settled`; `harness_cancellation_settles_and_stops_descendants`
  covers abort; `subprocess_routes_an_extension_dialog_and_keeps_stderr_off_the_wire`
  covers the dialog round trip and stdout-only protocol;
  `harness_answers_the_recorded_extension_approval` replays the recorded
  confirm; and the bridge tests cover installed allow/deny and the
  `unreviewed tools` fallback. `cargo test -p agent-harness --lib pi::tests`
  passed 21 tests, and the installed Pi 1.5.0 dialog probe passed with
  `--features live-pi`. At this commit, `cargo test -p shepherd --lib` passed
  127 tests and `cargo test -p shepherd --test shell_scope` passed. The bridge
  currently maps `confirm` dialogs to approval cards; other extension methods
  remain logged rather than mapped to a decision.
- [x] ACP adapter using `libs/acp-client`; no duplicate protocol client.
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
  Per-tool confirmation overrides a global allow; a fake-agent fixture checks
  that the approval request reaches the client in that case.
  Invalid deny or confirmation patterns cannot silently turn into an automatic
  allow; policy fixtures cover both and preserve explicit deny decisions.
  Agent-cancelled permission requests release their pending response task; a
  fake-agent fixture checks cancellation and rejects a late UI answer. The
  harness withdraws the matching displayed approval and skips cancelled queued
  approvals; fixtures cover both and a subsequent request in the same turn.
  When an answer and cancellation are both ready, cancellation wins. The
  harness waits for the SDK to accept the response before recording a reusable
  grant; fixtures cover cancellation priority and verify that failed confirmation
  creates neither a session nor project grant. This is transport acceptance,
  not an acknowledgement from the agent.
  Project-scoped grants persist per configured agent. Settings → Projects
  lists saved grants and live session grants by task and can reset either.
  ACP terminals use Shepherd's PTY engine and appear in the selected task's
  inspector. Fixtures cover input, resize, exit status, release, and cleanup
  on disconnect. A live Claude Code smoke is tracked in the gated smoke item.
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
- [x] Pi lifecycle and approval bridge contract fixtures when those paths are
  implemented.
  A recorded Pi 1.5.0 offline session in `fixtures/pi` replays through the
  transport and pins the startup state shape, the extension approval request
  and reply correlation, the handled prompt disposition and the exit report;
  the replay rewrites only the request IDs, as a live Pi echoes them. The
  recording is an offline extension command, so it holds no agent-turn
  lifecycle records; `PiHarness` maps those shapes in a fake-Pi script that
  covers `agent_start`, text and thinking deltas, tool execution and
  `agent_settled`, and the recorded confirm dialog replays as an approval the
  harness answers. A second script answers the installed extension's allow and
  deny confirmations, and capabilities report `unreviewed tools` only when the
  extension is absent. None of this is a live model capture; recording a real
  turn stays in the gated smoke item below.
- [x] One live smoke per installed OpenCode, Pi and Claude/ACP integration,
  gated in CI.
  The opt-in Shepherd adapter smoke workflow selects one preinstalled agent on
  a configured Nix runner. `scripts/check-adapter-smoke-config.sh` runs before
  the probes and rejects an empty settings variable, an unreadable settings
  file or a non-executable Pi path, so a missing integration cannot silently
  pass; the live tests themselves stay behind `live-opencode`, `live-pi` and
  `live-claude`. `adapter_smoke_workflow_is_opt_in_and_covers_each_integration`
  checks the dispatch inputs, self-hosted runner, feature flags, test filters
  and documentation, while `adapter_smoke_gate_rejects_missing_configuration`
  proves the gate fails for all three agents and accepts a readable settings
  file or executable. OpenCode 2.0.14 passed all seven installed probes
  (session create/resume, mode switching, text and image turns through a local
  fixture, sign-in failure, approval/deny/cancel and ACP fallback). Pi 1.5.0
  passed the installed RPC dialog probe. Claude Code is not installed here and
  the workflow has not been dispatched on its CI runner; both are human-gated,
  documented in [adapter-live-smoke.md](./adapter-live-smoke.md), and not
  claimed as passed.
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
  results, and label incomplete results.
  Content-search excerpts map lowercased offsets back to original characters,
  including lowercase expansions, so long Unicode lines retain the match.
  A debounced watcher refreshes Git
  badges for the selected root, including changes to the index and HEAD. A
  missing external Git metadata directory is skipped, while inspection errors
  are reported rather than silently disabling that part of the watcher. A
  pending watcher error survives subsequent file events during debounce;
  additional errors are included up to a bounded limit and logged. After a
  notification error the bridge reports it and continues watching; a fixture
  checks a subsequent real worktree change. A
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
  A repeat after bounding search snippets measured 16.7 ms p95 with the same
  fixture; it likewise excludes debounce and rendering. After transcript write
  batching, another run measured 17.0 ms p95 using directly populated records;
  a separate recovery fixture verifies search across batched writes after reopen.
- [x] Attachments and images, `@file` mentions, model/mode pickers.
  The composer queues removable local files and copies them into the task
  attachment cache on Send. Prompts retain the copied paths. Codex sends local
  images through app-server or through exec when its help advertises images.
  Native OpenCode sends structured file URIs when the API advertises them.
  ACP receives image bytes when negotiated and file links otherwise. ACP can
  read the selected linked text files through Shepherd's bridge during the
  sending turn. Turso saves composer attachment metadata with the prompt
  entry and restores the attachment order when reopening a task.
  Reusing a content-addressed attachment or preview cache file verifies its
  bytes first; a mismatch is reported and the existing file is preserved.
  Resolving an attachment for a prompt also verifies its filename and bytes
  against its ID while enforcing the size limit. A fixture covers same-size
  corruption for both cache paths and rejects a modified prompt attachment.
  Path-based adapters can still observe a replacement after this verification.
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

- [x] Crash and kill tests: interrupted state, orphan detection, recorded PID
  plus start time, reused-PID refusal.
  A headless subprocess fixture is killed with SIGKILL while a scripted agent
  waits for approval; reopening its database restores the prompt and marks the
  task interrupted without inventing the unfinished answer. It also verifies
  that an error notice recorded during the turn survives the crash, a scripted
  resume, a new turn, and a second reopen without transcript sequence collisions.
  PTY process-group shutdown checks each member's recorded start time before
  signalling; a fixture rejects a matching PID with a different start time.
  A headless SIGKILL fixture verifies that a surviving PTY process is recognised
  from its persisted identity after restart, then cleans up that test process.
  Project and task terminal launches now persist PID, start time, process group,
  session, and boot identity per launch. On exit, remaining background jobs
  are recorded before the leader record is removed; PTY fixtures verify
  persistence and cleanup. Shared process-record capture rechecks the start
  time, session and group before returning an identity; a local process fixture
  verifies its fields. Startup snapshots earlier records for read-only
  inspection against boot, start time, process group, and session identities;
  a match does not establish that another Shepherd instance has released it.
  ACP terminal launches also record and clear their process identities. A
  startup notice shows matching and uncertain records and allows a recheck.
  Closing an individual terminal reports process-table enumeration failures
  before changing its session; an injected-failure fixture verifies retry.
  Whole-app terminal cleanup retains sessions on scan or close errors and the
  bridge allows quit to retry after the core has closed; local fixtures cover
  both. Hangup and forced-kill signal failures also propagate while preserving
  live and exited-session records; injected-failure fixtures verify retries.
  Running direct ACP terminals now signal their process group on kill, release
  and shutdown, and report signal termination in the ACP exit status. Agent
  Codex app-server and OpenCode's native server now run in their own process
  groups and stop those groups on normal shutdown; local fixtures check that
  descendants cannot outlive shutdown. Codex also stops the group on malformed
  output or a closed output stream while the server is still running; local
  fixtures exercise both. If the input writer fails while stdout remains open,
  the reader reports the failure, stops the child and releases pending requests;
  local subprocess fixtures cover that path and an unexpected writer exit while
  stdout has a partial frame. If process-group cleanup fails after a reader
  error or closed stdout, the error is reported and the child remains available
  for a later shutdown retry; an injected-failure fixture covers both paths.
  Failed or cancelled Codex initialization also stops
  its group, covered by a local fixture. Cancelling Codex project or model
  discovery while awaiting a response also stops the group; a fixture covers
  both paths. OpenCode also stops its group on a
  disconnected turn. OpenCode stops the process group if server initialization
  fails or is cancelled, and on subscription failure or cancellation; local
  fixtures cover initialization, subscription, and project-mode discovery
  cancellation. Discovery reports simultaneous request and cleanup failures
  while hiding configured launch environment details. Codex app-server task
  launches now record the process identity before initialization and remove it
  after shutdown; save failures stop startup and failed removal can be retried.
  A headless SIGKILL fixture reopens the database and verifies surviving
  scripted app-server, exec and ACP processes against their recorded identities;
  the fixture then stops them. Local fixtures cover shutdown paths and durable
  storage. OpenCode's
  native server is also recorded before its startup handshake; a failed record
  write or cleanup blocks ACP fallback. Failed native startup stops the process
  and clears its record before fallback when cleanup succeeds. Record removal
  during normal shutdown can be retried. A local HTTP fixture covers the
  lifecycle. Records from interrupted startup may remain for
  inspection. ACP task launches, including OpenCode fallback and Claude, now
  record their process before protocol initialization and clear the record
  after the child is reaped. Save failures stop startup, and failed removal
  can be retried; local fixtures cover both and durable storage. Codex exec now
  records each per-turn process and retries failed record removal before a new
  run or shutdown. Local fixtures cover completion, cancellation and storage
  failures. When an agent leader exits, its record is retained if the recorded
  group and session still have members. Inspection reports the group as
  unverified, since reuse prevents proving ownership; a local fixture covers
  retention and eventual cleanup.  Group scans are snapshots and do not prove
  that every descendant is found. When the leader has exited, a recorded group
  can still be stopped: each launch carries a token in
  `SHEPHERD_PROCESS_TOKEN`, descendants inherit it, and Shepherd signals a
  surviving session member only while its environment still carries that
  launch's token (read through `KERN_PROCARGS2` on macOS and
  `/proc/<pid>/environ` on Linux). A record without a token, or a member whose
  environment cannot be read, makes the stop fail without signalling anything.
  Real process-tree fixtures cover a stopped tokened member, a member of
  another launch and an unrelated process left running, the untokened refusal,
  and the original group stop while the leader still matches.
  Newly recorded agents hold a per-record OS lock while owned by this instance;
  cross-process and crash fixtures verify that another process cannot acquire
  it until the owner exits. A record written before the launch token or the
  per-record lock existed carries neither: inspection still lists it, but a
  stop refuses without signalling anything because neither the lease nor any
  environment proves ownership, and the record stays listed for inspection. A
  record whose lease is still locked by a running instance, or whose lease file
  was replaced, is refused the same way.
  Lease files are retained so all instances continue to lock the same file.
  New lease directories are private; startup rejects a symlink, a directory
  owned by another user, or one writable by other users.  On Unix, lease files
  are created relative to an opened directory and its identity is checked
  before and after each claim. A recorded process can now be stopped from the
  startup notice: Shepherd adopts the retained lease, refusing one still
  locked by a running instance or whose file is no longer the entry it locked,
  and signals members while the recorded identity still matches, or by the
  launch token once the leader has exited. The lease stays
  held through termination, which escalates from `SIGTERM` to `SIGKILL` after
  500 ms only while the leader still matches or the member still carries the
  launch token; a fixture covers an owned lease,
  a replaced lease file, a reused start time and a completed stop.
  ACP supervision reports a failed process-group signal even if killing the
  leader succeeds, with an injected-failure fixture. ACP quit now waits for
  process supervision and reports its failure; timeout retains the connection
  for retry. When a stop or wait fails, the supervisor retains its child and
  retries on a later quit.  Local subprocess fixtures cover shutdown and an
  injected stop failure followed by retry. When an ACP leader exits while
  descendants survive, its record is retained and the startup notice reports
  the group, which the token-verified stop above terminates. ACP agent launches
  pass the launch token through their spawn spec and ACP terminals receive it
  from the PTY launch; a fixture exports the token from a spawned ACP child, and
  an end-to-end fixture spawns a tree through the ACP transport, lets its
  leader exit, and stops the launch while leaving a member of another launch
  and an unrelated same-token process running.
- [x] 30-day archived-worktree collection, dirty skip, branch-tip recreation.
  Startup preserves dirty worktrees and lists collection failures with their
  reasons; clean collection retains the tip and prunes Git worktree metadata.
  Collection also skips worktrees containing Git-ignored files and reports the
  reason; fixtures check the files and collection journal remain intact. The
  scan and Git removal are separate operations, but a modified or untracked
  file created after the scan still blocks removal because Git rechecks the
  checkout; a fixture covers that. An ignored file created in that window can
  still be removed along with the checkout.
  Opening a task, using its terminal, and accessing its files or Git views
  refresh a separate last-use timestamp, coalescing repeated input and reads
  to at most one write per minute per activity source. Older databases
  initialize it from task update time. ACP agent terminals carry their task
  identity; their output and user input refresh that task's worktree usage.
  Collection records its intent before removal. If the final database write
  fails, startup verifies the retained Git reference before finishing the
  missing-path record. Fixtures cover this recovery, a missing retained ref,
  an unrelated missing checkout, and a worktree that became dirty while a
  collection was interrupted. Opening an archived task with a missing checkout
  and a pending collection first completes recovery; failed verification
  preserves the intent for retry. Startup also reconciles pending collections
  for recently used tasks, keeping any checkout that still exists. Reopening
  an archived task also verifies pending collection recovery first. Existing
  paths are validated as task worktrees on the recorded branch before their
  pending markers are cleared; fixtures cover symlink and branch replacements.
- [x] Database backup, migration checks, malformed migration handling and
  transactional writes.
  Existing file databases receive a retained Turso snapshot in a private
  `<database>.pre-migration-<id>/snapshot.db` directory before pending
  migrations; a failed backup, unknown migration or gap in migration history
  blocks the migration before schema changes. File-database openers resolving
  to the same canonical path hold a retained migration lock across existence
  checks, backup and migrations; a database appearing during initial path
  resolution is rechecked, with a fixture for that race. Concurrent-open
  fixtures verify waiting and a
  single preserved snapshot. A separate-process fixture verifies that another
  opener waits and proceeds after the lock holder is killed. Lock contention
  uses cancellable async retries; a fixture verifies cancelling an opener does
  not occupy a blocking worker while the lock is held. Unix checks reject
  dangling database symlinks and symlinked lock files. On Unix, the acquired
  lock is checked against its pathname; a fixture verifies replacement while
  waiting is rejected before database open. The database pathname is also
  resolved again after acquiring the lock; a fixture rejects an alias redirected
  during the wait. Existing hard-linked databases are rejected on Unix because
  their aliases would not share a lock; a fixture checks both aliases and a
  successful reopen after one is removed. Path or link changes after these
  checks remain possible; hard-linked aliases remain unsupported on other platforms.
  A Unix fixture denies backup
  directory creation, verifies the old history remains intact, then retries
  successfully after access is restored. Every snapshot is checked for a
  readable SQLite header before it is retained; a failed write, verification or
  rename removes the incomplete directory and reports the reason, and a fixture
  injects a verification failure, checks no backup artifact remains and retries
  the migration successfully. An inaccessible database path also
  fails before opening or migrating instead of being treated as a new database.
  Turso's parser validates migration statements before changes and rejects
  transaction-control statements that could escape the migration transaction;
  fixtures verify rejection before any migration or history table is created.
  A migration list with an empty or duplicate name is also rejected before any
  schema or history changes; a fixture covers both cases.  A failed
  statement rolls back its migration and history entry; a fixture retries the
  repaired migration on the same connection. Transcript batches reject missing or mismatched task/session
  owners; fixtures verify failed attachment updates roll back the whole batch.
  Normal task-state saves and interrupted shutdowns commit eligible transcript
  entries in the same transaction. Both transcript write paths explicitly
  roll back failures and permit immediate retry. A failed shutdown save retains
  its session and can be retried while draining. Empty transcript batches still
  verify the persisted session and task before
  updating state; a fixture checks missing sessions, valid retries and deleted
  tasks in both stores. Multi-task archive rolls back on a mid-batch database
  error and can be retried. Task deletion clears the project's
  last-task reference and changed terminal layout in the same transaction and
  rolls back a mid-deletion failure, preserving those records, transcripts and
  attachments for retry. Draft creation commits the project selection and new
  task together, with a rollback and retry fixture for a failed task write.
  Batch archive rejects duplicate task IDs in both stores and changes only the
  state and timestamp, preserving newer task fields; a shared fixture checks
  rejection and a subsequent successful archive.
  The in-memory store takes task and project locks in the same order for draft
  and transcript saves; a concurrency fixture checks they cannot deadlock.
  It also holds the task, session, project and associated-state locks through
  task deletion; a concurrency fixture verifies a competing transcript save
  cannot recreate a deleted task.
  Project approval grants require an existing project in both stores; a shared
  fixture checks rejection and preserves the original grant on retry.
  Session saves require an existing task and cannot move an existing session
  ID to another task. Both stores reject a late save after deletion; the Turso
  check is part of the write statement.
  Task saves require an existing project and reject reassignment of an existing
  task ID. Both stores check ordinary, draft and transcript save paths; a
  rejected draft also leaves the other project's selection unchanged.
  Terminal saves similarly validate project and task ownership, reject owner
  changes and late writes, and preserve project-scoped terminals when a task is
  deleted; both stores have an ownership fixture.
  Terminal layout saves require an existing project; the same check protects
  task deletion, which rolls back if its accompanying layout cannot be saved.
  A shared fixture verifies both stores and a successful retry.
  Workspace layout saves require the task to exist at the write, so a late
  resize cannot recreate layout state after deletion; both stores are covered.
  Task deletion stages the checkout before the database transaction so a
  failed deletion can restore ignored files as well as tracked files.
  A deletion journal now survives task deletion and database reopen in both
  store implementations. A Git worktree move primitive preserves ignored
  files and registration while rejecting unsafe or occupied destinations.
  The journal can also record a random marker in the worktree's Git admin
  directory; a fixture verifies that the marker survives a move and rejects
  a different worktree identity.
  Startup recovery restores a staged checkout when the task remains and
  finishes removal after task deletion, checking the marker and paths first.
  Fixtures cover both outcomes, an already-finished removal, changed files,
  conflicting paths and a missing marker. A per-repository lock prevents
  another Shepherd instance from recovering an active deletion. Database
  failure and post-commit cleanup fixtures cover the live deletion path; the
  desktop does not restore attachments after the task has already been deleted.
  Attachment deletion uses a task-specific intent and an app-data lock;
  startup restores caches for surviving tasks and finishes removal for deleted
  tasks. Conflicting paths stop recovery, and older unidentified staging
  directories are left untouched. Local fixtures cover restart and lock cases.
  Separate headless SIGKILL fixtures interrupt both worktree and attachment
  deletion before and after the task transaction, then verify recovery.
  If an opened agent session cannot be recorded, the core stops it before
  retrying; a database-trigger fixture verifies cleanup and a subsequent start.
  A second fixture makes cleanup fail twice and verifies that the session is
  retained until cleanup succeeds, before another agent can start.
  Quit now returns an agent shutdown error and remains in the draining state
  with the session retained; a fixture verifies that a later quit retries it.
  Codex app-server shutdown propagates process-group and child-stop errors and
  retains the child handle for retry. An injected group-signal failure verifies
  the handle is retained and a later shutdown stops the process group. A
  separate injected child-stop failure also verifies handle retention and retry.
  App-server requests check for reader closure while holding the pending map
  lock, preventing registration after disconnect cleanup; a fixture covers
  the contested lock case.
  Cancelled app-server requests have their closed response waiters removed on
  the next request; a fixture checks 200 cancellations and a successful retry.
  Codex exec shutdown retains its waiter after a timeout so a later quit can
  retry, and reports a waiter failure on subsequent attempts. Cancellation and
  shutdown also report process-group or child-stop errors while retaining the
  child for retry. Local fixtures inject stop failures, cover both paths and
  verify that shutdown stops a real descendant process. A child-wait error is
  reported without declaring the turn finished, and shutdown retains the child
  for retry; a fixture covers errors before and after cancellation, and
  verifies that both cancellation and shutdown can report and retry the wait.
  One exec completion fixture reported an unexpected finish during a combined
  test run; the same fixture passed 200 isolated repetitions and the exec suite
  passed ten combined repetitions. Its assertion now prints the event sequence
  if the failure recurs; the cause remains unconfirmed.
  OpenCode shutdown retains its server when stopping it fails; an injected
  group-signal failure verifies the process remains available for a retry.
  A disconnected turn also reports a failed stop alongside the connection
  error and retains the child; a local fixture checks cleanup on retry.
  If OpenCode prompt admission fails and cancellation also fails, Shepherd
  stops the server, reports the combined error and blocks further prompts until
  cleanup; failure to clear its process record remains retryable. A local HTTP
  fixture covers this path. A cancelled admission future now stops the server
  in a background cleanup task, reports the outcome and blocks another prompt;
  local fixtures cover cancellation, a stop failure retained for retry and a
  future dropped from a thread outside the runtime. When the child handle is
  immediately available, the guard signals its group synchronously even after
  runtime shutdown; a local subprocess fixture covers that case. A contended
  child handle still depends on the runtime remaining available. A separate
  scripted core fixture verifies that an admission cancellation failure leaves
  the task failed, retains the prompt across database reopen and runs cleanup.
  Cancelling an OpenCode prompt attempts both queue removal and session
  interruption even if the first request fails; the local HTTP fixture verifies
  both errors are reported when both requests fail. If an active turn’s
  cancellation fails or its future is dropped, Shepherd stops the server and
  blocks further work until cleanup; local fixtures cover an interrupted request
  and successful cancellation without stopping the server. A failed cancellation
  reports request and cleanup errors together, retains cleanup for retry and
  has a local fixture covering both failures.
  Core sessions distinguish a disconnected adapter from completed cleanup.
  Retry, task deletion, batch archive and quit all attempt shutdown before
  discarding a disconnected session; an injected-failure fixture verifies that
  each operation retains it until a later shutdown succeeds.
- [x] Bounded transcript, stderr, search and frame buffers.
  Diagnostic history retains at most 256 KiB per record while draining longer
  lines, reports oversized records, and continues to later entries; a local
  log fixture covers recovery after an oversized line. New oversized diagnostics
  are stored as marked, UTF-8-safe field excerpts that fit the same limit;
  a fixture checks escaped content and retains both ends of the details.
  History seeks backward to the latest 200 records before parsing; a fixture
  checks ordering and identifies damaged records by absolute byte offset when
  earlier entries are skipped. The backward scan also stops after 64 MiB and
  reports the omission, keeping later complete records while skipping the
  fragment before the first complete line; a sparse-file fixture covers it.
  Search snippets cap the displayed matching word at 65 characters and compare
  case-folded text without allocating a copy of an arbitrarily long word;
  fixtures cover long Unicode matches and exact versus prefix matching.
  Codex, OpenCode and ACP stderr readers cap each line while draining the
  process, including newline-free output. OpenCode omits overlong lines to
  avoid exposing fragments of its authentication secret. ACP protocol lines
  are rejected above 8 MiB while reading, with a protocol error.
  Event queues are bounded by what producers may have in flight, not by the
  channel: streamed text, tool, plan and usage events keep their semaphore
  permit in the queued event, so a stalled consumer fills the budget instead
  of memory — 4096 units of 16 KiB for Codex app-server records, 64 chunks for
  Codex exec and OpenCode, 64 error permits for ACP, and 32 queued events with
  an explicit overflow for Pi. Turn-scoped control events (finish, error, mode,
  session, exit) are bounded by the protocol, and the shutdown, kill and
  stop-error channels carry at most one message per operation. The core emits
  at most 256 pending UI events before a `ResyncRequired`; a fixture keeps a
  drained stream event's permit held until the event is dropped. Pi admits at
  most 64 pending RPC requests, rejects excess requests and reclaims
  cancelled requests on the next attempt; a local subprocess fixture checks
  saturation and recovery. Pi also
  bounds unread RPC events to 32; overflow fails the transport explicitly,
  reserving space for its exit report. A subprocess fixture checks the queued
  events, error and process cleanup. Codex app-server, exec and
  handshake protocol lines have the same limit; oversized exec output fails
  the turn and stops its process group, including a descendant in the local
  fixture. Exec keeps draining output while it makes
  progress after process exit, with a 10-second limit and prompt cancellation.
  It fails the turn if the output pipe stalls or exceeds that limit instead of
  reporting incomplete output as successful. Codex version and help
  probes limit both stdout and stderr and time out if the command does not
  finish. Probe processes use isolated groups, with local fixtures for cleanup
  after a handshake or timeout. A timeout also stops a pipe-holding descendant
  when the leader has exited, before reaping the leader. ACP keeps
  only 64 unread stderr notifications while retaining its latest diagnostic
  tail. ACP terminal output honors smaller requested limits and caps larger
  requests at 1 MiB per terminal. ACP terminal output has a shared 64-event
  backpressure limit with 8 KiB chunks; release or shutdown unblocks a stalled
  pump. Large backend output events are decoded in 8 KiB pieces rather than copied
  wholesale into the intermediate UTF-8 buffer; a fixture covers characters
  split across both internal chunks and backend events.
  Direct terminal output uses the same incremental decoder, preserving
  characters split across reads and flushing an incomplete final sequence.
  A local subprocess fixture also checks split writes through terminal events.
  Cancelled terminal exit-wait futures remove their registry entries; fixtures
  also cover a reused terminal ID. Each terminal admits at most 64
  active exit waits and reports an error for excess requests; cancellation
  frees a slot and existing waiters still receive the exit status. The ACP
  handler honors request cancellation; a fake agent cancels 65 waits and then
  successfully waits for the same terminal to exit.
  A connection-wide limit of 256 exit waits also bounds handler tasks across
  terminals; a fake-agent fixture fills it, cancels the waits, and repeats.
  ACP allows at most 64 pending manual permission requests per connection;
  additional requests receive an error. A fake-agent burst verifies the limit,
  cancellation and recovery after the queued requests are released. OpenCode
  limits outstanding approvals to 64 and rejects duplicate pending IDs; a
  local event-stream fixture checks overflow and capacity after a reply. It shows
  one approval at a time and skips queued requests resolved by the agent. A
  local HTTP fixture sends two approvals and verifies both decisions and turn
  completion through the native adapter. Only the displayed approval can be
  answered, and a submitted answer cannot be sent twice while awaiting the
  agent's resolution event. If its HTTP request fails or is interrupted,
  Shepherd stops the server because delivery may be uncertain; fixtures cover
  both paths and a process-record cleanup failure followed by retry. The core
  folds already-queued disconnect events before returning an approval error; a
  fixture verifies a failure behind 300 stream events and database reopen.
  Resolved or withdrawn approval rows are saved during a live turn, while a
  pending replacement stays transient; a race fixture verifies both outcomes
  and interrupted recovery after reopen.
  The visible approval is withdrawn when the event stream fails or its reader is
  cancelled; fixtures cover queued requests and a dropped reader.
  Codex app-server likewise limits pending approvals to 64 and rejects a second
  request for an already pending item without replacing the first. Identified
  approvals for another session or turn, including a completed turn, are
  rejected; a fixture checks that a matching approval remains available. Requests
  omitting identity fields remain compatible, and an approval arriving before
  the start response identifies its turn cannot yet be checked against it.
  Identified resolution notifications from another session or turn cannot
  withdraw the active approval; a fixture checks both mismatches and a valid
  resolution. Notifications omitting those identities remain compatible.
  Repeated
  server request IDs are ignored while pending. A local protocol fixture covers
  saturation, withdrawal, retry and an unavailable event consumer.
  If enqueueing a Codex approval response fails, its pending request remains
  available for retry. The adapter now waits for its writer to flush the
  response before reporting the decision; if a write fails, times out or is
  cancelled after enqueueing, it prevents another answer while delivery is
  uncertain. Local fixtures cover failure, cancellation and withdrawal. A
  successful write does not establish that the agent accepted the decision.
  Codex app-server limits its outbound writer queue to 128 messages. Saturation
  fails a user approval before enqueueing so it can be retried; failure to queue
  a server-request reply ends the transport with a protocol error. A local
  fixture checks saturation and recovery after capacity becomes available; a
  subprocess fixture checks the reader reports the error and stops the child.
  ACP file reads and writes share a 32-worker limit per connection; excess
  requests receive an error. An injected exhausted-capacity fixture verifies
  both operations can be retried successfully when capacity returns.
  File requests cancelled while queued check cancellation before touching the
  filesystem; an occupied-worker fake-agent fixture covers reads and writes.
  Operations already underway complete rather than being interrupted.
  ACP terminal creation admits at most 32 pending starts per connection before
  spawning handler tasks; excess requests receive an error. A registry fixture
  checks that cancelling waiting starts frees capacity for another creation.
  A connection also retains at most 64 unreleased terminals, including exited
  terminals; a fixture checks that releasing one permits another start.
  Protocol cancellation while a start waits for the registry lock returns a
  cancellation response and does not launch the terminal; once startup begins,
  it finishes so the backend's resources remain tracked, then the terminal is
  released if cancellation was observed or response delivery fails. Failed
  cleanup is reported and retained for shutdown retry. Fake-agent fixtures
  cover cancellation before and during startup and a failed cleanup retry.
  Terminal kill and release handlers share a 64-request limit; saturation
  returns an error before spawning more tasks, and a fixture checks recovery.
  Cancelled kill and release requests waiting for the registry lock do not
  affect the terminal; registry and fake-agent fixtures check both and a
  subsequent successful release.
  Connection shutdown also unblocks pending waits if a terminal stop fails.
  It returns the stop failure and retains the terminal for retry; registry,
  connection and harness fixtures cover a failure followed by a successful retry.
  Direct terminal stop requests have a 64-request queue; excess requests fail
  explicitly and can be retried after capacity becomes available.
  A closed direct-terminal supervisor channel reports failure unless an exit
  was recorded, retaining the terminal rather than claiming it stopped.
  A direct terminal process wait failure is also returned to current and later
  exit waiters instead of being recorded as an exit with unknown status. The
  supervisor retains the child, permits stop retries, and retries the wait after
  a successful stop. Stop does not report success until the child is reaped;
  repeated wait failures retain it without signalling the group again. Registry
  and injected-failure subprocess fixtures cover these paths.
  A terminal backend stream that closes without reporting an exit likewise
  fails exit waits and retains the terminal for cleanup; a registry fixture
  covers the missing-status case. Terminal wait failures also reach the task
  transcript as error notices; the harness fixture covers propagation and the
  core redacts details when launch environment values are configured. A core
  fixture checks that terminal notices and agent turn errors are persisted during
  an active turn and survive task reopen, while unfinished text stays unsaved.
  Identical repeated wait failures do not create duplicate notices; changed
  failures and failures after a recorded exit are reported separately.
  Output permits remain held through the harness queue until the core
  consumes the events. A direct terminal's exit wait completes even if a
  descendant keeps an output pipe open, and shutdown rejects new terminals.
  ACP releases buffered early output and terminal-to-tool mappings after a
  released terminal's turn ends, preserving late tool updates during that turn.
  Early output is limited to 64 terminal buffers per turn, each capped at the
  existing command-output limit; overflow reports a connection failure. A
  fixture covers saturation, retained output and recovery after cleanup.
  A turn retains at most 256 released-terminal records for late tool updates;
  excess releases report a connection failure. A fixture checks the limit and
  that retained event capacity is released at turn cleanup.
  OpenCode decodes event-stream chunks one event at a time instead of
  queuing every decoded event from a network read. Its 8 MiB line and event
  limits are tested with fragmented lines and accumulated multiline data.
  It checks streamed text against the final message with a digest and length
  instead of keeping a second copy of every text fragment.
  OpenCode message and reasoning deltas use a 64-permit queue with 8 KiB
  UTF-8-safe chunks; the producer waits for consumption and a blocked producer
  can be cancelled. Codex exec message and reasoning events now use the same
  limits across turns, and its post-exit drain pauses its deadline while the
  consumer is backpressured. Local fixtures cover UTF-8, multiple turns,
  cancellation and a slow consumer. Codex exec tool events share that capacity;
  a fixture checks ordering, cloned-event permit retention and cancellation of
  a blocked producer. Codex exec plan and usage events also share that capacity;
  a fixture verifies ordering and retained cloned permits. OpenCode tool
  updates also share its text capacity; a mixed-stream fixture checks ordering,
  cloned permits and cancellation. Displayed OpenCode approval requests also
  use that capacity; queued approvals remain under their separate pending limit,
  so replies to queued requests can still be processed when the stream is full.
  OpenCode failure messages also hold capacity by their UTF-8 byte size and
  cap displayed details at 64 KiB with an omission marker. Fixtures cover
  Unicode truncation, permit release and a backpressured error.
  A fixture checks both reply handling and recovery when a permit is released. A core
  fixture fills the queue and verifies that 150 scripted messages and 150 tool
  calls reach the transcript in order. Codex app-server text, reasoning,
  command-output and tool events use a nonblocking 64 MiB serialized-size
  budget. Saturation fails the connection and stops the process instead of
  blocking the stdout reader; a local subprocess fixture checks cleanup and
  pending-request release. Fixtures verify that control replies remain reachable
  while the budget is full and after 100 unconsumed stream events. App-server
  approval requests now share that budget; a fixture verifies a resolved but
  unread approval retains its permit, excess requests are not registered, and
  ordinary replies still pass. Queued turn errors also retain capacity based on
  their own text, including errors saved before a small completion message;
  a fixture checks capacity release and control replies. Other critical queues
  are bounded as described above.
  App-server completion notifications for a different identified turn leave
  the active turn and its pending approvals untouched; a fixture covers stale
  and matching completions. Turn errors are retained only when their ID matches
  an active turn; missing IDs do not match an idle session.
  Unsolicited starts and completions while idle no longer enqueue turn events;
  a burst fixture also checks unrelated starts cannot replace the active turn
  and duplicate starts cannot erase its reported error.
  Thread-start notifications cannot replace the opened session ID; a local
  protocol fixture checks that a subsequent interrupt targets the right thread.
  Identified message, reasoning and tool events from another thread or turn are
  ignored, including after completion; a fixture also preserves fast-start and
  legacy events without identity fields. Events arriving before a turn is
  identified cannot yet be checked against it.
  A completion without a turn ID during a pending or active turn is a protocol
  error instead of a false completion; a fixture checks state and approvals are
  not changed before the error is handled.
  A subprocess fixture verifies that this error reaches the connection, releases
  pending requests and stops the agent process group, including a descendant.
  A successful turn-start reply missing its turn ID also stops the app-server
  because admission is uncertain; a subprocess fixture verifies descendant
  cleanup. A timed-out turn-start request also stops the server and its process
  group; an offline subprocess fixture verifies descendant cleanup. Dropping an
  in-flight admission marks the connection closed, signals its process group and
  reports cleanup asynchronously; a local fixture verifies descendant cleanup,
  pending-request removal and a subsequent shutdown. An injected signal
  failure is reported while retaining the child for a later successful shutdown.
  If the child lock is
  contended, cleanup still depends on the Tokio runtime remaining available.
  The same guard protects cleanup of a malformed successful reply; a subprocess
  fixture cancels that cleanup while the child lock is held and checks that
  cleanup completes after the lock is released.
  App-server turn starts are emitted by the protocol reader before completion,
  including when a fast turn finishes before its start reply is processed;
  an offline fixture checks reply and notification ordering. While that reply
  is pending, stale or duplicate identified completions do not close or
  complete the announced turn again.
  Event folding handles at
  most 256 events per session per pass, including during commands, and
  reschedules remaining events so one busy agent cannot monopolize other
  sessions. Shutdown drains the remaining events. The core buffers at most 256
  events between drains; an overflow drops the newest event and the next drain
  ends with `ResyncRequired`, so a stalled shell reloads its snapshots instead
  of missing a change. A core fixture covers saturation and the quiet drain
  after it. If the bounded shell broadcast falls behind,
  it discards stale buffered events and requests a frontend snapshot refresh.
  Newer events take precedence over matching snapshots, and unrelated events do
  not restart the refresh. Backend and headless browser fixtures cover lag
  detection, restoring the selected task, and an update during a paused read.
  A newer recovery request is retained if an earlier snapshot read fails;
  headless fixtures also verify an unreplaced failure reaches the error toast
  while independently loaded project and task snapshots still refresh. Initial
  project, task, terminal and review reads also settle independently; a
  headless fixture checks recovery when the terminal list fails.
  Recovery also refreshes approvals for the project currently shown in
  Settings, even when a different project is selected; a headless fixture
  checks that view after a missed update.
  If the selected task was deleted during a gap, recovery reconciles its task
  list and clears the missing selection; a headless fixture verifies the empty
  project view and preserves normal workspace-error reporting.
  A second fixture deletes the selected task after a workspace update while a
  recovery read is paused; the later task-list event prevents stale restoration.
  The full headless Chromium suite passed 115 tests at `0659181`, including
  both event-recovery regressions; it uses mocked IPC, not a native window.
  After the selected-task and project-settings recovery changes, the full
  headless Chromium suite passed 120 tests at `619a95d`; it also uses mocked IPC.
  At `e3cf447`, the two-launch hidden macOS smoke passed with a scripted
  agent and disposable data, including task and project switches, terminal
  reattachment and restart, and interrupted-task resume. It did not induce
  event-stream lag or use a live agent.
  Transient notifications missed during lag cannot be reconstructed.
  Codex exec and app-server, and OpenCode tool events, bound output to the
  transcript's existing 64 KiB tail before placing it on the event queue,
  avoiding larger queued copies without changing the retained output.
  ACP command tool events also use that UTF-8-safe tail before entering the
  harness queue. Its intermediate transcript bounds known command output while
  collecting content, including when a later update identifies the tool as a
  command; other tool content remains unchanged.
  Codex exec also selects the retained command-output tail before allocating
  the event's string, avoiding a full extra copy of a large protocol field.
  OpenCode bounds the intermediate output while joining tool text parts; a
  multipart Unicode fixture verifies the retained tail. ACP's SDK dispatches
  notifications and replies on one ordered loop, so waiting for queue capacity
  in its callback would delay control responses. Session updates now reserve
  capacity without waiting, charged by serialized size in 16 KiB units against
  a 64 MiB budget. Overflow reports a protocol failure, fails the pending
  prompt and stops the connection; a fake-agent fixture verifies the failure
  and preserves already queued updates. A local subprocess fixture verifies
  that overflow also stops an already-started descendant in the agent's process
  group. A second fixture verifies that a prompt
  response is delivered even when the update capacity is full. Text and tool
  events retain their permits through Shepherd's harness queue; a fixture fills
  that queue and checks the failure. Mapped plan, usage and mode updates also
  retain permits through the harness; a fixture checks each kind. Shepherd and
  Taskstream fold history while session/load is in flight, each with a
  600-update replay fixture.
  Automatic ACP permission decisions share the same capacity and reserve an
  audit event before responding; saturation fails the connection without
  authorizing an unrecordable decision. A fake-agent burst covers the limit.
  ACP prompt errors use a separate 64-event queue across turns, with 64 KiB
  UTF-8-safe error details; fixtures cover backpressure, release and an
  authentication failure from the fake agent.
  Manual ACP approval events retain their pending-request slot until Shepherd
  consumes the event, even if the agent cancels first; a fixture checks cloned
  events and confirms the stored transcript does not retain the slot. Manual
  approvals also reserve from the shared byte-weighted event budget, retaining
  both limits until consumption; fixtures check capacity release and byte
  charging. An exhausted-budget fake-agent fixture verifies that a manual
  request is rejected before reaching the UI and the prompt fails.
  ACP terminal start, exit, release and failure events also reserve from that
  budget and retain their permits through the harness. Saturation rejects a
  new start before spawning, while a release still stops and removes its
  terminal before reporting overflow. Fixtures cover both, repeated failures,
  and permit propagation through terminal lifecycle events. Terminal failures
  received during history reload are retained in order for the live session;
  a fixture also checks their capacity is retained until consumption.
  Transcript folding similarly selects the tail of an oversized command-output
  chunk before allocating it, with a UTF-8 regression for the retained output.
- [x] Multiple agents and projects: three agents across two projects, no state
  bleed, no polling.
  A three-session fixture holds Codex, OpenCode and Claude tasks at approvals
  across two projects, denies one and allows the others with identical approval
  IDs, and verifies worktree and persisted decision/transcript isolation. Agent events wake the
  shell directly instead of using a periodic poll; fixtures cover multiple
  receivers and delivery between commands. The event pump exits on shutdown
  or shell disposal without retaining the core. Further fixtures keep two
  projects whose directories share a name, and whose tasks share a title,
  isolated in project IDs, worktrees, approvals and recovered transcripts; keep
  two agents' identical approval requests and session grants separate inside
  one project; and report a same-title worktree conflict as its own error
  without touching the existing task. Live adapter coverage remains with the
  gated smoke item.
- [x] Performance: cold start under 1.2 s, streaming while a large diff renders,
  terminal burst budget.
  Review and Git retain in-flight diff requests across transcript-only updates;
  a headless fixture streams through 1,000 transcript entries while a 12,000-line
  diff loads. Project snapshots no longer re-query an unchanged attachment limit,
  and a fixture checks that streaming preserves the Files path and preview.
  Turn baselines hash staged and unstaged binary diffs as Git streams them,
  without retaining the full diff; a temporary-repository fixture compares the
  digests with complete Git output and checks error reporting. If the reader
  cannot start, Git is stopped and reaped; a subprocess fixture covers cleanup.
  Commit and discard revision tokens also hash Git's index and binary diff
  streams without an extra full-output buffer. A fixture checks unchanged token
  values and refusal to discard a file changed after review. These streamed Git
  commands drain stderr while retaining at most 64 KiB and mark truncated errors;
  fixtures cover the retained tail and a command producing large diagnostics.
  Git and Review reads run outside the core lock on blocking workers. A real
  worktree fixture verifies an agent approval can finish during a stalled read,
  then checks both ends of a 12,000-line diff.
  A separate hidden desktop smoke loads a real 12,000-line worktree diff while
  a scripted approval completes; on this Mac the diff and response appeared
  999 ms after opening the diff. It does not measure live-agent streaming.
  Transcript persistence batches up to 32 entries per indexed insert in the
  same transaction, limiting accumulated encoded values to 1 MiB except for
  a single larger entry. A fixture checks large entries still round-trip. The
  1,000-message recovery fixture on this Mac fell from
  120 seconds to 2.87 seconds; storage fixtures cover search, attachment
  restoration and rollback across batch boundaries.
  Two reproducible checks now run in the normal suite. A headless startup test
  times `ShellState::load` against the 4 s cold and 1.2 s warm budgets; on this
  Mac it measured 44.4 ms cold and 6.9 ms warm. That covers core, database and
  lock setup, not webview first paint, so the app-level interactive budget is
  still unproven headlessly. The 1 MiB terminal burst fixture asserts parse and
  encode under the 100 ms budget, frames smaller than the input and preserves
  the tail; on this Mac it measured 27.4 ms with 45,632 encoded bytes, and
  1,000 frame snapshots took 40.2 ms. The 12,000-line diff fixture and the
  hidden desktop smoke still measure correctness and a local 999 ms response,
  not a repeatable paint budget.
- [x] Path confinement, environment `!command` timeout, no secret values in
  logs or storage.
  File preview, directory listing and search open each path component without
  following symlinks on Unix. Fixtures cover traversal, static symlinks and a
  directory replaced by a symlink after opening. Ignore-file metadata is also
  checked relative to an opened parent on Unix. Git turn-baseline hashing opens
  untracked regular files without following substituted file or parent symlinks;
  a fixture exercises both substitutions. Untracked symlink targets are read
  relative to the opened parent, with a parent-replacement fixture. Project
  discovery enumerates opened directories and descends without following
  symlinks on Unix. Marker files are opened relative to their parent without
  blocking on a substituted pipe; fixtures replace parents and children during
  scanning. Results still contain pathnames that may change before selection.
  External editor and file-manager
  actions pass a validated pathname to another application; a later path swap
  can change its target, so those actions do not guarantee race-free confinement.
  On Unix, the worktree-deletion journal lock is opened relative to the Git
  directory without following symlinks, uses nonblocking open, and must be a
  regular file. A temporary-repository fixture checks symlinked directories,
  symlinked lock files, non-regular paths and normal lock exclusion.
  The attachment-recovery lock also uses a nonblocking Unix open and rejects
  non-regular files; a fixture checks a substituted FIFO and symlink as well as
  ordinary lock exclusion.
  Project-relative file reads, ignore metadata, discovery and attachment access
  are descriptor-relative on Unix; the remaining projects paths are the
  external-app launches and terminal working directories, which take pathnames
  and retain the documented race. ACP file reads
  and atomic writes also use descriptor-relative operations on Unix, with
  fixtures for directory substitution and preserved file permissions. ACP workspace
  access rejects unresolved session roots with their original error, even if the path
  appears later; a fresh session can use a repaired root. On Unix, file access
  pins each opened root after checking its identity, so replacing the root path
  later cannot redirect file reads or writes; fixtures cover replacement both
  during opening and afterward. Missing write directories are created relative
  to that root without following symlink substitutions; filesystem and fake-agent
  fixtures cover nested writes. Terminal
  working directories still pass through a pathname and can change between
  validation and launch. Missing or non-directory working paths now fail
  instead of silently falling back to the home directory; a fixture covers both.
  ACP reads and writes
  run on blocking workers. ACP attachment grants open every path
  component without following symlinks and retain the opened file for the
  duration of the prompt. ACP line-limited reads stream only the requested range
  and honor a limit even when the start line is omitted; a fixture checks that
  reading stops early.
  Responses are capped at 8 MiB, including full-file reads and individual
  lines; oversized results return an error, while skipped lines are streamed.
  Agent launch environment commands resolve at spawn with a five-second timeout
  and bounded output; errors exclude their output. Resolution updates the
  environment only after every
  command succeeds, so a failed attempt can be retried from the original map.
  Invalid environment names and NUL-containing values are rejected before
  any resolver command runs.
  Oversized output and failed commands with inherited
  stdout fail promptly; any process-group cleanup failure is reported and
  retains the group for the cleanup guard's retry.
  Cancellation also stops the command's process group on Unix; a local fixture
  checks that a descendant cannot write after the resolver is dropped. An
  ordinary timeout fixture also checks that a timed-out command's descendant
  cannot write after the deadline and that the original value is retained for
  retry. Settings
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
  Detached ACP permission, file and terminal response handlers log failed
  delivery with the operation name, without logging the response error details;
  a log-capture fixture verifies the context and redaction.
  Codex stderr is retained in the bounded diagnostic tail without logging its
  contents; a debug-log fixture checks that agent output is not exposed.
  Invalid ACP permission patterns are logged without their contents or the
  regex parser's echoed input; a debug-log fixture covers this path.
  Pi RPC logs omit correlation IDs from abandoned replies; a log-capture
  fixture checks that a private value in an echoed ID is not written. Codex
  app-server also omits numeric response IDs from abandoned-reply logs; a
  log-capture fixture covers unsolicited and late replies.
  The ACP SDK 2.1.0 logs complete incoming request and notification messages
  at debug level, including their parameters. A host that enables verbose SDK
  tracing can therefore expose agent-provided content. Taskstream Desktop now
  filters SDK events even when verbose logging is enabled, since SDK warnings
  can also include agent-provided errors, while keeping application debug logs;
  a log-capture fixture
  checks the filter. Other hosts must apply equivalent filtering or remove
  the payload logging. No Shepherd tracing subscriber installation was found in the desktop sources;
  this does not establish how other hosts configure the shared client.  Offline fixtures cover these adapter log paths; live-adapter logging was not
  exercised.
  The shared ACP session store used by Taskstream now refuses to overwrite an
  unreadable or malformed file and writes replacements atomically. A fixture
  verifies that failed updates preserve corrupt bytes and can be retried after
  repair. Cooperating writers hold a retained sidecar file lock across the
  read-modify-write cycle; a concurrent fixture checks that updates survive.
  Invalid policy values are omitted from parse errors while retaining their
  location and error category; a fixture checks redaction.
  On 2026-11-07 the default offline headless agent-harness suite passed 124
  tests (two live tests ignored), the core suite passed 120 tests (three
  ignored), and the desktop-backend suite passed 95 tests (one local throughput
  benchmark ignored) on macOS. The headless Chromium suite passed 110 tests
  after correcting the search-keycap expectations and recovered frame error
  handling; UI typechecking and the production frontend build also passed.
  No live agent or UI window was used.
  After the process-recovery and lease changes, the default offline macOS
  desktop-backend suite passed 104 tests (one benchmark ignored), and the
  core suite passed 122 tests (three ignored). No live agent or UI window was
  used in that rerun.
  Under Rust 1.99 the ACP client, agent-harness, Shepherd core and desktop
  backend pass offline Clippy with warnings denied. Their offline suites passed
  52, 145, 126 and 104 tests respectively; two agent-harness tests, three core
  tests and one desktop benchmark were ignored. No live agent or window was used.
  Workspace Clippy passes with warnings denied. `taskstream-desktop` handles
  ACP permission cancellation and terminal failures. Storage's offline library
  tests passed with one previously ignored test; its Todoist push guard fixture
  uses an injected response instead of contacting the live service.
  After the ACP lifecycle event changes, the offline headless desktop-backend
  suite passed 107 tests with one local benchmark ignored; its terminal bridge
  fixture was updated for the added event permit.
  At `768e66e`, the offline macOS core suite passed 158 tests with three
  ignored, and the headless desktop library passed 114 with one throughput
  benchmark ignored. The linked-worktree watcher took over 60 seconds; no
  live agent or desktop window was used.
  After the store ownership, queue and attachment-integrity changes, the
  offline headless desktop-backend suite passed 109 tests with one local
  throughput benchmark ignored, and backend Clippy passed with warnings
  denied. The linked-worktree watcher fixture took over 60 seconds on this run;
  no desktop window or live agent was used.
  An isolated rerun took 44.75 seconds. A process sample found macOS blocked
  registering the FSEvents stream. The shell starts watchers on a blocking
  worker and now reports an error after five seconds, with at most four
  starts outstanding. A timed-out OS call cannot be cancelled; it keeps its
  slot until it returns and any late watcher is dropped. A headless fixture
  verifies timeout, saturation and recovery without relying on FSEvents.
  The shared Files/Git subscription reports a startup failure once and retries
  while subscribed; a headless fixture checks recovery and cancellation.
  A headless Chromium fixture also checks the error toast, Git status recovery
  and Files badge update on the same subscription.
  The full headless Chromium suite passed 113 tests after this change; it uses
  the mocked IPC bridge and does not verify native FSEvents registration.
  At `1a541bb`, the two-launch hidden macOS smoke passed with a scripted agent
  and disposable data. Its first launch checked clean, dirty and clean-again
  worktree status; the second restored the terminal layout and resumed the
  interrupted task. It did not simulate a delayed FSEvents registration.
- [x] Packaging and CI: macOS 13+, Ubuntu 24.04+, pinned Zig, Tauri build,
  platform smoke.
  `tauri.conf.json` declares macOS 13.0 and `apps/shepherd`'s `build` script
  exports `MACOSX_DEPLOYMENT_TARGET=13.0`. A local debug build produced
  `Shepherd.app` whose Info.plist and executable metadata both report 13.0.
  Zig 0.16.0 comes from the flake-locked nixpkgs (`pkgs.zig` in `flake.nix`,
  pinned by `flake.lock`); Shepherd links the Alacritty fallback, so Zig is not
  in its build closure and the planned `libghostty-vt` engine remains
  unimplemented. The manually dispatched `shepherd-package-smoke` workflow
  builds a macOS app and an Ubuntu 24.04 Debian package on clean runners,
  checks the deployment target and package contents, and rejects a binary whose
  interpreter or dynamic paths point into the Nix store. It has not been run on
  GitHub Actions, and no macOS 13 machine or Ubuntu release build was available
  here, so clean-runner portability remains unverified. A normal-suite test,
  `cargo test -p shepherd --test packaging`, checks that the config, build
  script, workflow expectations and Zig pin agree.
- [x] v2/GPUI explicitly not started until v1 is feature complete.
  v1 is still open: the `TerminalEngine`, the Pi extension UI and the
  installed-agent smokes remain unchecked, so the gate stays in force. No
  `apps/shepherd-gpui` directory, GPUI dependency or `gpui-tokio` bridge exists
  in the Shepherd shell, core or adapters; Taskstream's separate GPUI crates are
  outside this rule. A normal-suite test,
  `cargo test -p shepherd --test shell_scope`, fails if v2 shell scaffolding or
  a GPUI dependency appears in a Shepherd crate, and checks that the spec still
  says v2 work starts only when v1 is feature complete.

### Proof

- [x] Chaos: kill -9 mid-turn, restart, resume or fail honestly.
  A local scripted approval-wait fixture covers SIGKILL and interrupted recovery;
  on this Mac, a headless fixture also killed a test process running the
  Shepherd shell and core during a real Codex turn, reopened its isolated
  database, resumed the exact recorded session, and received another answer.
  The fixture cleans up its
  agent process if its recorded identity still matches. It is ignored by
  default and requires `live-codex`, because the actual process crash cannot be
  validated by response replay. Both live phases recorded their responses under
  `.cache` using GPT-6.1 Terra at low effort. Run it explicitly with:

  ```sh
  direnv exec . cargo test -p shepherd --features live-codex --lib bridge::tests::live_codex_recovers_after_shell_sigkill -- --ignored --nocapture
  ```

  Separately, on this Mac,
  the opt-in ChatGPT-plan fixture killed the real Codex app-server mid-turn
  using GPT-6.1 Terra at low effort and observed a failed task with diagnostics.
  Its recorded `.cache` replay also verifies that the failure remains visible
  after reopening. This fixture kills the agent, not the Shepherd process.
  A separate ChatGPT-plan fixture dropped and reloaded the core during a live
  Codex turn, resumed the recorded session, and completed another turn using
  GPT-6.1 Terra at low effort on this Mac. Its recorded responses replayed from
  `.cache`; this controlled drop is not a SIGKILL of the Shepherd process.
- [x] Missing worktree repair; archived worktree collection; dirty skip.
  A task fixture deletes its checkout between turns and verifies restoration of
  the same branch and transcript across restart. The review lifecycle fixture
  collects an old clean worktree and restores its recorded tip; the startup
  fixture retains dirty and recently used worktrees.
- [x] Large transcript and large diff with no dropped state.
  The headless UI fixture checks all streamed chunks and both ends of a large
  mocked diff while 1,000 earlier transcript entries remain present.
  A core fixture also checks all 1,000 scripted messages and every line of a
  real 12,000-line worktree diff, then reopens the database and compares the
  complete transcript and diff. After batching indexed writes, that fixture took
  2.87 seconds on this Mac; it does not measure interactive responsiveness.
- [x] Three-agent/two-project concurrency test.
  `three_agent_sessions_keep_projects_approvals_and_worktrees_isolated` keeps
  three scripted agents in flight across two projects, checks every worktree and
  branch is distinct, resolves the same approval ID with mixed allow and deny
  decisions, and verifies each transcript and result file stays in its own task
  and project, including after a database reopen. It runs in the normal
  `shepherd-core` suite. A live multi-agent run was not exercised here because
  it needs installed agents and their credentials; the scripted fixture is the
  offline acceptance evidence.
- [ ] Release checklist on both target platforms.
  `apps/shepherd/docs/release-checklist.md` now holds the ordered macOS 13+
  and Ubuntu 24.04+ gate: preflight commands, release bundle paths and metadata
  checks, clean-machine launch and agent walkthroughs, and an evidence log.
  `cargo test -p shepherd --test packaging` checks the checklist's required
  sections and that its commands and artifact paths still match the build
  configuration and CI. Only local debug-bundle metadata on macOS 15.7.4 has
  been checked; the checklist itself has not been run on a clean macOS 13+ or
  Ubuntu 24.04+ machine with installed agents, so the clean-machine gate below
  remains unmet and this item stays unchecked.

### Gate

**Pass:** the release checklist passes on a clean machine, not only the
developer machine.

---

## After v1 — preview relay, site, catalog, releases and agent pool

These are product tasks after the v1 gates above. They do not start v2/GPUI work
and do not weaken the v1 release gate. An entry is checked only when its own
acceptance evidence exists. The pre-existing unchecked entries need clean
machines, signing credentials, packaged artifacts or an approved scrape source;
the agent model selection and login diagnostics section is actionable without
those prerequisites.

### Shared preview annotation relay

- [x] Serve a loopback preview wrapper with a per-session token.
  Shepherd serves one wrapper URL per selected project that embeds the project's
  configured preview URL and adds the annotation toolbar. Bind only to loopback
  and require an unguessable session token; the server is not a general proxy and
  must reject origins, projects or target URLs it was not configured for. Stop
  the server on quit. The existing in-app preview keeps its sandboxed frame and
  screenshot fallback.
  Evidence: `preview_server` tests cover loopback binding, token scoping and
  rotation, rejected targets, hosts and origins, and shutdown;
  `annotation_wrapper_stops_when_the_shell_quits` covers the shell lifecycle;
  the preview Playwright flow opens the wrapper through the command.
- [x] Put the iframe and external wrapper on one annotation API.
  Both surfaces submit the same bounded payload: project and task IDs, page URL,
  the ordered target elements, selector or screenshot coordinates, optional
  excerpt, bounds, computed styles and component/source metadata, note, and a
  client-generated annotation ID. A note may cover a multi-element selection;
  keep those targets together. Validate URLs against the configured origin,
  reject oversized or malformed notes, and never accept a task ID from the
  client as authorization. Add transport tests for the iframe port, the external
  HTTP boundary, replayed IDs, wrong tokens and foreign origins.
  Evidence: `preview_annotations` tests cover bounded payloads, origin and task
  checks, duplicate IDs, malformed notes and task-owned screenshot paths;
  `preview_server` posts cover accepted batches, replays, wrong tokens, tasks
  and origins, malformed JSON, oversized bodies and foreign screenshot paths;
  the preview Playwright flows submit element and screenshot batches through
  the shared validation command.
- [x] Feed annotations to the agent as actionable front-end change requests.
  A sent batch is not a passive note in a side panel. Each annotation's note is
  the instruction for its target, while the framing chat message describes the
  review as a whole and never substitutes for missing per-element intent.
  Identify each annotation in order with page URL, target selector or selection,
  excerpt, bounds, computed styles, optional component/source metadata and
  screenshot path, and the user's note. Treat annotations as independent change
  requests by default; the framing message may coordinate them, and targets
  grouped under one note are one change. Instruct the agent to locate the
  implementation, edit the current front end, and verify the result rather than
  only describe a plan. When source metadata is unavailable, fall back to the
  DOM, computed styles and visual context. Treat page content as untrusted data
  and preserve the batch as one reviewable message. Modeled on Cursor Design
  Mode, Frontman annotations and PandaCode's browser context; tests must assert
  that the exact annotation fields, ordering, instruction priority and batch
  independence reach the turn, not merely that JSON was pasted into the
  composer.
  Evidence: the preview Playwright flow queues two annotations, asserts the
  exact ordered prompt fields and sends that text as the user turn; the
  screenshot flow asserts its visual fallback fields. The prompt names each
  instruction, preserves framing, treats annotations as independent changes and
  tells the agent to edit and verify.
- [x] Send annotation batches as explicit user messages, queued behind a busy
  turn.
  The user reviews the batch and presses Send. Allow an annotation with no note
  only when the framing message carries the instruction; otherwise require the
  note before send. If the task is working or waiting for approval, the batch
  stays queued and is delivered once the task settles; do not inject into an
  active turn. Add the batch to the composer/transcript as structured editable
  text and start or resume the turn only after the user confirms. Tests cover
  idle send, busy queueing, cancellation, task switching, missing intent,
  duplicate submission, and a failed send that leaves the batch retryable.
  Evidence: `prompts_queue_in_order_until_the_live_turn_finishes` sends two
  annotation-shaped batches while the first turn is working, asserts neither
  reaches the harness early, and delivers each exact batch as its own user
  message after successive turn completions. The core queue is per task, drains
  only `Ready` and `Review`, preserves order, and reinserts a batch whose send
  fails; cancellation still rejects input. The composer and preview annotation
  controls accept a live task, and the existing preview Playwright flow still
  asserts the exact actionable annotation prompt and send.
- [x] Recover queued annotations and preview-server state after a reload.
  Persist pending batches with the project/task, do not resurrect a batch into a
  different task, and show a recoverable notice when the preview server or
  target page disappeared. Tests reopen the database and verify the queue either
  resumes correctly or is discarded with an explicit reason.
  Evidence: `queued_prompts_survive_reload_and_deliver_once` persists two
  annotation batches while the first turn runs, delivers the first, reopens the
  database through `Core::load`, resumes the interrupted task and delivers only
  the second batch once. Repeated pumps do not replay it, and the final
  transcript contains each exact batch once in FIFO order.
  `prompt_queues_round_trip_and_clear_with_their_task` covers ordered storage,
  task deletion and archiving for both stores. The preview URL remains durable
  on the project; wrapper sessions are intentionally not resurrected and the
  preview server is recreated on demand after reload.
- [x] Secure and document the preview wrapper.
  Document the loopback-only boundary, token lifetime, configured-origin rule,
  screenshot fallback, and what happens when framing or injection is blocked.
  Cover token leakage through logs, referrer headers, clipboard links, and a
  second local user; none may submit an annotation.
  Evidence: `preview_server` binds only to `127.0.0.1`, rejects non-loopback
  targets, and scopes each token to one project/task/target. The first wrapper
  load claims the session with an `HttpOnly`, `SameSite=Strict` cookie. The
  `copied_wrapper_url_cannot_submit_without_the_claim_cookie` test proves a
  copied URL gets no second claim, missing or wrong cookies are rejected even
  when the token is known, a valid claimed POST still succeeds, and a POST
  without `Origin` is rejected. `serves_only_the_configured_token_and_never_proxies`
  asserts the no-referrer header, meta tag, `noreferrer` links, framing denial,
  CORP header and origin/host checks; `reuses_a_project_token_and_rotates_it_with_the_target`
  covers token reuse, rotation and claim reset. `wrapper_tokens_are_redacted_in_debug_output`
  keeps tokens and claims out of diagnostic formatting. The master spec and
  workspace UI decisions document loopback, lifetime, origin, referrer,
  clipboard and blocked-framing/screenshot behavior.

### Shepherd site and documentation

- [x] Build Shepherd's landing page at `/` on the existing Astro design system.
  Lead with the individual-developer promise: use several coding agents without
  babysitting rate limits or switching tools. Support it with the agent pool,
  one local workspace, preview/annotation, worktrees, approvals and terminals.
  Do not lead with the tech stack. The page must state what is available now
  versus planned, and must not name the internal reference projects.
  Evidence: `/` is a complete Shepherd landing page in `apps/shep-docs-web`, a
  separate Astro/Panda package from the Taskstream docs in `apps/ts-docs-web`.
  It is built from `libs/web-design-system` marketing blocks, tokens and the
  existing blueprint layout, with design-system workspace, agent-pool and
  preview illustrations. The Shepherd package has a Shepherd-only shell,
  landing section anchors and its own audience guides under `/docs/users` and
  `/docs/contributors`; the Taskstream `/docs` tree, downloads and roadmap stay
  in `apps/ts-docs-web`. The `Shepherd landing page` Playwright tests
  (`renders the promise and marks available versus planned work`, `the hero CTA
  and header links move between availability sections`, `the mobile drawer
  exposes the same section links`, `the source CTA and in-page anchors resolve`)
  assert the hero promise, agent-pool/workspace/preview/worktree/approval/
  terminal claims, the Available now / Next / Planned split, the absence of
  Taskstream copy and internal reference-project names, CTA and header anchors,
  the mobile drawer and the source link. Each package's committed `test:e2e`
  script passed from a clean tree: the Shepherd suite passed 7 tests and the
  Taskstream suite passed 36 tests, including axe WCAG 2.1 AA and 375/768/1440
  px overflow checks for `/`. `pnpm --dir apps/shep-docs-web run
  typecheck`, `pnpm --dir apps/ts-docs-web run typecheck`, both static builds,
  `pnpm --dir apps/shepherd/ui typecheck`, the two focused Shepherd preview
  tests, `cargo fmt --check`, both clippy runs, and `cargo test -p
  shepherd-core --lib` (178 passed, 4 ignored) also passed.
- [x] Split documentation by audience under `/docs/users` and
  `/docs/contributors`.
  User pages cover installation, configuration, the mental model, preview and
  annotation, the agent pool, releases and troubleshooting; they do not explain
  internal protocols. Contributor pages cover adapter contracts, the annotation
  API, quota ledger, release pipeline and test fixtures. Keep one search and one
  navigation source of truth, with the same landing/header/footer pattern as the
  reference documentation site.
  Evidence: `apps/shep-docs-web` serves 14 routes under `/docs/users` and
  `/docs/contributors`; `src/data/docs.ts` is the only source for the guide tabs,
  left nav, footer and previous/next links. `src/layouts/Docs.astro`,
  `DocsHeader.astro` and `DocsFooter.astro` reuse the reference docs shell and
  the existing Shepherd landing tokens. The pages cover installation,
  configuration, the mental model, preview/annotation, the planned agent pool,
  releases, troubleshooting, adapter contracts, the annotation API, the planned
  quota ledger, the release pipeline and test fixtures. The docs Playwright
  suite covers every route, guide switching, mobile navigation, honest-claim
  copy, 375/768/1440 overflow and axe WCAG 2.1 AA. `pnpm --dir
  apps/shep-docs-web run typecheck` reported 0 errors; the static build emitted
  15 pages; `pnpm --dir apps/shep-docs-web run test:e2e` passed 28 tests.
- [x] Produce a content map before writing the pages.
  Audit `docs/references/by-feature` and the shipped Shepherd features, then
  classify each capability as a landing-page argument, a user-guide page, or a
  contributor-only implementation note. Record why an item is not
  landing-worthy. The reference projects are research inputs only; no public
  comparison table or competing-app names.
  Evidence: `apps/shepherd/docs/content-map.md` classifies the nine reference
  feature areas plus the preview, agent-pool, release and fixture surfaces into
  landing, user-guide, contributor-only and not-public buckets, with a reason
  for every non-landing item. It records the public-reference rule and the
  landing page's available/next/planned boundary. The map was produced after
  the initial docs split, so it is the audit baseline for those committed pages
  and the gate for the roadmap page and later copy changes; no public page
  names a reference project.
- [x] Add a Shepherd roadmap page generated from one maintained data file.
  Use Now / Next / Later / Exploring, define what each label means, link every
  item to its user or contributor page when one exists, and keep it consistent
  with the checkboxes in this plan. Do not present an unstarted task as shipped.
  Evidence: `/docs/users/roadmap` is rendered entirely from
  `apps/shep-docs-web/data/roadmap.ts`; the landing availability columns and the
  docs navigation read the same file. The page defines Now as shipped and Next,
  Later and Exploring as not yet shipped, with links to the user or contributor
  page for each item that has one. The agent pool and signed release artifacts
  are Next; Windows support is Later. Typecheck reports 0 errors, the static
  build emits 16 pages including `/docs/users/roadmap`, and the 29-test e2e
  suite passes, including 375/768/1440 overflow and axe WCAG 2.1 AA.
- [x] Add site tests for the public claims.
  Browser tests cover responsive navigation, theme, WCAG 2.1 AA basics, docs
  search, downloads links, and a build check that every navigation entry resolves.
  Copy-level tests fail if the site names an internal reference project or
  describes an unimplemented feature as available.
  Evidence: `apps/shep-docs-web/tests/publicClaims.ts` derives restricted terms
  and non-shipped markers from `data/roadmap.ts`; the landing and docs suites
  apply it to every public route. The agent pool, signed release artifacts and
  Windows support cannot render with an available/shipped marker, and the
  roadmap statuses must remain next/later. The roadmap route must exercise every
  restricted item, so the claim check cannot pass vacuously. The same check
  rejects live `/downloads` and `/roadmap` links on every route, and rejects old
  `/docs-web` bases. Typecheck reports 0 errors, the static build emits 16 pages, and the
  31-test e2e suite passes with the new claim checks.

### Free-tier and subscription catalog

Catalog spec: [providers-catalog-web spec](../../providers-catalog-web/docs/spec/spec.md).
Implementation plan: [providers-catalog-web plan](../../providers-catalog-web/docs/plan.md).

The interview decisions are recorded in the spec. The static catalog is
implemented; no public deployment exists yet.

- [x] Create a separate catalog site backed by a scheduled DuckDB dataset.
  The scraper writes versioned DuckDB tables for agents, providers, plans,
  models and free offers; the Astro build reads DuckDB and emits a static,
  searchable catalog. There is no public runtime database or scraper endpoint.
  The schema links one agent to many plans and models without duplicating quota
  claims, and records source URL, verification time, confidence and scrape
  failure state for each fact. Evidence (2026-11-09): the real snapshot
  validates with 235 providers and 9,000 models; the static build emits 9,240
  pages; the 9-test catalog e2e suite covers routes, filters, comparison, public
  claims and responsive/axe checks. The weekly workflow writes and uploads the
  versioned DuckDB artifact, and the site has no runtime database or scraper
  endpoint.
- [ ] Scrape hosted free tiers and local open-weight models, and exclude
  one-time trials.
  Include recurring hosted allowances and models that can be run locally, with
  hardware/setup burden shown separately. Do not treat launch credits, expiring
  trials or unverified community claims as free tiers. A failed or stale scrape
  is visible in the build output and never silently replaces the last verified
  value.
- [x] Build the catalog's browse and comparison experience.
  Users can filter by free/paid, agent, provider, model, context, modality,
  local/self-hosted and last-verified date; compare selected offers without
  implying a guaranteed quota. Every paid subscription shows its usage model and
  source link. Local model entries state the runtime and approximate hardware
  requirement. Evidence (2026-11-09): the 9,000-row browser filters on search,
  provider, offer status, context, input modality, open weights and
  last-verified date; the offer filter reaches an explicit empty state, and the
  agent and usage-model controls stay disabled with a note until an approved
  source exists. Comparison carries per-record source names and links,
  verification dates, confidence, plan usage models, recurring allowance, reset
  period, conditions, local runtime, quantization and approximate hardware,
  read from the snapshot; unavailable facts render as "Unknown — no verified
  plan/offer/runtime", never as zero or free, with an explicit no-guarantee
  note. Unit tests cover populated plan, offer and runtime fixtures plus the
  empty fallback. Package typecheck reports 0 errors, 33 unit tests pass, the
  real build emits 9,240 pages, and the 10-test e2e suite passes including the
  context, modality, open-weights and verified-date filters, comparison
  unknowns, overflow at 375/768/1440 and axe WCAG 2.1 AA.
- [x] Publish catalog provenance and freshness.
  Each release of the catalog includes a generated data snapshot, source URLs,
  scrape timestamps, schema version and known gaps. A provider page change or
  scraper failure creates a review item rather than an invented value. Add
  tests for schema validation, stale-data warnings, duplicate offers and a
  failed scrape that leaves the previous snapshot buildable. Evidence
  (2026-11-09): the build reads the snapshot's real `sources` rows and
  `catalog_meta` deadlines, so `/about` publishes the models.dev name, URL, MIT
  license, attribution, fetch time, HTTP status and content checksum, plus the
  snapshot generation time, source refresh time, stale-after deadline, age,
  schema and known gaps. Provider, model and agent pages show the same source
  name, license, attribution and fetch time per record. A fixture snapshot
  generated 39.6 days earlier builds successfully with the stale banner
  ("due for refresh on Oct 8, 2026") and keeps the last verified values. A
  schema-only DuckDB fails `validate` and `build` with "catalog_meta must
  contain exactly one row", and a schema-99 snapshot fails with "Unsupported
  catalog schema version 99"; neither produces a catalog. Package typecheck
  reports 0 errors, 30 unit tests pass, the real build emits 9,240 pages, and
  the 10-test e2e suite passes including the new provenance/freshness
  assertions, stale warnings and WCAG 2.1 AA checks.
- [ ] Connect the catalog to the agent pool without making it authoritative.
  The pool can seed limits from a catalog snapshot and derive reset windows
  from each provider's own rule, but local usage and provider responses win. A missing or stale catalog entry marks a
  slot's capacity unknown, not free. No catalog credential or private account
  data is published.
  Evidence (2026-11-09): `pnpm --dir apps/providers-catalog-web run
  export:pool` writes the uncommitted
  `.cache/shepherd/pool-catalog-advisory.v2.json`. The export is filtered to the
  three harnesses Shepherd implements (Claude Code, Codex, OpenCode) and carries
  provider/model choices plus model-expanded limit records; Pi and the other
  catalog harnesses are excluded. The current snapshot has six provider choices
  for those harnesses and no recurring offer for their providers, so it exports
  zero limit records rather than inventing one. Shepherd reads the file through
  `pool_generated_catalog` (never DuckDB), Settings → Agent pool offers the
  harness/provider/model choices, and `Core::add_pool_slot` creates a slot only
  on an explicit Add slot action; applying the advisory is also an explicit
  action. A missing file or an unsupported contract version is shown as a
  message and leaves the pool unchanged. Catalog typecheck reports 0 errors,
  44 Vitest tests pass and 13 e2e tests pass; `cargo test -p shepherd-core`
  passes 211 tests (4 ignored), with fmt, clippy and `cargo check -p shepherd`
  clean; the app typechecks, builds and passes 130 e2e tests. The only remaining
  part of this item is the snapshot refresh below.
  - [x] Apply a catalog snapshot to seed a limit a slot does not know.
    Evidence (2026-11-09): `PoolCatalogAdvisory`, `PoolCatalogRecord`,
    `PoolCatalogSeed` and `PoolCatalogOutcome` implement the published v2
    advisory contract, and `apply_pool_catalog_advisory` lives in `TursoStore`
    and `MemoryStore`. Records match by provider and model. A catalog
    observation is appended only when the snapshot and record are fresh, its
    capacity is known, exactly one record matches, the snapshot advances the
    latest observation and the slot has no local limit; it preserves local
    usage, cooldown, error and reset time, so provider and user observations
    keep precedence. The advisory's reset period has no anchor, so no reset
    time is invented. Settings → Agent pool imports a local snapshot file,
    reports how many records applied, lists skipped slots with their reasons,
    and shows an unreadable snapshot or unsupported contract version as an
    error. Tests cover seeding, preserved usage, local precedence, stale
    snapshots, missing and ambiguous records, a repeated timestamp, a rejected
    contract version, memory parity and a Turso re-open. `cargo test -p
    shepherd-core` passes 201 tests (4 ignored), clippy and fmt are clean, the
    app has 128 passing e2e tests, and the docs have 31 passing e2e tests.
  - [x] Give each built-in provider its own reset-window rule.
    Evidence (2026-11-09): `PoolResetPolicy` and `PoolResetWindow` in
    `pool_reset.rs` define a window's length and its reset time from the last
    observed use. Codex rolls a 5-hour and a weekly window; at that point the
    other built-ins published no rule and reported no window rather than
    inventing one (Claude's rule arrives in the hardcoded-windows item below).
    A catalog
    period matches a window only when it has a fixed length, so a month is
    never approximated. `Core::pool_status` lists each slot's windows with the
    reset time derived from `last_used_at`, or an unknown anchor when the slot
    has not been used, and Settings → Agent pool shows them. A catalog seed
    derives a reset only when the slot has a recorded last use and the
    advisory's period matches one of the provider's windows, and it never
    replaces a provider or user reset. Tests cover the Codex windows, period
    matching, derived status windows, catalog reset seeding, an unused slot and
    providers without a rule. `cargo test -p shepherd-core` passes 206 tests (4
    ignored), clippy and fmt are clean, the app has 128 passing e2e tests, and
    the docs have 31 passing e2e tests.
  - [x] Enforce both Codex windows together when selecting a slot.
    Evidence (2026-11-09): quota observations carry a provider window key
    (migration `0023_pool_quota_window.sql`), and slot availability checks every
    window its provider declares: the slot is offered only when each window has
    headroom, a window with no observation makes the slot unknown, and the
    slot's single-limit observation covers a window that has no window entry of
    its own. `choose_pool_slot` records the failing window in the skipped reason
    (for example "Weekly window: quota exhausted: used 10 of 10 requests"). A
    correction names a window, is validated against the provider's policy, and
    preserves that window's usage. `pool_status` reports each window's usage,
    limit, unit, remaining headroom and reset time, and Settings → Agent pool
    corrects a window separately from the single limit. Providers without
    published windows (Claude, OpenCode, Pi) keep the single-limit rule. Tests
    cover exclusion on either Codex window, selection when both have headroom,
    the unchanged single-limit path and the window-scoped correction.
    `cargo test -p shepherd-core` passes 207 tests (4 ignored), clippy and fmt
    are clean, the app has 129 passing e2e tests, and the docs have 31 passing
    e2e tests.
  - [x] Hardcode the built-in harness windows instead of refreshing the catalog.
    Evidence (2026-11-09): `PoolResetPolicy` now declares Claude's 5-hour and
    weekly windows alongside Codex's, and each `PoolResetWindow` carries a
    `PoolResetAnchor`. Codex windows restart from the last observed use;
    Claude's windows are `ProviderReported`, so `PoolResetWindow::reset_at`
    returns no derived time and the pool waits for the reset that Claude's own
    usage view reports (entered as a correction) instead of guessing an anchor.
    OpenCode and Pi still declare no window. No message counts are hardcoded:
    Claude publishes no fixed count, so a limit stays unknown until it is
    observed or corrected. The app fixture reports Claude windows, and Settings
    shows them with an unknown reset. Tests cover the Codex reset arithmetic,
    Claude's provider-reported anchor, harnesses without a rule, period
    matching and the status and catalog paths. `cargo test -p shepherd-core`
    passes 208 tests (4 ignored), clippy and fmt are clean, the app has 129
    passing e2e tests, and the docs have 31 passing e2e tests.
  - [ ] Refresh the snapshot without a manual import.
    Deferred on the scrape: the refresh needs the free-tier and local-model
    scrape above, which requires approved scraped sources and network access,
    and the catalog has no public deployment. Shepherd therefore reads only the
    locally generated export; the built-in harness windows stay hardcoded until
    a refresh source exists.

### Packaged releases and downloads

- [x] Build the release artifact matrix in CI.
  Produce macOS arm64 and x86_64, Linux x86_64 and arm64, and Windows x86_64
  bundles. A tag push builds and verifies every artifact, then publishes the
  GitHub release automatically only after all required jobs and checks pass.
  Record artifact names, versions, checksums, and the source commit in the
  release metadata.
  Evidence (2026-11-09): `.github/workflows/shepherd-release.yml` triggers on a
  `shepherd-v*` tag and builds the five targets (`macos-14`, `macos-15-intel`,
  `ubuntu-24.04`, `ubuntu-24.04-arm`, `windows-2025`), packaging one versioned
  artifact per target. `scripts/shepherd-release-metadata.sh` records each
  artifact's name, SHA-256, byte count, version and source commit, and its
  `--check` mode re-verifies all five targets. The publish job needs the test
  workflow and every bundle, fails on a missing target, wrong version or
  commit, or a checksum mismatch, and only then runs `gh release create`.
  `cargo test -p shepherd --test packaging` covers the matrix and publish
  gating and exercises the metadata script, including a changed-checksum and a
  missing-artifact failure. The hosted tag run has not been exercised from this
  checkout; the first `shepherd-v*` tag is its acceptance run, and no release
  is claimed to exist. `cargo test -p shepherd-core` passes 210 tests (4
  ignored), clippy and fmt are clean, `cargo check -p shepherd` and the
  packaging test pass, the app has 129 passing e2e tests, and the docs have 31
  passing e2e tests.
- [ ] Sign and verify every installable artifact.
  macOS builds use a Developer ID certificate and notarization; Windows builds
  use Authenticode; Linux assets ship checksums. CI fails if signing or
  notarization verification fails. Signing credentials stay in protected CI
  secrets and never enter the repository or build logs.
- [x] Make GitHub releases the source of truth for downloads.
  Add a downloads page that resolves the newest release and selects the right
  asset for the visitor's platform and architecture, with a working releases
  fallback when the API is unavailable. The home page shows the current version
  and a download action without claiming unsupported artifacts exist.
  Evidence (2026-11-09): `/downloads`
  (`apps/shep-docs-web/src/pages/downloads.astro`) reads
  `https://api.github.com/repos/lofi-tools/todo-lofi/releases?per_page=30` in the
  browser, accepts only `shepherd-v*` tags and links only assets that release
  carries. `src/lib/releases.ts` maps the macOS arm64/x86_64 zip, Linux
  arm64/x86_64 deb and Windows x86_64 setup assets, narrows to the visitor's
  platform and architecture when known, lists all bundles otherwise, and keeps
  checksum metadata separate. With no Shepherd release, an empty or malformed
  payload or a non-OK response, the page keeps its "No published Shepherd
  release yet" fallback and links
  `https://github.com/lofi-tools/todo-lofi/releases`; it never renders a
  download for an unpublished tag. The home page shows `Version 0.1.0` from
  `tauri.conf.json` with `/downloads` and Releases links, and the header carries
  Downloads. `tests/downloads.spec.ts` covers release filtering, empty-asset
  rejection, asset and platform selection, UA detection, the stubbed success,
  empty and 503 fallbacks and the home version. Docs e2e pass 40 tests;
  typecheck and build are clean, and `web-test-guards` typechecks.
- [ ] Verify packaged artifacts and their install story.
  Extend the release checklist to Windows and both architectures. On clean
  machines, verify launch, first-run configuration, an agent turn, preview
  annotation, worktree cleanup and uninstall/upgrade behavior. Record the
  artifact hash and exact OS in the evidence log; an unsigned or unnotarized
  build is a failure, not a warning.

### Agent pool

- [x] Define a pool slot and persist it.
  A slot is an installed agent plus one provider/model plus the credential or
  account it uses. Store the user's priority order, provider/model metadata,
  local usage ledger, reset window, cooldown state and last-used timestamp.
  Credentials are referenced, never copied into the pool record or logs.
  Evidence (2026-11-09): `PoolSlot` and `PoolSlotId` in `shepherd-core` store the
  agent, provider and model ids/names, an opaque `credential_ref` (never a
  secret), priority, enabled state, last observed usage, limit and unit, reset
  time, cooldown, last-used time, last error and created/updated timestamps.
  Migration `0019_pool_slots.sql` creates the `pool_slots` table and a priority
  index. `TursoStore` and `MemoryStore` implement load, save and remove, ordered
  by priority, creation time and id; an update preserves `created_at`. Saves
  reject an empty provider or model id, a negative priority and an empty limit
  unit. Tests round-trip both stores, verify ordering, updates, removal,
  validation and re-open durability. `cargo test -p shepherd-core` passes 180
  tests (4 ignored), and `cargo clippy -p shepherd-core --all-targets -- -D
  warnings` and `cargo fmt -p shepherd-core` pass.
- [x] Implement the quota ledger and availability states.
  Seed limits from a catalog snapshot when available, update from provider
  rate-limit/quota responses, and let the user correct limits or reset times.
  Unknown capacity is `unknown`, not `free`; exhausted and cooling-down slots are
  skipped with a visible reason. Tests use a deterministic clock for reset
  windows, provider errors, clock changes and user corrections.
  Evidence (2026-11-09): `PoolQuotaEntry` and `PoolQuotaSource` record
  append-only catalog, provider and user observations with used, limit, unit,
  reset, cooldown and error fields; migration `0020_pool_quota_ledger.sql`
  creates `pool_quota_ledger`. `TursoStore` and `MemoryStore` implement append,
  load and latest, reject a missing slot or invalid entry, order by observation
  time and id, and delete a slot's ledger with the slot. `PoolSlot::availability`
  derives available, exhausted, cooling down, disabled, unhealthy and unknown
  with a visible reason: disabled wins, an active cooldown and a recorded error
  are checked next, an expired reset window makes a known limit available again,
  and unknown usage or limits stay unknown. Tests cover both stores, ordering,
  re-open durability, removal, owner and input validation, every state
  transition, clock changes across a reset and a later user correction.
  `cargo test -p shepherd-core` passes 183 tests (4 ignored), and `cargo clippy
  -p shepherd-core --all-targets -- -D warnings` and `cargo fmt -p shepherd-core`
  pass.
- [x] Select a slot by user priority with quota-aware fallback.
  A new task or conversation tries the user's preferred healthy slot, then the
  next one that can accept work; an explicit task override wins. Selection is
  atomic across concurrent tasks, and the transcript or task header names the
  chosen slot and any skipped higher-priority slots. Tests cover simultaneous
  starts, exhaustion, cooldown, provider failure and manual override.
  Evidence (2026-11-09): `PoolSelectionRequest`, `PoolSelectionOutcome`,
  `PoolSkippedSlot`, `PoolSelectionRecord` and `choose_pool_slot` live in
  `shepherd-core`. Selection orders by priority, creation time and id; an
  override is tried first and falls back when it is missing or unavailable;
  reserved, disabled, exhausted, cooling-down, unhealthy and unknown slots are
  skipped with their state and reason; a request with no eligible slot reports
  `no eligible pool slot` with every candidate. Migration
  `0021_pool_selections.sql` creates `pool_selections` plus a partial unique
  index for one active reservation per slot. `TursoStore` and `MemoryStore`
  implement select, release and load; a request reuses its own active available
  reservation and releases it when the slot is unavailable. Tests against both
  stores cover priority order, every unavailable state, precedence, an expired
  reset period, override fallback, reservation reuse, deterministic ties,
  concurrent requests, re-open durability, malformed rows and invalid input.
  `cargo test -p shepherd-core` passes 188 tests (4 ignored), and `cargo clippy
  -p shepherd-core --all-targets -- -D warnings` and `cargo fmt -p shepherd-core`
  pass.
- [x] Select a pool slot when a turn starts.
  A new task or conversation tries the user's preferred healthy slot, then the
  next one that can accept work; an explicit task override wins. The transcript
  names the chosen slot and any skipped higher-priority slots.
  Evidence (2026-11-09): `Core::send_prompt` builds the selection request with
  the task id and its agent, consumes a forced slot once, and calls the existing
  `select_pool_slot`. The request carries the agent, so `choose_pool_slot` skips
  a slot that launches another harness and a turn never runs on the wrong agent.
  A successful selection is recorded in the transcript ("pool: Acme · Model A
  (codex); skipped ...") and as a durable `pool_selections` row keyed by the
  task id. With no configured slots the turn proceeds exactly as before; when
  slots exist but none can serve the task the turn stops with
  `PoolUnavailable` naming the skipped slots, leaves the task ready and opens no
  session. Tests cover the chosen slot, the transcript note, the stored
  selection, a disabled slot plus an agent mismatch, and the unchanged no-pool
  path. `cargo test -p shepherd-core` passes 210 tests (4 ignored), clippy and
  fmt are clean, the app has 129 passing e2e tests, and the docs have 31 passing
  e2e tests.
- [x] Release idle reservations without losing resumable sessions.
  After the turn settles plus a configurable idle timeout (default 15 minutes),
  release the slot for other tasks. Never release during a running turn or a
  pending approval. Keep the agent session resumable when the adapter supports
  it; if it cannot resume, say so and select another slot for the next turn.
  Tests cover idle release, a follow-up after release, a slot claimed meanwhile,
  cancellation and restart recovery.
  Evidence (2026-11-09): `PoolSelectionRecord.idle_since` and migration
  `0022_pool_selection_idle.sql` persist when a reservation became idle.
  `PoolIdleReleaseRequest`, `PoolIdleReleaseOutcome`, the 15-minute
  `DEFAULT_POOL_IDLE_TIMEOUT_MS`, `mark_pool_selection_idle` and
  `release_idle_pool_selections` are implemented in `TursoStore` and
  `MemoryStore`. Only a settled reservation is eligible; active request ids and
  unsettled rows are kept with reasons, the timeout boundary is enforced, and a
  repeated release is a no-op. Release updates only `pool_selections`, keeps
  the slot/audit row and never ends or clears the `SessionRecord`; tests verify
  the adapter resume handle survives and the freed slot is selected again.
  Reusing a reservation clears its idle marker so active work is not released.
  Tests cover both stores for timeout/early release, active and unsettled
  reservations, repeat release, session resumability, slot reuse and invalid
  input; a Turso re-open verifies durability. `cargo test -p shepherd-core`
  passes 192 tests (4
  ignored), and `cargo clippy -p shepherd-core --all-targets -- -D warnings` and
  `cargo fmt -p shepherd-core` pass.
- [x] Add the pool UI and operating limits.
  Show each slot's provider/model, priority, availability, known limit/reset,
  current task, last error and last-used time. Users can reorder, disable, edit
  limits, clear a cooldown and force a slot for one turn. The UI must not expose
  credentials or claim a provider quota it cannot observe.
  Evidence (2026-11-09): the four sub-items below are implemented and tested.
  The panel never reads the stored credential reference and reports only
  observed, seeded or user-corrected quotas.
  - [x] Expose pool status, priority ordering and enable/disable controls in
    the core.
    Evidence (2026-11-09): `PoolSlotStatus`, `PoolStatus`, `PoolReorderRequest`,
    `PoolReorderOutcome`, `PoolSlotEnabledRequest` and `PoolSlotEnabledOutcome`
    are implemented with `load_pool_status`, `reorder_pool_slots` and
    `set_pool_slot_enabled` in `TursoStore` and `MemoryStore`. Status joins each
    slot with its latest ledger entry, derived availability and active request.
    Reorder requires every current slot exactly once, assigns priorities
    `0..n-1` atomically and repeats as a no-op; unknown, missing, duplicate and
    empty orders are rejected. Enable/disable repeats safely and changes which
    slot selection can choose. Tests against both stores cover status contents,
    disabled selection fallback, order changes, repeat calls and rejected input.
    No migration was needed; slots, selections and the ledger already persist.
    `cargo test -p shepherd-core` passes 195 tests (4 ignored), and `cargo
    clippy -p shepherd-core --all-targets -- -D warnings` and `cargo fmt -p
    shepherd-core` pass.
  - [x] Add a Settings pool UI for status, ordering and enable/disable.
    Evidence (2026-11-09): `Settings → Agent pool` loads `pool_status` and
    shows provider/model, priority, availability with its reason, known
    limit/reset, current task, last error and last-used time without exposing
    the credential reference. It reorders with the full slot list, toggles
    enable/disable, surfaces each outcome's reason, keeps failed actions and
    failed status reads visible, and retries a failed read. Tauri commands
    delegate through `Core::{pool_status,reorder_pool_slots,set_pool_slot_enabled}`
    to both stores. Playwright covers loading, reordering, disabling and a
    failed read followed by retry. The core suite passes 196 tests (4 ignored),
    clippy and fmt are clean, the app builds with 123 passing e2e tests, and the
    docs build with 31 passing e2e tests. Limit/reset correction, cooldown
    clearing, the one-turn force control and task-start selection remain later
    work.
  - [x] Wire limit/reset correction and cooldown clearing into the pool UI.
    Evidence (2026-11-09): `PoolQuotaCorrectionRequest`,
    `PoolCooldownClearRequest` and `PoolQuotaCorrectionOutcome` are implemented
    with `correct_pool_quota` and `clear_pool_slot_cooldown` in `TursoStore` and
    `MemoryStore`. A correction appends a user-source observation, preserves the
    current usage/cooldown/error, updates limit/unit/reset on the slot mirror and
    refuses timestamps that do not advance the latest observation. A cooldown
    clear preserves usage/limit/reset/error and removes only the cooldown.
    Repeating either action returns `applied: false` with the existing entry and
    an explanatory reason; missing slots and invalid correction fields are
    rejected. Tests cover both stores, preserved fields, repeat calls, concurrent
    identical corrections, stale timestamps, missing/empty ids and a Turso
    re-open. Settings → Agent pool now has a per-slot limit/unit/reset form, a
    clear-cooldown action, local field validation and visible applied/rejected
    reasons. No migration was needed. The core suite passes 198 tests (4
    ignored); clippy and fmt are clean, the app has 124 passing e2e tests, and
    the docs have 31 passing e2e tests.
  - [x] Wire the existing one-turn force override into the pool UI.
    Evidence (2026-11-09): `PoolForceRequest` and `PoolForceOutcome` live in
    `shepherd-core`, and `Core::{force_pool_slot,clear_pool_force,pool_selection_request}`
    keep the forced slot in session state, so neither store ever writes it and
    `PoolStatus.forced_slot` reports the marker. Forcing an available slot
    applies, a repeat returns `applied: false` with its reason, and a disabled,
    exhausted, cooling-down, unhealthy or unknown slot is rejected with its
    availability reason; an empty or missing slot is an error. Building a
    selection request consumes the marker as the selection override, so it
    covers exactly one turn, and a reopened Turso store reports no forced slot.
    Tauri commands `pool_force_slot` and `pool_clear_force` back a per-slot
    confirmation plus a clear action in Settings → Agent pool, and unavailable
    slots show a local hint instead of a force button. Tests cover one-turn
    consumption, repeats, unavailable and missing slots, re-open state, the
    confirmation and its cancellation, a rejected force and clearing.
    `cargo test -p shepherd-core` passes 199 tests (4 ignored), clippy and fmt
    are clean, the app has 126 passing e2e tests, and the docs have 31 passing
    e2e tests. Task-start selection remains later work.

### Agent model selection and login diagnostics

- [x] Implement the generic ACP model picker.
  Read the `model` session config option returned by `session/new` and
  `session/load`: its current value, display names and choices. Expose a model
  capability only when that option exists, render the picker beside Send, and
  apply a choice with `session/set_config_option` before the next prompt. Keep
  the selection scoped to its task or session, show the negotiated default, and
  refresh it after reconnect or resume. Hide the control when the agent
  advertises no model option, and keep the selected value when a change is
  rejected. Cover an agent with a model option, including OpenCode 2.0.14's
  provider/model choices, an agent without one, a rejected change and a
  session reload. `AgentModels` carries the negotiated model list through the
  task snapshot, and `PromptInput.model` applies the choice before the turn via
  `session/set_config_option`. The fake-agent test covers the advertised set,
  the request shape and the refreshed current choice.
- [x] Implement the same model picker for OpenCode's native HTTP adapter.
  Discover the server's models and default through its model endpoint, expose
  them through the bridge, and send the selected provider/model with the next
  prompt. A draft must accept a choice before the session exists; a live task
  must keep its choice across turns and reconnects. Match the ACP picker's
  labels, default handling and unavailable state, and hide the picker when the
  server does not advertise models. Cover discovery, the selected model reaching
  the prompt, a missing or unsupported endpoint, and selection isolation between
  tasks. The native adapter reads `/api/model` and `/api/model/default`, sends
  `/api/session/{id}/model` before a selected prompt, and exposes the same
  bridge command to drafts. Tests cover the default/enabled catalog and the
  browser model selection reaching the turn.
- [x] Classify Turso login failures and show actionable detail.
  When the app reports a Turso failure to log in, use the received error text to
  say whether it is a Turso/database failure or a model-provider sign-in failure.
  Name the provider and model only when the message identifies them; do not
  guess from unrelated text. Keep the short explanation in the failure row and
  put the full received message behind an expandable disclosure. Preserve the
  existing redaction of launch environment values and credentials. Cover a
  provider-specific message, a database-specific message, an ambiguous message,
  and an expanded diagnostic. Auth-required rows name Turso, OpenRouter,
  Anthropic, OpenAI or OpenCode when the received text identifies it, fall back
  to the model provider otherwise, and keep the raw text behind `Error details`.
  The browser smoke covers the Turso summary and the disclosure.

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
| 2026-11-08 | S4 | Partial | The default hidden macOS smoke passed both launches at `91513b9`, using a disposable project and scripted agent. It covered terminal input and output, bracketed paste, task and project switches, saved layout, terminal restart and interrupted-task recovery. The visible-window, macOS candidate-input and workday checks remain separate. |
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

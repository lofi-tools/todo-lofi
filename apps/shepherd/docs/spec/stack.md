# Shepherd — stack

Status: authoritative for the technology choices. Behaviour lives in
[master-spec.md](./master-spec.md); this page says what the app is built from and
how the pieces fit. Where the two disagree on a library or a process boundary,
this page wins.

Two shells, one core:

- **v1** — Tauri 2 with a SolidJS + TypeScript frontend on the system webview.
- **v2** — GPUI + gpui-component (gpui-kit) + gpui-kit-assets.
- **Both** — `libs/shepherd-core`: tokio, harness adapters, PTY and terminal
  state, Git, Turso store. No Tauri or GPUI types. v1 splits it out from the
  first commit precisely so v2 reuses it unchanged.

## Shape

```
┌──────────────────────────────────────────────────────────────┐
│ shell (v1: Tauri webview + SolidJS; v2: GPUI window)         │
│ command palette · task workspace · transcript · canvas terminal │
└───────────────▲───────────────────────────┬──────────────────┘
                │ Event bus                 │ Command + input
┌───────────────┴───────────────────────────▼──────────────────┐
│ libs/shepherd-core (one Rust process, one tokio runtime)     │
│ projects · tasks · terminal layouts · harness · git · store  │
│ terminal host: one emulation thread per terminal             │
│ PTY tasks: portable-pty                                      │
│ libghostty-vt: parse, state, scrollback, key/mouse encoding  │
│ Turso (toasty + toasty-driver-turso), embedded migrations    │
└──────────────────────────────────────────────────────────────┘
```

No daemon, no socket, no attach protocol, no listening port. The core is a
library linked into the shell. In v1 the webview is the operating system's
component, not a second Shepherd process, so the product is still one app that
owns its work and ends it on quit (§2.5 of the master spec).

## Layers

| Layer | v1 | v2 |
| --- | --- | --- |
| App shell | Tauri 2 | GPUI (`gpui-pre` 0.3.4) |
| UI language | TypeScript + SolidJS | Rust |
| Widgets | purpose-built Solid components | `gpui-component` 0.6.1 (gpui-kit) + `gpui-kit-assets` |
| Diff rendering | `@pierre/diffs` + Shiki worker | `gpui-component` diff |
| Markdown | `marked` + `DOMPurify` + `KaTeX` | `gpui-component` text |
| Styling | CSS custom properties from shared tokens | gpui-kit theme tokens from the same palette |
| Terminal painting | `<canvas>` (Canvas2D, WebGL only if measured) | GPUI canvas |
| Shell ↔ core | `#[tauri::command]` + Tauri channel (binary frames) | `libs/gpui-tokio` bridge + event channel |
| Core | `libs/shepherd-core` | same crate, same API |
| Commands | core registry and resolver | same |
| VT engine | `libghostty-vt` | same |
| PTY | `portable-pty` 0.9 | same |
| Async runtime | tokio 1.52 (multi-thread) | same |
| Database | Turso via `toasty` 0.7 | same |
| Search | Turso FTS (Tantivy-backed) | same |

## Shared core (`libs/shepherd-core`)

Cargo: `[lib] path = "src/shepherd_core.rs"`, edition 2024, no `mod.rs`.

| Module | Responsibility |
| --- | --- |
| `shepherd_core.rs` | `Core` handle, startup, shutdown state machine, re-exports |
| `commands.rs` | `Command` enum: every mutation a shell can request; command IDs, labels and defaults |
| `input.rs` | focus and input mode, chord resolution, terminal key routing |
| `events.rs` | `Event` enum: state, transcript, terminal frames, errors |
| `projects.rs` | registry, Git metadata, path validation |
| `tasks.rs` | records, queue, state machine, worktrees, orphan cleanup |
| `harness.rs` + `harness/` | tier selection and adapter implementations: Codex, OpenCode, Pi RPC, ACP, terminal wrapper |
| `terminal.rs` | `TerminalEngine` boundary, frame model, input encoding, scrollback |
| `pty.rs` | spawn, resize, reader/writer tasks, process groups |
| `git.rs` | status, diff, stage, commit, squash-merge, branches, worktrees |
| `search.rs` | cross-task queries over the FTS index |
| `store.rs` | Turso connection, migrations, batched writes |

`libs/agent-harness` is a separate crate (`[lib] path = "src/agent_harness.rs"`):
the `AgentHarness` trait, `HarnessEvent`, capabilities and transcript entry types.
It depends on no UI crate and no database. `libs/acp-client` keeps ACP protocol
types, permissions and fs/terminal client capabilities, and loses its GPUI
dependency; every adapter maps its protocol into `HarnessEvent`.

Rules:

- One dependency direction: shells depend on the core, and the core depends on
  `agent-harness` for the contract. No UI crate. `cargo tree -p shepherd-core` must
  never contain `tauri` or `gpui`, and CI fails the build if it does.
- The core is the only writer to the database and the only owner of PTYs.
- The API is commands in, events out. Snapshots (`ProjectList`, `TaskList`,
  `TaskWorkspace`, `ProjectTerminalLayout`) are returned by command responses so
  a fresh shell can paint without replaying history.
- The core never blocks on a shell: event channels are bounded and drop policy is
  explicit per stream (terminal frames coalesce, task events never drop).
- Testable without a window: adapter fixtures, PTY fixtures and Turso migrations
  run under `cargo test` with no webview and no GPUI.

## v1 shell (Tauri 2 + SolidJS)

- Tauri 2 with the system webview, `tauri-plugin-single-instance`,
  `tauri-plugin-dialog`; no bundled Chromium, no Node server, and no updater in
  v1.
- SolidJS + TypeScript on Vite. Signals hold task workspaces, project terminal
  layouts and transcript pages; the store from the core is the source of truth,
  not a second model.
- Bridge: one `invoke` command per core `Command`, plus a Tauri channel per shell
  for events. Tauri 2 channels deliver raw byte payloads to JavaScript as an
  `ArrayBuffer`, so terminal frames stay binary end to end.
- Terminal view: `<canvas>` with a glyph atlas and a dirty-row blit; the
  component owns only painting, hit-testing and caret geometry.
- Preview tab: a sandboxed iframe for the project's loopback preview URL;
  annotation uses an injected same-origin script, with screenshot notes when
  framing or injection is blocked.
- Input: keydown/keyup, composition and paste events are normalized into input
  events (`code`, `modifiers`, `text`, `ime_commit`) and sent to the core. The
  shell never resolves app shortcuts: the core checks its registry first and only
  encodes unclaimed chords as terminal bytes; **Send next key to terminal**
  bypasses the registry for one chord.
- IME: composition text is sent on `compositionend` as committed text; the
  preedit is drawn by the shell as a floating overlay, never fed to the PTY.
- Clipboard: image paste is read in the webview and handed to the core as bytes;
  copy uses the core's selection so it matches v2.
- Accessibility: the canvas carries an off-screen text mirror in the DOM for
  assistive tech and a keyboard copy mode, since canvas pixels are opaque to
  screen readers.

## v2 shell (GPUI)

- `gpui-pre` 0.3.4 + `gpui_platform`, plus `gpui-component` 0.6.1 (gpui-kit) and
  `gpui-kit-assets` for the widget set; `libs/gpui-tokio` moves core events onto
  the GPUI foreground.
- Entities mirror the same snapshots the v1 shell receives. The core's `Command`
  and `Event` types are reused directly; the only new code is view state,
  layout and painting. Key events use the same normalized pipeline as v1; the
  GPUI shell never resolves app shortcuts.
- Terminal view: a GPUI canvas using the same `TerminalFrame`; text shaping comes
  from GPUI instead of a canvas glyph atlas, but cell metrics, theme mapping and
  selection geometry are shared logic in the core.
- v2 ships as `apps/shepherd-gpui` and runs beside v1 until the parity checklist
  in the migration section passes. Then packaging points at GPUI and the Tauri
  shell is deleted; the core is untouched. Deleting it accepts the remaining
  theme and accessibility gaps as tracked debt.

## Terminal backend

**`libghostty-vt`** (Rust bindings to Ghostty's extracted VT library) is the
engine. It supplies parsing, terminal state, scrollback, reflow, render state and
the key/mouse/focus/paste encoders, which is more than `alacritty_terminal`
offers and is the part that is expensive to get right. `ghostling-rs` — a single
~1000-line emulator built on the same bindings — is the reference for wiring
state, frames and input, and is the first spike before integration.

Constraints the design absorbs:

- The API is explicitly unstable; the crate is pinned exactly and vendored if a
  release breaks, and all use goes through `TerminalEngine` so a replacement is
  local.
- Types are `!Send`/`!Sync`. Each terminal gets its own emulation thread; PTY
  bytes, input events and resize arrive over channels, frames leave the same way.
  This also contains a parser panic to one terminal.
- The build needs Zig. CI installs a pinned Zig and builds libghostty-vt for each
  target in the matrix; this is the one non-Rust toolchain in the project.
- `portable-pty` owns the PTY (same crate both GUI references ship). The engine
  never touches the OS terminal.

`TerminalEngine` boundary:

```rust
trait TerminalEngine {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16, cell_px: (u16, u16));
    fn frame(&mut self, dirty_only: bool) -> Frame;
    fn encode_key(&self, key: &KeyInput) -> Vec<u8>;
    fn encode_mouse(&self, mouse: &MouseInput) -> Vec<u8>;
    fn selection_text(&self) -> String;
}
```

`alacritty_terminal` is the fallback implementation behind the same trait if
libghostty-vt cannot build on a target. It is pure Rust and easy to ship, but it
leaves input encoding, selection and some modes to us, so it is a contingency,
not the plan.

**Frame protocol** (binary, little-endian, version byte first):

```
FrameHeader { version: u8, flags: u8, cols: u16, rows: u16,
              cursor: { x: u16, y: u16 }, display_offset: u32,
              modes: u32, row_count: u16 }
RowRun      { row: u16, run_count: u16 }
CellRun     { count: u16, attrs: u8, fg: u32, bg: u32,
              text_len: u32, text: [u8] }
```

The per-terminal channel identifies the terminal and orders its frames. Header
flag bits 0 and 1 indicate cursor visibility and a full frame. Each row starts
at column zero. A run repeats one UTF-8 cell with the same style, with the cell
width in attribute bits 0–1 and bold, italic and link in bits 2–4. Attach,
resize and scrollback jumps send full frames. Subsequent frames include only
rows that differ from the last frame sent on that subscription. A cursor or
mode change can arrive with zero rows. The canvas repaints changed rows and any
rows affected by cursor movement.

## Store (Turso)

- Embedded **Turso** through `toasty` 0.7 with `toasty-driver-turso`, the same
  stack `libs/storage` already uses. Database file in the platform data dir;
  Shepherd has its own file and schema.
- User settings live outside the database: `settings.json` and `keybindings.json`
  in the app config dir, versioned and written atomically; project overrides are
  keyed by absolute path (master spec §11.1).
- Migrations are numbered SQL files in `libs/shepherd-core/migrations/`
  (`0001_init.sql`, `0002_…`), embedded with `include_dir`, applied at startup
  in one transaction after a file backup, with the applied version recorded in a
  migration table.
- One writer task; transcript entries are batched per settled message or tool
  call, never per token. Reads go through the core, not the shells.
- **Search is Turso FTS, which is Tantivy-backed and not SQLite FTS5.** Turso
  does not run FTS5 virtual tables, so the schema uses Turso's own FTS index and
  `search.rs` hides the query syntax. Ranking and tokenisation therefore differ
  from SQLite: the spec's search budget and tests are written against Turso's
  behaviour, not FTS5's.
- Tokenizer: the Unicode-aware default with prefix queries, decided. `ngram` is
  available for substring matching and is not enabled in v1.
- No `libsql` remote, sync or embedded replica in v1 or v2. The database is a
  local file, full stop.

## Workspace layout

| Path | Contents |
| --- | --- |
| `libs/agent-harness` | UI-free adapter trait, `HarnessEvent`, capabilities and transcript entry types |
| `libs/shepherd-core` | shared core, terminal engine wrapper, Turso store, adapter implementations |
| `apps/shepherd` | v1 Tauri shell: `src-tauri/` (commands, channel bridge), `ui/` (SolidJS + TS) |
| `apps/shepherd-gpui` | v2 GPUI shell (added at M6) |
| `libs/acp-client` | ACP protocol types and client capabilities, used by the ACP adapter; no GPUI dependency, fake agent behind `test-support` |
| `libs/storage` | reference for toasty + Turso wiring and migration loading, not a dependency of Shepherd |
| `libs/gpui-tokio` | v2 only: tokio → GPUI foreground bridge |

Workspace rules apply: no `mod.rs`, no `unwrap`, full-word names, errors
propagated, no silently discarded fallible calls.

## Pinned dependencies

| Dependency | Version | Where |
| --- | --- | --- |
| `tokio` | 1.52 (workspace) | core |
| `libghostty-vt` | exact pin, vendored on breakage | core |
| `portable-pty` | 0.9 | core |
| `toasty` + `toasty-driver-turso` | 0.7 | core |
| `serde`, `serde_json` | 1 | core, shells |
| `tracing` | 0.1 | core |
| `tauri` | 2.x | v1 only |
| `solid-js` | 1.x | v1 only |
| `@pierre/diffs` + `shiki` | pinned | v1 only |
| `marked`, `dompurify`, `katex` | pinned | v1 only |
| `vite`, `typescript` | current stable | v1 only |
| `gpui-pre` / `gpui_platform` | 0.3.4 | v2 only |
| `gpui-component` / `gpui-kit-assets` | 0.6.1 | v2 only |

## Build and CI

- Rust: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
  `cargo test --workspace`.
- libghostty-vt: pinned Zig in CI, plus a smoke test that parses a fixture and
  produces a frame, so a silent C build regression fails fast.
- v1 frontend: `pnpm` install, `tsc --noEmit`, `vite build`, Playwright smoke
  against a dev server.
- ACP crate: `cargo check -p acp-client` without UI dependencies, and
  `cargo test -p acp-client --features test-support` for the fake agent.
- Boundary check: fail if `cargo tree -p shepherd-core` contains `tauri`, `gpui`
  or a webview crate.
- Migration check: apply all migrations to an empty database and to a fixture
  from the previous schema in CI.
- Platform floor: macOS 13 (Ventura) and Ubuntu 24.04 with WebKitGTK 2.44+.

## v1 → v2 migration

v1's UI is deliberately thin — core flows only — so the migration is view code,
not a redesign. v2 is unscheduled: work starts when v1 is feature complete.

1. Keep `libs/shepherd-core` free of shell types from the first commit; land the
   `Command`/`Event` types with v1 so they are exercised by a real shell.
2. Put the frame model and input encoding in the core, not in the canvas
   component, so v2 reuses both.
3. Add `apps/shepherd-gpui` at M6 and run it beside v1 on the same database file
   (never at the same time).
4. Parity checklist: every v1 flow (task workspace, transcript, terminal, approvals,
   files, Git, search, notifications, quit), plus migrated keybindings/settings
   and transcripts/search history. Theme and accessibility parity are follow-ups,
   not part of the first v2 build.
5. Delete the Tauri shell, keep the core's public API and schema unchanged.

## Risks worth tracking

| Risk | Mitigation |
| --- | --- |
| libghostty-vt API breaks | exact pin, vendored copy, one trait boundary, `alacritty_terminal` fallback |
| Zig toolchain in CI | pinned version, cached builds, frame smoke test |
| Canvas frame throughput | dirty rows, coalescing, 1 MB burst budget, measure before WebGL |
| IME differences between webview and GPUI | normalize input events in the core, test composition in both shells |
| Turso FTS semantics differ from FTS5 | query syntax behind `search.rs`, budget and tests against Turso |
| Theming drift between CSS tokens and gpui-kit tokens | one token source in the core, generated for both shells |
| Two shells during migration | v2 starts read-mostly, same DB file, explicit parity checklist |
| Hand-edited settings file is malformed or newer | atomic writes, load-time backup, banner with the path, **Reload configuration** command |
| Keybindings drift between shells | one core registry and one resolver; shells only normalize key events |
| v2 ships without full theme or a11y parity | accepted debt at M6; keep the token source and the terminal text mirror in the core so both can land without restructuring |

## Non-goals

- No Electron, no bundled Chromium, no Node server.
- No remote database, sync or embedded replica.
- No embedded JavaScript plugin runtime; mods are a v3 question, not v2.
- No command-line interface in v1.
- No update checks or auto-update in v1.
- No second window, daemon or attach protocol in either shell.

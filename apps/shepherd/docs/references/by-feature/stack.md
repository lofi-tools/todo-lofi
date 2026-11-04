# Stack of the reference apps

What each checkout is built from, layer by layer. Everything here was read from
the manifests and source in `.cache/checkout/`, not from project websites.

Basis:

| Checkout | Version | Commit |
| --- | --- | --- |
| herdr | 0.9.5 | `ee5bfb5041bb3e4a49d591c72308aae3c81ecdde` |
| herdr-core | 0.1.17 | `cd6ba77c710b07e97ea53aaaadcbc56f9814240c` |
| herdr-app | 0.1.12 | `5b3736312f7eff558de49e9883cadb0700fb8adb` |
| pandacode | 0.3.92 | `28b835f8b8cb72dcda5506d4bcfff489ad9a98dc` |
| pi | 1.5.0 (packages) | `a7a75a9dc6891281eed58906c07c55929e7b6c13` |

## At a glance

| Layer | herdr | herdr-core | herdr-app | pandacode | pi |
| --- | --- | --- | --- | --- | --- |
| Language | Rust 2021 | Rust 2024 | Rust 2024 | Rust + TypeScript | TypeScript |
| Main loop | tokio + crossterm | tokio, headless | GPUI foreground thread | tokio + webview | Node event loop |
| GUI toolkit | ratatui | none | GPUI + gpui-kit | none (DOM) | none (pi-tui) |
| Styling | Rust styles | — | JSON theme tokens | hand-written CSS | JSON theme tokens |
| Terminal emulation | ghostty-vt | ghostty-vt | ghostty-vt (`ghostty-sys`) | xterm.js | none |
| PTY | portable-pty | own PTY layer | via core | portable-pty | none (inherits one) |
| Scripting | none embedded | none | JavaScript (QuickJS) | none (markdown skills) | TypeScript + JS |
| Primary store | `session.json` | TOML config | JSON files | SQLite | JSONL |
| App protocol | local socket JSON | local socket OpenRPC | core CLI/socket | Tauri IPC | JSONL RPC |

## Main app loop

### herdr — client/server TUI

`herdr` is two roles in one binary: a background server that owns every PTY, and
a TUI client that attaches to it. The server runs a multi-threaded tokio runtime,
reads PTY output on its own tasks, parses it with its vendored `ghostty-vt`, and
serves a local-socket JSON API (`interprocess`) to the CLI and to agents. The
client is a `ratatui` app driven by `crossterm` input and the server's frame
stream; layout and focus are held client-side so several clients can attach with
independent views. Detaching is just dropping the client; the server keeps
running, which is why persistence restores layouts rather than processes.

### herdr-core — headless runtime

No render loop at all. A tokio process starts programs in PTYs, keeps their
screens, streams bytes to attached viewers, and exposes an OpenRPC API over a
local socket plus a CLI. It also carries the cross-machine layer (`remote/`,
`machines.rs`) so a terminal can live on another host and be driven from here.

### herdr-app — GPUI window

A GPUI application, so the loop is the framework's: one foreground thread owns
all entities and rendering, `cx.spawn` work is scheduled on that thread's
executor, and IO/process work runs on the background executor. Frames are
vsync-driven; herdr-app vendors `gpui-pre-macos` with a patch so an idle window
stops waking every vsync, and ships an `fps` crate to measure the result. The
window does not own terminals: it starts and talks to `hc` (`core_bin.rs`,
`hc.rs`), and renders the streams core sends it. Layouts, mods and the command
palette sit on top of the GPUI entity graph.

### pandacode — Tauri

Two loops that meet at the IPC boundary. The Rust side is a tokio runtime that
owns PTYs (`portable-pty`), harness child processes, SQLite access, cloud and
relay clients; the UI side is plain TypeScript in the operating system's webview
(WKWebView, WebView2, WebKitGTK) with no React, no bundled Chromium and no
resident Node server. The UI updates from `invoke` promises and Tauri events;
modules that animate (`motion.ts`, `choice-menu.ts`, `diff-prefetch.ts`,
`simulator-3d.ts`) schedule their own `requestAnimationFrame` work. A separate
headless mode runs the same native commands without a window.

### pi — Node, four modes

One Node process per agent, built on the event loop rather than a render loop.
Interactive mode is a custom TUI (`@earendil-works/pi-tui`) that redraws only
changed lines with CSI 2026 synchronized output; RPC mode keeps the same process
alive and speaks JSONL; print and JSON modes run once and exit. The agent loop
itself lives in `packages/agent`, with `packages/coding-agent` adding modes,
tools, sessions and extensions on top.

## GUI and UI components

**herdr-app** gets its components from the vendored `gpui-kit`:

- `crates/base` is the widget set: accordion, alert dialog, avatar, button,
  calendar, checkbox, collapsible, color picker, combobox, date picker, dialog,
  dock, hover card, input, number input, OTP input, pagination, plot, popover,
  settings list and others — all native GPUI elements.
- `crates/shell` is the host for mods: dock, component registry, theme and input
  tokens, syntax, storage, file watching, script callbacks, and the QuickJS
  engine.
- `crates/fps` measures frame pacing.

herdr-app also patches the framework itself (taffy min-content measurement,
macOS vsync idle wakeups, Linux maximized-window snapping, XIM decoding, Windows
dead keys), which is the practical cost of a young native toolkit.

**pandacode** has no component framework. Panels are imperative TypeScript
modules that build DOM through helpers in `dom.ts` (`escape`, SVG icon paths,
file-link decoration, scroll preservation), one module and one CSS file per
feature — 106 CSS files under `src/`. That is a deliberate trade: no virtual DOM
or build-time styling layer, at the cost of hand-written markup and CSS.

**herdr** uses `ratatui` widgets with `crossterm` input, and **pi** ships its own
component model (`Text`, `Editor`, `Markdown`, `SelectList`, `ScrollView`, `Box`,
`HStack`, `VStack`, …) with interchangeable main-screen and alternate-screen
renderers, mouse/trackpad scrolling and Kitty/iTerm2 inline images.

## Styling

- **pandacode**: CSS custom properties selected by `html[data-theme]`, plus
  per-feature CSS files. No Tailwind, no CSS-in-JS, no utility framework; themes
  (including imported VS Code themes) write the same variables.
- **herdr-app**: JSON token files — `default-tokens.json`, a palette preset or
  `themes/<name>.json` with `colors` overrides — consumed by gpui-kit's theme
  layer, so mods and chrome share one token vocabulary.
- **pi**: JSON themes with 37 required color tokens across core UI, message and
  tool backgrounds, markdown, diffs and syntax, resolved from hex/256-color
  variables and downgraded to 256 colors when the terminal lacks truecolor.
- **herdr**: styles are constructed in Rust as `ratatui` `Style` values; no
  theme file format of its own.

## Customization and extension languages

| App | Mechanism | Language | Isolation |
| --- | --- | --- | --- |
| herdr-app | mods: folders under `~/.config/herdr-app/mods` with `mod.json` | JavaScript | QuickJS VM in-process, capability grants |
| herdr | installed plugins + custom commands | none embedded; plugin artifacts | external programs / socket API |
| pandacode | agent skills, rules, themes, design tokens | Markdown, JSON, CSS | none (no app scripting runtime) |
| pi | extensions, codemode, skills, packages | TypeScript, JavaScript | in-process (extensions), QuickJS-WASI (codemode) |

**herdr-app mods** are the deepest host API in the set. Mods are loaded live
from disk, get `herdr.d.ts` and `gpui-kit.d.ts` for the installed version, and
run in a QuickJS VM (the `rquickjs`/LLRT path, with a patched `quickjs-jit`
runtime). They are declared capabilities rather than ambient trust:
`ExecuteGrant` and `NetworkGrant` gate running programs and network access, and
the security doc states plainly that a mod can run programs with your
environment, fetch URLs and read files. `herdr-app check <dir>` loads a mod and
renders it once to catch errors; the frame's own mods are compiled into the
binary and rewritten on every start so a broken personal mod cannot break the
shell. Save-to-reload is the development loop.

**pi extensions** are TypeScript modules loaded with `jiti` (so `.ts` runs
without a build step) inside the Pi process, with the same OS permissions as Pi.
They subscribe to lifecycle events, register tools and commands, render custom
TUI components, intercept tool calls, and can hot-reload with `/reload`.
Codemode is the sandboxed counterpart: model-written JavaScript runs in a
QuickJS VM compiled to WASM with only injected tools as capabilities, capped at
two concurrent workers per process.

**herdr** has no embedded scripting engine — nothing in its manifest can execute
JS. Plugins are installed artifacts recorded in `plugins.json` and invoked
through plugin actions and custom-command keybindings; the work happens outside
the TUI process.

**pandacode** has no app-level plugin runtime either. Its customization surface
is aimed at agents: skills and rules as markdown, imported themes, design tokens,
and a design-canvas/moodboard feature. Harness behaviour lives in Rust adapters,
not user scripts.

**herdr-core** exposes only its API; there is nothing to script inside it.

## Terminal stack

| Concern | herdr / herdr-core / herdr-app | pandacode | pi |
| --- | --- | --- | --- |
| VT parsing | vendored `libghostty-vt`, Rust bindings (`ghostty-vt`, `ghostty-sys`) | `@xterm/xterm` 6 in the webview | none — draws into a real terminal |
| PTY | `portable-pty` (herdr vendors a patched 0.9) | `portable-pty` 0.9 | inherits the caller's terminal; tools spawn normal children |
| Screen model | core keeps screens so late viewers and restarts can replay | xterm.js buffer | pi-tui's component tree, not an emulator |
| Graphics | Ghostty VT capabilities | xterm.js addons (`addon-fit`) | Kitty / iTerm2 inline images |
| Clipboard | GPUI / core | OS clipboard; `arboard` on Linux/Wayland for pasted images | native NAPI clipboard addon per platform |

Both graphical apps landed on `portable-pty`; the emulation layer is where they
diverge. Ghostty gives herdr a real terminal engine shared by the TUI, the core
and the desktop app, with the C library vendored and built through bindgen.
xterm.js gives pandacode a browser-native terminal that matches its webview UI,
and needs the `addon-fit` addon to size cells to the panel. pi has no emulator at
all because it is not a terminal multiplexer: it renders its own UI to whatever
terminal it was started in, and `@xterm/headless` appears only as a dev
dependency for testing its output.

## Storage and databases

| App | Primary store | Notes |
| --- | --- | --- |
| herdr | `~/.config/herdr/session.json` | plus `session-history.json` and `plugins.json`; atomic replace before rename |
| herdr-core | `~/.config/herdr-core/config.toml` | no database crate; terminals live in the running server |
| herdr-app | JSON files in the app config dir | `settings.json`, `themes/*.json`, `mods/`; atomic writes in `persist.rs` |
| pandacode | SQLite via `rusqlite` (bundled) | schema tracked with `PRAGMA user_version`, currently 36, migrated step by step |
| pi | JSONL session files | `~/.pi/agent/sessions/--<path>--/<timestamp>_<id>.jsonl`, version 3 tree via `id`/`parentId` |

Only pandacode ships a relational database. Its `rusqlite` is built with
`bundled`, so the SQLite amalgamation compiles into the binary and no system
library is required; migrations are explicit numbered steps keyed on
`user_version`, with tests asserting the version matches. Attachments are files
under the app data directory rather than blobs, and provider credentials go to
the OS keyring (`keyring` crate; Secret Service on Linux, native stores on macOS
and Windows). Transcripts are imported from other tools' JSONL formats, but the
app's own state is the database.

pi stores conversations as append-only JSONL, one entry per line with a `type`
field; entries form a tree with `id`/`parentId`, so branching and forking happen
in place without new files, and the format is versioned (v1 linear, v2 tree, v3
renamed role) with automatic migration on load. SQLite appears only in the
reusable `durable` and `chord` packages, through Node's built-in `node:sqlite`
with an additional Cloudflare D1 adapter — not in the agent's own session path.

herdr and herdr-app both use hand-rolled atomic file writes rather than a
database: serialize, write a temporary file, rename. herdr-core keeps terminals
in process memory and persists only configuration, which is consistent with its
"runtime, not multiplexer" rule.

## Process boundaries and IPC

| App | Boundary | Transport |
| --- | --- | --- |
| herdr | client ↔ server, CLI ↔ server, agents ↔ server | local socket (`interprocess`), JSON messages |
| herdr-core | viewers/CLI ↔ runtime, remote machines | local socket, OpenRPC; SSH for machines |
| herdr-app | window ↔ core | spawns `hc`, then CLI/socket calls |
| pandacode | webview ↔ Rust, app ↔ relay/cloud | Tauri `invoke` commands + events; HTTPS/WebSocket for cloud |
| pi | host ↔ agent process | JSONL RPC on stdin/stdout; SDK embeds in-process |

pandacode's IPC is the least visible and the most typed: every command takes
camelCase arguments, returns a Promise, and rejects with a string; a headless
mode exposes the same native commands without the webview. herdr's socket API is
JSON over a per-session local socket, with the CLI as its first client and
agents as the second — the same surface serves humans and automation.

## Libraries by concern

| Concern | herdr family | pandacode | pi |
| --- | --- | --- | --- |
| Markdown | — | `marked` + `DOMPurify`, `katex` for math | `marked` (+ CJK-friendly fork) |
| Syntax highlighting | gpui-kit `syntax` module | Shiki (Oniguruma WASM) in a worker pool | `highlight.js` |
| Diffs | — | `@pierre/diffs` with virtualized file diffs | `diff` |
| Documents | — | `pdfjs-dist`, `xlsx` (SheetJS), `docx-preview`, `three` | — |
| Images | `image`, `png`, `ttf-parser` | image via Tauri, papered into previews | `photon-node` (WASM resize) |
| Icons | SVG assets in `defaults/icons` | `material-icon-theme` + inline SVG paths | — |
| Search / globs | `regex`, `fancy-regex` | `ignore`, `regex` | `minimatch`, `ignore` |
| File watching | `notify` (app) | — | extensions / `chokidar`-style watchers in packages |
| Updates | self-update (`hc update`, app `update.rs`) | `tauri-plugin-updater` + `plugin-process` | npm / Bun binary releases |
| Desktop integration | `windows` notifications, `zbus` logind inhibitor, `wmi` | `single-instance`, `global-shortcut`, tray icon, `objc2`/WebKit glue | — |
| Crypto / secrets | `sha2` (session hashing) | `keyring`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `hmac` | — |
| ML | — | `ort` (ONNX Runtime) + `tokenizers`, aarch64-only model router | — |
| Testing | Rust tests + in-tree TS integration tests | `node:test`, `cargo test`, Playwright | `vitest`, `node:test`, `@xterm/headless` |
| Telemetry | `telemetry.rs` in the app | none (stated privacy stance) | `packages/telemetry` |

## Categories worth investigating

Beyond the layers asked about, these are the areas where the references make
concrete, non-obvious choices.

1. **Native GPU UI versus system webview.** herdr-app shows what GPUI buys
   (frame control, no webview, one language) and what it costs (vendored forks
   of gpui, taffy, xim; patches for idle vsync, min-content measurement and dead
   keys). pandacode shows the opposite trade: an OS webview and plain TS, with
   per-platform glue for WebKit, WebView2 and WebKitGTK. Worth deciding early
   because it fixes the component, styling and accessibility stories.
2. **Terminal emulation depth.** Ghostty VT versus xterm.js is not cosmetic:
   scrollback semantics, reflow, hyperlinks, graphics protocols, truecolor and
   Unicode width handling all differ. A terminal that only replays byte streams
   is much cheaper than one that must keep a screen model for late viewers.
3. **Terminal-adjacent platform work.** IME and dead keys (herdr-app vendored
   `zed-xim`/`xim-ctext` and patched Windows input), pasted images on Wayland
   (`arboard` with data-control), font fallback and shaping (`font-kit`,
   `ttf-parser`), and clipboard ownership on each OS. These are where desktop
   terminal apps usually break first.
4. **Extension runtime and sandboxing.** Four distinct answers: in-process JS in
   a QuickJS VM with explicit grants (herdr-app), in-process TS with full OS
   permissions (pi extensions), WASM QuickJS with injected tools only (pi
   codemode), and no embedded engine at all (herdr, pandacode). The deciding
   questions are memory limits, capability enforcement, API typing generation,
   hot reload and the trust prompt.
5. **Schema and format versioning.** pandacode's stepwise `user_version`
   migrations and pi's auto-migrating session versions are two mature patterns;
   herdr and herdr-app show the file-based alternative (atomic replace, tolerate
   unknown fields, keep working when parsing fails).
6. **Process topology.** Every reference except the app/window split keeps the
   runtime and UI in separate processes. That is what makes terminals survive a
   closed window. A single-process app has to solve the same expectations with
   explicit quit semantics instead.
7. **Update and distribution.** Tauri's signed updater plugin, herdr's
   self-replacing binary, and pi's npm/Bun packaging each imply different
   failure modes (partial updates, running processes, version skew between CLI
   and app). Packaging also shows up as Homebrew formulae, Nix flakes and
   install scripts in these repos.
8. **Telemetry and crash reporting.** herdr-app has a telemetry module, pi has a
   telemetry package, pandacode deliberately has none. If Shepherd collects
   anything, the decision interacts with the local-only stance.
9. **Remote transports.** SSH machines (both herdr lines), pandacode's relay and
   cloud agents, and pi's remote environment package are three levels of
   ambition. End-to-end crypto (x25519 + ChaCha20-Poly1305) only appears in
   pandacode, where a relay sees ciphertext.
10. **UI test harnesses.** GPUI render tests plus pixel snapshots
    (`tests/shot_pixels.rs`), Playwright against the webview, and headless
    terminal emulation for TUI output are the three viable approaches; only the
    first two give screenshots without a display.
11. **Performance instrumentation.** herdr-app's `fps` crate, pandacode's
    `bench/` directory and pi's profiling scripts are worth copying earlier than
    feels necessary: rendering and transcript scrolling are the two places these
    apps get slow.
12. **Accessibility and internationalization.** gpui-kit carries macOS
    accessibility support, pandacode's DOM gets ARIA for free but still writes
    `aria-checked`/focus styles by hand, and pi depends on East Asian width
    handling to keep terminal layout correct.

## What this implies for Shepherd

- GPUI is a proven base for exactly this app class; herdr-app also documents the
  patches a young toolkit still needs, which is the risk to budget for.
- All three terminal-bearing references use `portable-pty` for PTYs. Shepherd's
  choice of `alacritty_terminal` diverges on emulation, not on process control:
  it avoids a vendored C build and bindgen, and gives up Ghostty's graphics and
  feature coverage.
- `rusqlite` with the `bundled` feature and numbered `user_version` migrations is
  the reference-grade local database choice; pandacode runs it at schema 36.
- herdr-app is the only reference that ships a general scripting runtime inside
  the app (QuickJS with grants). Deferring that for v1 keeps Shepherd's trust
  model simple without closing the door.
- Pi's TypeScript extensions and JSONL RPC are the same surface Shepherd's Pi
  adapter speaks, so the protocol work is shared rather than reinvented.

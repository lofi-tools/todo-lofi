# herdr-app (Herdr 2)

The desktop window for terminals and coding agents: a native GPUI client of
`herdr-core`. Work lives in spaces, agents that need you surface in a sidebar,
and terminals keep running when the window closes because the core owns them.

- Checkout: `.cache/checkout/herdr-app` at `5b3736312f7eff558de49e9883cadb0700fb8adb`
- Upstream: <https://github.com/herdrdev/herdr-app>
- License: Apache-2.0

Two programs make up the product:

- `herdr-app` — the window: spaces, tabs, layouts, panels, mods.
- `herdr-core` (`hc`) — runs every terminal and agent and keeps them alive. The
  app is one client of it, and users can drive `hc` themselves.

The app deliberately stays small: its core is the model, machine layout
documents, commands and shared mod APIs; everything else is a mod. Source map:
`src/` (`window/`, `mods/`, `model.rs`, `commands.rs`, `layout.rs`, `keys.rs`,
`settings.rs`, `mirror.rs`, `sprites.rs`, `core_bin.rs`, `hc.rs`), shipped
interface files in `defaults/`.

## Features

### Spaces, tabs and panes

- A **space** holds work on one machine, usually one repo; each space has
  **tabs**, and a tab splits into **panes** holding terminals or mod panels.
- Unnamed spaces follow their first terminal's folder; naming a space or tab
  pins a label. New tabs open in the focused terminal's folder.
- Split, close, zoom, move and swap panes; select and reorder spaces and tabs;
  "settle" and "unsettle" a tab to mark work as parked.
- Terminals can be kept, parked, pinned, peeked (an overlay over the current
  tab), shown, scrolled, searched, copied from and pasted into, including image
  paste.

### Agents sidebar

- Agents from every space, including workers started by another agent, appear in
  one list.
- States: **Working** (busy), **Blocked** (waiting for you), **Done** (finished
  while you were elsewhere) and **Idle** (not working, or seen).
- Clicking an agent shows it; a worker without a pane opens in **peek** over the
  tab and can be kept as a tab.
- Live terminal addresses in terminal text (`t7`, `vps/t7`) open the terminal on
  click (Ctrl-click, Cmd-click on macOS).

### Runtime model and recovery

- The window arranges terminals; the core runs them. Closing the window leaves
  them running; closing a pane or tab ends its terminals.
- After a reboot or core crash, kept panes get fresh shells in their last
  folders, not their old programs.
- The app can install or update a core it cannot use (`core_bin.rs`,
  `update.rs`), and `app.update_core` is a command.

### Multiple machines

- Add an SSH machine from Settings → Machines or the sidebar's Add machine
  action; pick a host from the SSH config or type how SSH reaches it.
- Spaces on that machine run their terminals there. Each machine keeps its own
  spaces, tabs and splits in its core; connected screens share structure and
  live terminals but keep their own focus and zoom.
- Remote work continues while the local window is closed or the laptop sleeps,
  as long as the remote machine and core stay up.

### Layouts and mods

- A **layout** arranges the window; **mods** are small JavaScript programs that
  draw its panels, menus and dialogs. The whole interface is files under
  `~/.config/herdr-app` (`defaults/` holds the shipped copies):
  - `layouts/main.json`, `layouts/terminals.json`, `layouts/win98.json` built-in
    layouts (Herdr, Terminals, Herdr 98), plus themes and icons;
  - ~20 shipped mods: `agents`, `changes`, `confirm`, `files`, `help`,
    `machine-add`, `menu`, `new-menu`, `pane`, `peek`, `picker`, `prompt`,
    `spaces`, `tabs`, `terminals-bar`, `terminals-pane`, `toast`, `win98`,
    `workspace-chrome`, `workspace-status`, `worktree`;
  - `gpui-kit.d.ts` and `herdr.d.ts` describe the full element and app API and
    are regenerated on start.
- Mods use gpui-kit (JavaScript describing GPUI elements) — no DOM, HTML, CSS,
  web view or Node APIs. They can run programs, read files, fetch URLs, keep
  per-mod or shared state, open dialogs, popups and floating blocks, embed live
  terminals and animate natively.
- Editing is live: saving a layout, mod, key or settings file reloads it within
  a second. A broken edit keeps the last good version and writes `<name>.error`.
- Shipped layouts and mods are replaced on start; edited versions are moved to
  personal copies, which stay yours.
- `ctrl-alt-shift-escape` is a hard escape hatch: it sets the shown layout and
  `keys.json` aside and returns to the default `main` layout.
- `herdr-app check <mod folder>` loads a mod once and validates its description,
  native elements, virtual lists and motion; `herdr-app shot [file.png]` saves
  the last rendered frame as a PNG for UI debugging.

### Keys, commands and palette

- Every action is a command with an ID, so keys, mods, the palette and scripts
  all address the same surface. Command groups in `docs/commands.md`:
  - `space.*` — new, close, rename, move, select, next, prev
  - `tab.*` — new, close, rename, move, select, next, prev, settle, unsettle
  - `pane.*` — split, close, zoom, focus, go, swap
  - `terminal.*` — open, keep, park, right-click, pin, kill, peek, show, copy,
    scroll, paste, paste image, search
  - `agent.*` — focus, next, prev
  - `layout.*` / `mod.*` — switch, clone, set, toggle, show, hide, resize,
    select, reset, open
  - `app.*` — help, welcome, what's new, feedback, palette, settings, check
    updates, update core, reset, theme, zoom, update, quit, hide, hide others,
    show all
  - `window.*` — close, minimize, maximize; `overlay.close`
- Command palette (`ctrl-shift-p`, `cmd-shift-p` on macOS) lists every command
  with its key; any key can be rebound in `keys.json` / `keys.macos.json`,
  including JSON objects for typed command arguments and `send:` sequences for
  raw terminal input.

### Settings and appearance

- Settings cover fonts, colors, sounds, terminal padding/opacity, cursor, 16
  ANSI colors, pane gaps/radius/background, layouts and privacy.
- Themes ship (`atom-one-dark`, `win98`) and named layouts can carry their own
  frame; per-OS key files (`keys.json`, `keys.macos.json`) are installed only
  when missing so new defaults do not overwrite user choices.
- Windows: sprite support for terminal graphics (`src/sprites.rs`), Win32 key
  handling and window controls.

### CLI for agents

- `herdr-app --skill` prints the window manual (`src/SKILL.md`), which the
  herdr-core skill points agents to.
- `herdr-app ls` and `herdr-app status` inspect the open window;
  `herdr-app commands` lists commands without a window.
- `herdr-app check`, `herdr-app shot` as above.
- Agent-facing behaviour is documented in `defaults/AGENTS.md`, which is written
  into every user's config folder on start; `cargo test` checks that it names
  every command and settings key.

### Updates, privacy and security

- Installed releases check for updates in the background, download beside the
  running app and take over on restart or on the update action; terminals keep
  running in the core. Settings → Updates shows what is available; What's New
  opens after an update.
- Builds from source do not self-update.
- Usage telemetry is on by default in releases and can be turned off in
  Settings → Privacy.
- Mods are trusted code, not sandboxed extensions; the security doc covers the
  trust model.

### Engineering notes

- The app's core stays small: anything that only changes appearance belongs in a
  mod, and anything the core needs from herdr-core is discussed with the
  maintainer first.
- Compatibility with released versions and installed user data is preserved;
  breaking changes require explicit agreement.
- Platform quirks stay behind small per-OS modules with a comment and a test on
  that platform, never spread through shared code.
- Drawing performance is measured with `scripts/bench.py` (fixed workloads,
  spinners/TUI sets) and compared with `scripts/bench_compare.py`; benchmark
  tests run with `python -m unittest discover -s scripts -p test_bench.py`.
- Build: Rust 1.95.0 and Zig 0.16.0, `just build`, `just test`, `just install`,
  `just docs-check` (needs Bun).

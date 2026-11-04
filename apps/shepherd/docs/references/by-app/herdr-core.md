# herdr-core (`hc`)

A headless terminal runtime for coding agents. It runs programs in terminals
that stay alive after you leave, relays their bytes to any viewer, reads status
from the Program Status Protocol, and lets agents start, talk to, read, wait on
and be notified by other agents. It draws nothing: no layout, no workspaces, no
window.

- Checkout: `.cache/checkout/herdr-core` at `cd6ba77c710b07e97ea53aaaadcbc56f9814240c`
- Alias: `hc`; crate `crates/herdr-core`, agent manual in
  `crates/herdr-core/src/SKILL.md` (`hc --skill`)
- Upstream: <https://github.com/herdrdev/herdr-core>
- License: Apache-2.0

`hc` is not the older `herdr` multiplexer. Its stated rule is "Core is a runtime,
not a multiplexer": a feature that only makes sense with a layout or a workspace
to return to does not belong in core. Source map: `crates/herdr-core/src/`
(`runtime*`, `server.rs`, `holder.rs`, `terminals.rs`, `wait.rs`, `notes.rs`,
`machines.rs`, `templates.rs`, `files.rs`, `api/`, `cli/`, `remote/`,
`viewers/`, `platform/`).

## Features

### Persistent terminals

- `hc run [--bg] [--name N] [--cwd DIR] [--keep] [--on MACHINE] [program ...]`
  starts a program in a terminal owned by a background server. `--bg` returns
  immediately; without it the caller streams output and waits.
- Closing the viewer does not end the program. A waiting run checks in after 180
  seconds by default, leaving the program running and printing the terminal's
  address so it can be read or waited on again.
- `hc attach <t>` takes over interactively (`Ctrl+\` detaches); `hc read <t>
  --lines N` reads without attaching; `hc ls` lists live terminals.
- `--keep` keeps the terminal alive even if its program is lost. Recovery after
  a restart starts a fresh shell and never re-runs the task; a normal exit or an
  explicit kill still ends it.
- Default shell comes from `$SHELL` (PowerShell 7 on Windows when available);
  overridable with a `shell = [...]` list in
  `~/.config/herdr-core/config.toml`. A bad shell reports a config or
  `spawn_failed` error and keeps retrying instead of silently falling back.
- Terminals are addressable as an ID (`t7`), a name (`review`) or
  `machine/t7`. Piped output is JSON except `run`, `exec`, `read` and `file
  read`, which stay text.

### Agent-to-agent coordination

- Every agent started inside an `hc` terminal becomes that terminal's child.
  Children end with their parent unless started with `--outlive-parent`.
- `hc send <t> "text"` delivers input, `hc send --wait` waits for the turn,
  `hc interrupt <t>` stops the current turn without killing the process, and
  `hc keys <t> down enter` drives a TUI directly.
- `hc wait <t>` blocks until the terminal stops working or exits; `hc wait <t>
  --output X` waits for a specific string (for example a server ready line);
  `hc wait --children all|any` waits for child agents. The manual forbids sleep
  or polling loops in favour of these primitives.
- Parent notes (`notes.rs`) tell a parent when a child needs attention or ends.
- Sends respect human drafts: a send stays pending while a person has unsent
  input in that terminal, expires after three minutes, and reports
  `waiting_for_person`/`waited_for_person` instead of overwriting the draft.
- `hc get <t>` returns observed state: status, foreground process, current
  folder, draft hold and `first_message.sent` for launches.
- Reading history is careful with full-screen apps: it scrolls, then restores
  the view, and refuses to scroll while the program is working, blocked or in
  error, marking the read incomplete with a reason.

### Status from the program, not the screen

- Programs report `working`, `done`, `blocked` or `error` through OSC 7501, the
  Program Status Protocol. Core stores what it reported and does not guess from
  screen text or recognise agent brands. A program that does not report has no
  status.
- `hc get <t>` returns a snapshot; `hc events` follows changes as they happen.
- This is a deliberate contrast with `herdr`, which classifies agents by screen
  detection.

### Templates

- Reusable agent roles: one Markdown file per role under
  `~/.config/herdr-core/templates/<name>.md`, with frontmatter for `name`,
  `description` and `command` (including model flags) and a body of
  instructions.
- `hc templates` lists them; `hc run --bg --name review reviewer "task"` starts a
  role with the task as the first message, so a send to a still-booting agent is
  not dropped.
- Template launches allow up to 60 seconds for readiness, retrying startup
  settle failures; failures are recorded in `first_message` for later
  inspection instead of blindly resending.

### Machines

- `hc machine add user@host --name box` registers an SSH machine (including
  hosts from the SSH config), installing `hc` there when missing or too old
  (`--install` for scripts, interactive confirmation in a terminal).
- `hc run --on box --cwd /srv/app cargo test` runs there; `hc attach box/t3`
  attaches. Programs and files stay on that machine; local core brings its
  terminals into the same list.
- Multiple core sessions on one host can be reached with
  `hc machine add user@host --session scratch`.
- Machine commands: `add`, `ls`, `login`, `rename`, `rm`, `update`.
- Remote files are read and written through `hc file`/`hc put` over the same
  connection; there is a Python example for a Vercel sandbox in
  `docs/via/vercel-sandbox.py`.

### Files and one-off execution

| Command | Purpose |
| --- | --- |
| `hc file read` | read a file on the terminal's machine |
| `hc file write` | write a file |
| `hc file edit` | edit a file |
| `hc put` | place content (scratch/paste) without a shell round trip |
| `hc exec` | run a command and capture its result without a persistent terminal |

### API and clients

- One method table generates everything: params and results in `api/types.rs`
  are also the CLI arguments and the published OpenRPC schema, regenerated with
  `HC_UPDATE_SCHEMA=1 cargo test -p herdr-core --test m0`.
- `hc schema` (or `rpc.discover`) returns the OpenRPC document plus JSON Schema
  for every method; `schema/hc.openrpc.json` is the checked-in copy and
  `schema/released.openrpc.json` is the last released API. A test fails on any
  breaking change, and the API only grows.
- Plain mode is JSON-RPC 2.0, one message per line over a Unix socket (named
  pipe on Windows). `hc connect` carries the protocol over stdin/stdout, so
  `ssh box hc connect` reaches a remote core with no extra daemon.
- The API covers connection modes, terminal streams, event notifications and
  status; errors carry a stable `kind` and a `hint`.
- `hc attach`/viewers support multiple simultaneous viewers; raw bytes are
  relayed to all of them.

### Server lifecycle and maintenance

`hc server start|stop|restart|status`, `hc update` (running terminals keep
running), `hc keep`, `hc kill`, `hc rm`, `hc label`, `hc interrupt`, `hc lt`,
`hc picker`. `just install` installs a checkout and restarts core without
stopping terminals; `just docs` regenerates `docs/commands.md` from the binary's
help and `just docs-check` verifies it and the examples in CI.

### Security and design constraints

- Endpoint paths, permissions, state and update signing are documented in
  `docs/security.md`; Windows uses ACLs on a named pipe, Unix a socket with user
  permissions.
- Linux builds are static; Windows needs `sh` on PATH for templates and quoted
  shell commands.
- Deliberate minimalism: fewer lines, files and dependencies; no abstraction
  before a second real caller; no config for things nobody asked to configure.
- Deciding code is separate from IO so it tests without processes; per-OS
  behaviour lives behind one `platform` interface (`unix.rs`, `windows.rs`);
  TDD with one good test per documented behaviour and a few real-PTY end-to-end
  tests. Source files are kept under ~32 KB (hard cap 48 KB).

## What it deliberately does not have

- No layout, tabs, workspaces, splits or window arrangement.
- No agent brand registry, screen scraping or vendor-specific detection.
- No built-in permission system; it runs with the user's permissions.
- No GUI; the companion repository `herdr-app` is one client of this runtime.

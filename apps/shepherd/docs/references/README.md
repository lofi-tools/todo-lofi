# Reference projects

Feature notes for the checkouts under `.cache/checkout/`, taken from the code and
docs in each checkout. Per-app pages live in [`by-app/`](./by-app/); the desktop
apps are also compared feature by feature in [`by-feature/`](./by-feature/).
Each page records the commit it was read at; a later checkout may have moved on.
[`stack.md`](./by-feature/stack.md) compares what all five are built from: runtimes, UI
toolkits, extension languages, terminal libraries, storage and IPC.

| Reference | What it is | Stack | License |
| --- | --- | --- | --- |
| [herdr](./by-app/herdr.md) | TUI terminal workspace manager for coding agents (Herdr 1) | Rust, ratatui, portable-pty, ghostty-vt | Apache-2.0 |
| [herdr-core](./by-app/herdr-core.md) | Headless terminal/agent runtime (`hc`), draws nothing | Rust, ghostty-vt, OpenRPC | Apache-2.0 |
| [herdr-app](./by-app/herdr-app.md) | Native desktop client of herdr-core (Herdr 2) | Rust, GPUI, QuickJS mods | Apache-2.0 |
| [pandacode](./by-app/pandacode.md) | Tauri desktop workspace driving many coding CLIs | Rust, Tauri 2, SQLite, TypeScript, xterm.js | PandaCode License (MIT + fork credit) |
| [pi](./by-app/pi.md) | Extensible terminal agent harness and its libraries | TypeScript, Node, custom TUI | MIT |

The three herdr repositories are related but separate designs. `herdr` is the
mature single-binary TUI; `herdr-core` and `herdr-app` split the same problem into
a runtime and a window, and herdr-core explicitly does not copy herdr's code
(see `herdr-core/AGENTS.md`). Do not assume shared APIs between them.

For the two desktop apps, [`by-feature/`](./by-feature/) reorganizes the same
material by feature area and separates what both apps share from what only one
has.

## Feature spread at a glance

| Capability | herdr | herdr-core | herdr-app | pandacode | pi |
| --- | --- | --- | --- | --- | --- |
| Persistent terminals after detach | yes | yes | via core | yes | no |
| Terminal layout (spaces/tabs/splits) | yes | no | yes | yes | no |
| Agent status awareness | screen-scrape manifests | OSC 7501 only | via core | harness protocols | none (process-level) |
| Multiple coding CLIs | yes | yes | yes | yes, 15+ | multi-provider models |
| Multi-machine | SSH machines | SSH machines | via core | SSH + cloud | remote env package |
| Agent teams / delegation | panes + CLI | child terminals, `wait` | mods | architect + workers | via extensions |
| Plugin/mod surface | plugins | no | mods (JS) | extensions via docs (skills, rules) | extensions, skills, packages |
| Programmatic API | CLI + socket JSON API | CLI + OpenRPC | CLI + commands | Tauri IPC + headless server | CLI, JSON, RPC, SDK |
| GUI | no (TUI) | no | native desktop | native desktop + mobile | no (TUI) |

# Desktop features, by feature

Two of the reference projects are desktop applications: **herdr-app** (native
GPUI window over herdr-core) and **pandacode** (Tauri 2 app driving installed
coding CLIs). The other references are a TUI (`herdr`), a headless runtime
(`herdr-core`) and a terminal harness (`pi`), so they are out of scope here and
covered in the app pages under [`by-app/`](../by-app/).

Each page covers one feature area with three sections: what both apps have,
what only herdr-app has, and what only pandacode has. Per-app detail lives in
[`herdr-app.md`](../by-app/herdr-app.md) and [`pandacode.md`](../by-app/pandacode.md).
For the runtimes, toolkits, extension languages and libraries behind all five
checkouts, see the layer-by-layer comparison in [`stack.md`](./stack.md); this
section stays on desktop behaviour.

| Feature area | herdr-app | pandacode |
| --- | --- | --- |
| [Layout and navigation](./layout-and-navigation.md) | spaces, tabs, split panes, peek | projects, tasks, inspector, board |
| [Terminals](./terminals.md) | core-owned PTYs, keep/park/pin, graphics | terminal tab, run/preview, cloud shells |
| [Agents and status](./agents-and-status.md) | Working/Blocked/Done/Idle from screen detection | harness protocols, approvals, teams, usage |
| [Work management](./work-management.md) | settle/unsettle, no task records | board, queueing, steering, schedules, goals |
| [Files and previews](./files-and-previews.md) | files/changes/peek mods | diffs, browser, design, simulators, VMs |
| [Git, machines and cloud](./git-machines-and-cloud.md) | worktree mod, SSH machine spaces | full Git/GitHub, 13+ cloud targets, deploys |
| [Customization](./customization.md) | layouts + JS mods for the whole UI | design tokens, VS Code themes, agent skills |
| [Lifecycle and automation](./lifecycle-and-automation.md) | `herdr-app` CLI, mod checks, self-update | headless server, Tauri IPC, self-update |

## The one big difference

Herdr-app is **spatial**: the pane is the unit of work, and the app is a window
over terminals that live in another process. PandaCode is **temporal**: the task
(one thread with one harness) is the unit of work, and panes, diffs, browsers
and simulators are views onto that task. Most of the differences below follow
from that.

## Shared shape

Both are native desktop apps for macOS, Linux and Windows that put several
coding agents side by side, keep the work while you look elsewhere, group work
per repository, support remote machines over SSH, ship their own updater, and
expose a command-line surface so an agent can inspect and drive the window.

# Layout and navigation

How the window is organized and how you move around it.

## Common to both

- One native window with a left sidebar for work and a main area for the active
  thing, plus a resizable secondary panel for inspection.
- Split or arrange several things side by side; focus, zoom and resize each
  region; state persists across restarts.
- Keyboard and mouse are both first-class; every frequent action has a key, and
  keys are configurable.
- A named work grouping per repository (space in herdr-app, project in
  pandacode) with names that survive restarts.
- A launcher for starting new work without hunting through menus.

## herdr-app only

- **Space → tab → pane.** A space is work on one machine, usually one repo; each
  space has tabs, and a tab splits into panes that hold terminals or mod panels.
- An unnamed space follows its first terminal's folder; naming a space or tab
  pins a label. New tabs open in the focused terminal's folder.
- **Peek**: a worker without a pane opens in an overlay over the current tab and
  can be kept as a tab. Live terminal addresses (`t7`, `vps/t7`) in terminal text
  open the terminal they name.
- The layout itself is a document (`layouts/*.json`); shipped Herdr, Terminals and
  Herdr 98 layouts can be cloned and edited, and changes apply live. Everything
  drawn — sidebar, tab bar, menus, dialogs, notifications — is a mod.
- **Command palette** (`ctrl-shift-p`, `cmd-shift-p`) lists every command with its
  key; `ctrl-alt-shift-escape` resets a broken layout or key file to the default
  `main` layout.
- Pane operations: split, close, zoom, focus, go, swap. Tab operations: new,
  close, rename, move, select, next, prev, settle, unsettle.

## pandacode only

- **Project → task → thread.** A task is a conversation with one harness, model
  and reasoning effort; its transcript is the main pane, with the composer below.
- Sidebar sections with their own rules: Pinned, Active (running, needs you,
  limited, unseen failure), Finished, and Settled docked at the bottom. Manual,
  Recent and Created sort orders; per-section caps; filter; roving keyboard
  focus; row actions and undo.
- **Board view** arranges the same tasks as Pending → In progress → Review →
  Completed columns, and the GitHub view replaces the sidebar with repositories.
- **Inspector** with its own tabs: files, changes, browser, terminal, design
  canvas, moodboards, VM, simulator. The main pane is an inset panel with a
  resizable sidebar and inspector; with the sidebar hidden it goes full-bleed.
- **Dock** along the composer with panels (agents/team, subagents) that open in a
  flyout; a quick overlay (global shortcut) launches or jumps to tasks from
  anywhere.
- Menus and dialogs rather than a command palette: a searchable model picker, a
  task-row menu, a deploy menu, Settings.

## Divergence worth knowing

Herdr-app's navigation is spatial — you arrange panes and return to them. Its
sidebar is a status board for agents, not a record of tasks. PandaCode's
navigation is temporal — you move between task threads, and the layout is mostly
fixed shell chrome. Neither app has the other's grouping: herdr-app has no task
list, and pandacode has no concept of an ad-hoc split terminal layout bound to a
space.

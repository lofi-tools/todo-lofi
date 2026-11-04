# Terminals

The terminal is the substrate of both apps: agents are terminal programs, and
running commands is a first-class action.

## Common to both

- Real PTY-backed terminals rendered natively inside the app, with scrollback,
  selection, copy and paste.
- Search within a terminal.
- Image handling: pasting images and rendering terminal graphics.
- Terminal work happens in a runtime separate from the window, so drawing is not
  blocked by process output and work can continue while you look elsewhere.
- Open a terminal in the current repository without leaving the app, and keep it
  beside agent output.

## herdr-app only

- Terminals are owned by **herdr-core** (`hc`), a separate binary. Closing the
  window leaves them running; `hc` commands can start, read, send to, attach to
  and wait on any of them, and other clients can attach to the same terminal.
- Per-terminal commands: open, keep, park, pin, kill, peek, show, scroll, copy,
  paste, paste image, search, right-click.
- **Keep vs park**: kept panes get fresh shells in their last folders after a
  reboot or core crash (the old program is not restored); parking marks a
  terminal as not currently needed.
- Kitty graphics with a sprite layer, clipboard forwarding, bracketed paste,
  clipboard images and terminal notifications.
- Terminal addresses are links: click `t7` or `vps/t7` in any terminal to jump to
  that terminal, local or remote, including as a peek overlay.
- The app also mirrors remote terminal screens and can carry a live terminal
  inside a mod block.

## pandacode only

- A terminal is one tab of the inspector, scoped to the task's project and
  worktree, rather than a pane that defines the layout.
- **Run and preview** a project from inside the app, with the dev server output
  and a preview URL exposed next to the agent (`docs/run-project.md`).
- Shells inside local VMs and cloud sandboxes are driven by the app's tools
  (`pandacode_vm_*`, cloud CLI) and appear in the VM panel rather than as regular
  panes.
- Agent CLIs get approvals and questions rendered as native UI instead of a
  terminal prompt, so the terminal is for ad-hoc commands rather than the primary
  agent surface.
- Terminals are owned by the Rust backend; cloud agents and the headless server
  carry work past a closed window, which is a different persistence boundary
  from a per-pane keep/park model.

## Divergence worth knowing

Herdr-app treats a terminal as an object with an address you can delegate to,
attach to from elsewhere and link to. PandaCode treats a terminal as one tool in
a task workspace. If Shepherd wants agents to hand work to each other through
terminals, herdr-app's model is the closer precedent; if it wants terminals to be
incidental to task review, pandacode's is.

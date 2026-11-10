# Shepherd — goal

Shepherd is a local-first desktop app for running several coding agents at once
in your own repositories. It multiplexes agents and terminals around persistent
projects and treats reviewing what an agent produced — files, diffs, commits — as
part of the same window. It is deliberately one app: a single Rust core owns
every agent, terminal and database write, and quitting the app ends the work.

Target: `apps/shepherd`, macOS 13+ and Ubuntu 24.04+ first. v1 is a Tauri 2 shell with a
SolidJS + TypeScript frontend over a shared Rust core; v2 replaces the shell with
GPUI without touching that core, is planned but unscheduled, and starts when v1 is
feature complete. The stack is specified in
[stack.md](./stack.md); behaviour in [master-spec.md](./master-spec.md), which
also records the interview decisions.

## Problem

Coding agents are terminal programs (Claude Code, Codex, Pi, OpenCode, Freebuff,
and whatever ships next month), and each speaks a different machine interface, if
any. A developer running more than one juggles terminals, loses track of which
agent is waiting, and reviews the result in a separate Git client. Existing tools
pick a side:

- Terminal multiplexers (`herdr`, `herdr-app`) own terminals and panes well but
  keep no task record and no review surface.
- Agent workbenches (`pandacode`) own tasks, worktrees and native harness
  protocols, but bind deeply to each harness and add cloud, bots and deploy
  machinery Shepherd does not need.
- Agent harnesses (`pi`) own the loop but are single-agent and terminal-only.

Nothing in the reference set is both terminal-first and project-first, treats
agent protocols as adapters rather than identity, and puts review in the same
window as the work.

## Vision

One window where every project is always there, every running agent is visible
with an honest state, and every task that changes code ends with a review you can
act on — without Shepherd becoming an agent itself, and without pretending that
closing the app keeps working.

## Principles

1. **Adapter-first, not single-protocol.** Use an agent's native protocol where it
   buys real capability (Codex, OpenCode, Pi RPC), fall back to ACP for the long
   tail, and use a plain terminal for Freebuff. The adapter tier is visible
   where Shepherd integrates an agent.
2. **One app, honest lifecycle.** The Rust core owns agents, PTYs and state; the
   shell only draws and sends input. Closing the window is quitting, it warns when
   work is live, and reopening never claims that killed work survived.
3. **Terminals are first-class, not a fallback.** Every task workspace has a chat
   anchor and a project terminal tab with shared and task-specific terminals, and
   every agent can be read and steered in one even when no adapter exists.
4. **Projects are persistent, tasks are reviewable.** A project is a local
   directory, usually a Git repo, remembered across launches. A task is a
   conversation in its own worktree plus the changes it produced.
5. **Local-first and quiet.** An embedded SQLite-compatible database on disk, no
   account, no telemetry, no network calls beyond the agents themselves.
6. **Nothing hidden.** The exact command, working directory, adapter tier and exit
   status are visible; an approval is shown with who decided it and why.
7. **Keyboard-first, mouse-complete.** Every action is a command with a default
   binding and a palette entry.

## Scope

### Required for v1

- **Persistent projects**: add a local directory, remember path, Git metadata and
  its last active task workspace; handle moved or deleted directories honestly.
- **Task workspace** per task: one required chat, a project-configured Terminals
  tab with shared and task-specific terminals, preview/annotate, review, Git,
  files, and an always-on inspector with fixed regions and terminal layout
  presets.
- **Native adapters** for Codex (first), OpenCode, then Pi over its RPC mode.
  **ACP** supports Claude Code and other ACP agents. Freebuff runs directly in a
  standalone terminal in the conversation region. A separate
  [integrated Freebuff adapter](./freebuff-adapter.md) is planned alongside it.
- **Task isolation**: one Git worktree per task by default, under the repository's
  resolved Git directory, with a warned shared checkout as the exception.
- **Task management**: parallel tasks with visible state (working, waiting on
  you, needs review, done, failed, interrupted), queueing, steering, cancel,
  review notes, archive, resume after a restart.
- **Terminal multiplexing**: per-project terminal arrangements, splits,
  scrollback, copy/paste, search, shared terminals that survive task switches,
  and task terminals that survive closing their view or switching tasks.
- **Files view**: project tree, filter/search, read-only viewer, changed-file
  marks, open in external editor.
- **Git view**: status, staged/unstaged diffs, stage/unstage, commit in-app with
  the project's own hooks and signing, branch display and checkout, and
  squash-merge into the base.
- **Preview and annotation**: load the project's preview URL, select an element,
  attach a note, and add the annotation to the prompt.
- **Chat surface**: streaming transcript with history, attachments and images,
  `@file` mentions, slash commands, advertised model/mode pickers, cross-task
  search, notifications, themes, rebindable keys and a command palette.
- **Approvals**: safe reads auto-allowed, everything else judged by the agent's
  own review mode, falling back to asking the user — never a silent allow.
- **Quit**: a named confirmation when a task is working or waiting, a bounded
  shutdown, and an interrupted state that resumes next launch.

### Later (designed for, not built in v1)

- Remote projects over SSH, MCP servers, image/PDF preview.
- Export/share, usage and cost display, memory/instructions.
- Agent teams, schedules, Windows support.
- Pull requests and in-app conflict resolution.

### Not Shepherd's job

- No model provider, no hosted sandbox, no deploy pipeline, no mobile app, no
  bot channels, no telemetry, no account system.
- No bundled agent. Shepherd shells out to installed CLIs.
- No daemon, background service or second window.
- No editing environment: files are viewable and diffs reviewable; Shepherd is
  not a code editor, LSP host or debugger.
- No programmable UI or plugin API.

## Success criteria

- A new user adds a repo, starts a Codex task in its own worktree, sees streaming
  output with tool rows, answers an approval, and lands on a reviewable diff in
  under two minutes.
- Quitting with a working agent names what will stop, stops it within two seconds,
  and the next launch offers resume rather than showing a task that silently died.
- Three agents across two projects run concurrently with correct states and no
  polling, each in its own worktree, with no cross-task file collisions.
- Committing from the Git tab produces exactly the commit `git commit` would have
  produced there (hooks, signing, author, message).
- Freebuff opens in a standalone terminal in place of the conversation pane.
  Input reaches its TUI through the normal PTY path.
- A failed adapter probe explains the reason and offers the next tier down.
- Window cold start under 1.2 s; streaming does not drop frames while a large diff
  is rendered.

## Relationship to the existing workspace

| Existing piece | Use in Shepherd |
| --- | --- |
| `libs/acp-client` | ACP connection, launching, permissions, fs and terminal client capabilities; no GPUI dependency |
| `libs/gpui-tokio` | v2 shell bridge from the Tokio core to GPUI; v1 uses Tauri channels |
| `libs/storage` | Turso driver wiring, embedded migration conventions and error types (Shepherd gets its own database and migrations) |
| `apps/agent-cli` | Reference ACP-capable agent; local agent to test against |
| `docs/references/` | Feature and architecture evidence from `herdr`, `herdr-core`, `herdr-app`, `pandacode` and `pi` |

## Milestones

| Milestone | Outcome |
| --- | --- |
| M0 | Window shell, project registry, schema, palette, status bar, quit flow |
| M1 | Fixed task workspace, project terminal layout, worktree per task |
| M2 | Codex native adapter: streaming transcript, approvals, cancel, resume |
| M3 | OpenCode adapter, ACP path for Claude Code, standalone Freebuff terminal |
| M4 | Files, Git and preview panes, in-app commit |
| M5 | Pi RPC adapter, attachments, mentions, model/mode pickers, cross-task search, notifications |

Milestone acceptance criteria live in the master spec.

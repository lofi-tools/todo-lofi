# Agents and status

Both apps exist to run coding agents and tell you which one needs you.

## Common to both

- Several agents run at once, in parallel, across projects and machines.
- A list of agents with a status word, clickable to focus the right surface.
- Status is derived from the agent, not typed in by the user; the app surfaces
  "working", "waiting for you" and "finished while you were away".
- Agents that finish out of sight are marked so you can come back to them.
- Notification when an agent blocks or ends, so you do not poll.

## herdr-app only

- Four states with fixed meanings: **Working**, **Blocked** (waiting for you),
  **Done** (finished while you were looking elsewhere), **Idle** (not working, or
  a finished agent you have looked at).
- Status comes from herdr-core: agent **detection manifests** that inspect the
  screen for each supported CLI, plus agent-side integrations (Claude Code,
  Codex, OpenCode) that report turn boundaries. The app itself does not speak any
  harness protocol.
- The agents sidebar aggregates agents from every space and machine, including
  workers started by another agent; a worker without a pane opens in peek.
- `agent.focus`, `agent.next`, `agent.prev`; Ctrl/Cmd-click a live terminal
  address to peek at it.
- Done/idle is per client and per seen state, so two windows can disagree about a
  finished agent.

## pandacode only

- Status comes from each **harness's own protocol** (native CLI, ACP, app-server,
  RPC, MSP): streaming replies, tool activity, subagent activity, approvals,
  questions and usage limits are parsed from the harness rather than the screen.
- **Approvals bar** above the prompt: one line per request, grouped with
  **Approve all** when several are pending; approval modes include agent-reviewed
  approvals and plan-mode investigation.
- **Rich questions**: harness questions render Markdown, workspace images, charts
  and sanitized HTML/SVG rather than plain text.
- **Agent teams**: an architect plus up to three workers mixing harnesses and
  models, named team profiles, and automatic teams (Cheap/Balanced/Best) where a
  local router picks each worker's model. Spawned workers are links in the turn.
- Subagent and machine-thread views, plus a dock badge counting running workers,
  subagents and machine threads.
- **Agent usage**: account limits and cost estimates per installed harness, with
  a provider price list.
- Harnesses can be switched from the composer, with reasoning effort per task.

## Divergence worth knowing

Herdr-app infers state from the outside (screen plus optional integrations),
which makes it work for any CLI without protocol support but limits it to coarse
states. PandaCode requires a protocol adapter per harness, which is more work per
agent but yields structured events, approvals, questions, teams and cost. A
system that wants both should keep the coarse state as a fallback and use
protocol events where available.

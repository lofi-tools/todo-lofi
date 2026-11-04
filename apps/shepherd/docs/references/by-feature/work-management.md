# Work management

How each app records and organizes the work itself, as distinct from the panes
that show it.

## Common to both

- Work is grouped per repository (space or project) and the grouping survives a
  restart, including spaces and tabs on remote machines.
- Parallel work streams are normal; neither app assumes one agent at a time.
- A way to park work without deleting it, and to find it again later.

## herdr-app only

- There is no task record, no title, no queue and no history: the unit of work is
  a pane and its terminal. The core does not persist conversations, only the
  layout, terminal metadata and (for supported agents) the session needed to
  resume.
- **Settle / unsettle** a tab marks it as parked and out of the way; that is the
  whole lifecycle.
- Workspaces and worktree groups are the organizing structure; a worktree can be
  opened as its own space.
- Because nothing is recorded, "what did I ask this agent" lives in the terminal
  scrollback and the agent's own session, not in the app.

## pandacode only

- **Tasks are records in local SQLite**: title, project, branch, harness, model,
  stage, unread state, linked issue or PR, linked worktree or cloud machine, and
  the full transcript. A task can be reopened after being settled.
- **Task board** with Pending → In progress → Review → Completed columns; the
  sidebar has its own four sections (Pinned, Active, Finished, Settled) over the
  same records.
- **Message queueing and steering** while an agent runs: send follow-ups that
  land in order, or redirect the current turn.
- **Rewind**: edit a sent prompt and re-run from that point; saved **turn
  changes** record what each turn touched.
- **`/btw` side threads** for a question that should not derail the task.
- **Schedules** fire tasks on a timer, with a trace format and agent tools.
- **Goals** and harness command workflows attach structured objectives and
  repeatable command sequences to a task.
- **Workflow recipes** and slash commands for repeatable prompts.
- **Token saver** packs change response style per task to cut output tokens.
- **Conversation titles and activity** summarize what a thread is about and
  whether it is still moving.

## Divergence worth knowing

PandaCode's work management is its largest subsystem and the main reason to
choose it over a multiplexer: durable tasks, review stages and schedules.
Herdr-app deliberately has none of that, which keeps the core small and makes the
terminal the only source of truth. If Shepherd records tasks, it needs a
database, stages and a retention story; if it does not, the terminal must be good
enough to be the record.

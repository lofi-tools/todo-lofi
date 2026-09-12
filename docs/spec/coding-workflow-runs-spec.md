# Coding Workflow Runs (AI-assisted feature development) — Spec

Status: Implemented (see §19 for the parts that are still open)
Supersedes/relates to: `docs/spec/workflow-engine-spec.md` (v1 recipe format),
`docs/spec/workflow-simplification-spec.md` (v2 data model, implemented),
`docs/spec/agent-cli-interview-feature-spec.md` (`/interview`, `ask_user`),
`docs/spec/todo2-agent-pane-ui-spec.md` (agent pane UI), and
`docs/spec/acp-client-and-agent-panel-spec.md`.

## 1. Intent

Give a coding task (a title + description) a guided, AI-assisted workflow:

```
title + description
  → /interview  (spec the feature out)
  → approve spec
  → implement on a feature branch
  → review / annotate            ──┐
  → re-spec + re-implement         │ repeat as many times as needed
  → completed (merge branch)     ←─┘
```

Big features can be split into sub-tasks by the model. Sub-tasks that are
under-specified can request their own interview round ("sub-task
re-interview"), which the user launches manually from the UI.

The primary question this spec answers: **does this fit the existing workflow
engine and data model, and what must be extended?** The primary UX question:
**how do the details panel and the agent panel carry the user through the
phases?**

## 2. Interview decisions (normative)

| # | Topic | Decision |
| --- | --- | --- |
| 1 | Data model | **Reuse the workflow engine.** A built-in `coding-task` recipe whose nodes are the phases, reusing `workflow_recipes` / `workflow_runs` / `tasks`. The recipe node schema is extended (see §6). |
| 2 | Run identity | **One feature task is the run root.** The existing task gets `workflow_run_id`; phase steps materialize as its subtasks. |
| 3 | Driving | **User-driven.** The user clicks each phase action in the details panel; the agent works when asked. |
| 4 | Execution | **The existing per-project ACP session** in the agent pane — the transcript is visible and the user can type into it. No headless/background agent runs in v1. |
| 5 | Tool transport | **todo-2 hosts a local MCP server** and passes it to the agent launch (the `mcp_servers` the ACP client already sends, currently ignored server-side). |
| 6 | Agent capabilities | Save/attach spec, create sub-tasks, request sub-task interview, annotate/append notes, propose branch + summary. |
| 7 | Sub-task shape | Sub-tasks are **subtasks of the feature task** (`parent_id`); the model may make a complex sub-task a **nested run**. |
| 8 | Sub-task re-interview | The app creates an **interview step** for that sub-task; the **user manually launches** it; the model asks questions; the user answers; the interview step **auto-completes when the user answers**. |
| 9 | Phase UI | A **phase stepper at the top of the details panel** for the run root (and nested-run roots). |
| 10 | Spec artifact | **Stored on the task** (new `tasks.spec` + `tasks.spec_path`), rendered inline in the details panel. The agent still writes the `docs/spec/<slug>-spec.md` file as part of `/interview`; the task field is the app's source of truth and the path is an "open file" affordance. |
| 11 | Review loop | **Annotate + Reject**, reusing the engine's approval + retrigger machinery (generalized to name the node to re-open). |
| 12 | Git | **The app manages git**: branch created once the spec is approved, merge performed on the Completed step. |
| 13 | Phase prompt | **The app composes it**: a per-phase template (feature context + spec + annotations) inserted into the agent pane's prompt box for the user to edit and send. |
| 14 | Annotation persistence | **One ordered log, all cycles**, stored in `workflow_runs.step_results` under a reserved `"@notes"` key. |
| 15 | Branch name | **The agent proposes it during the spec phase**; the app creates the branch once the spec is approved. |
| 16 | Failure UX | **Block the phase with an inline reason** and a fix affordance (Configure agent / Set project directory / Resolve conflict). |
| 17 | Recipe availability | **Seeded built-in** (idempotently ensured), available to every project. |
| 18 | Phase completion | **Mixed**: interview and spec auto-complete from agent signals; implement requires a user confirmation; review requires an explicit approve/reject. |
| 19 | Concurrency | **One active coding run per project**; nested runs are exempt; no cycle counter in v1. |
| 20 | MCP gating | The MCP server is **always attached** to the agent launch; tools take explicit task/run ids. |
| 21 | Cancel | Tombstone the steps but **keep the run visible with its branch name** so the branch is cleaned up deliberately. |
| 22 | No diff surface | **No diff component in v1.** Review is the agent transcript plus a free-text annotation box. |
| 23 | Stepper scope | The stepper renders on the **run root and on nested-run roots**; plain sub-tasks get a badge/link. |
| 24 | Stepper content | Phase status + actions, the round/annotation log, the sub-task list, and branch + spec artifacts — **all four**. |
| 25 | `ask_user` bridge | **Typed ACP Elicitation**, advertised and handled by `libs/acp-client`, rendered as a question card in the pane; the agent emits the SDK's elicitation schema (§17.1). |
| 26 | Pane agent for coding runs | **`agent-cli --acp`**, selectable per project, because the interview contract lives there; `opencode` stays the default for general chat. |

Out of scope for v1 (explicit): unattended/background runs, multi-agent
fan-out, remote PR integration.

## 3. Current state (verified in this repo)

### 3.1 Workflow engine — `libs/storage/src/workflow.rs`

Implemented per the v2 simplification spec:

- `workflow_recipes` (`id`, `slug`, `version`, `recipe_json`, `created_at`),
  immutable and versioned; runs pin an exact row.
- `workflow_runs` (`id`, `recipe_id`, `schedule_id`, `status`
  `active|completed|cancelled`, `params`, `step_results`, `created_at`,
  `completed_at`).
- Steps are ordinary `tasks` rows with `tasks.workflow_run_id` + `tasks.node_id`;
  waits use `blocked_until`; dependencies use `task_links` (`blocked_by`);
  cancellation tombstones rows.
- `parse_recipe` validates: node `kind` ∈ {`action`, `event`}, node id
  `[A-Za-z0-9_-]+`, edge `condition_type` ∈ {`on_complete`, `on_result`,
  `timer`, `event`}, event nodes need event-typed incoming + `on_complete`
  outgoing edges, `approval` nodes must be fed by an `on_result` edge from an
  `ai: true` node, `retrigger_on_reject` requires `approval`, and the graph must
  be an **acyclic DAG** (Kahn check).
- `RecipeNode { id, kind, title, description, ai, approval, retrigger_on_reject }`.
- Engine methods: `create_run`, `create_managed_run`, `complete_workflow_step`,
  `evaluate_workflow_completion`, `trigger_event`, `cancel_run`,
  `cancel_active_runs`, `list_active_run_views`, `workflow_run_view`,
  `check_run_complete`, `spawn_node_task`, `retry_task`, `target_spawnable`,
  `node_done_task_id`, `node_result`, `resolve_params`.
- Existing loop support: `retry_task` re-opens the **parent** of a rejecting
  approval task (`done=false`, clears `step_results[node_id]`) and tombstones
  its pending children.
- `workflow_run_view` returns `None` for cancelled runs;
  `list_active_run_views` skips managed-tag recipes (travel).
- Sync guard is implemented: `libs/storage/src/todoist.rs:554` skips
  `task.is_seed || task.workflow_run_id.is_some()`.

### 3.2 Workflow UI — `apps/todo-2/src/ui_parts/workflows.rs`

`WorkflowPanel` renders a "Workflows" header with a `▶ <recipe>` start button
per recipe (managed-tag recipes filtered out), then one card per active run:
recipe name, status, Cancel, and per-step rows with `Complete`, `Approve` /
`Reject` (from `on_result` values), or `Fire: <event>` for event waiters.
Emits `WorkflowPanelEvent::Changed`; `main.rs` reloads the task list on it.

### 3.3 Task details — `apps/todo-2/src/ui_parts/task_details.rs`

`TaskDetails` renders the selected task: title/description editing, subtasks
(inline `+ subtasks` input), follow-up tasks, blockers, "after" links,
"Linked to", repeat, "blocked until". Events: `Toggled`, `TitleCommitted`,
`PendingConfirmed`, `PendingCancelled`, `SelectTask`, `TaskRefreshed`,
`SubtaskCreated`, `FollowUpCreated`. No workflow awareness today.

### 3.4 Agent pane — `apps/todo-2/src/ui_parts/agent_pane.rs`

One ACP session per directory-backed project (`ProjectEntry`): transcript,
busy flag, prompt queue, tool-permission cards, terminals, slash-command
dropdown fed by the agent's `available_commands`. Exposes
`insert_prompt_text(context, window, cx)` and `build_task_context(task)`
(`"Task: …\n\n<description>\n\nTags: …"`), emits `BusyChanged` and
`AttachTaskRequested`. `main.rs` `Layout` owns `right_pane: RightPane
{ Details | Agent }` and `agent_available` (true for directory-backed tags).

### 3.5 Agent side — `apps/agent-cli`, `libs/acp-client`

- `/interview` exists: `apps/agent-cli/src/interview.rs::INTERVIEW_BASE_PROMPT`
  + `build_interview_prompt`, writing `./docs/spec/<slug>-spec.md`.
- `ask_user` exists as a first-class tool (`apps/agent-cli/src/tools.rs`
  `AskUserTool`) with a TUI channel bridge.
- Sub-agents (`spawn_agents`) include `code-reviewer`, and
  `suggest_followups` exists (`apps/agent-cli/src/subagents.rs`).
- MCP: `cersei::mcp` exists; `agent-cli` has `config.mcp_servers`; `acp.rs`
  parses client-supplied `mcp_servers` into `_mcp_servers` and **ignores them**;
  `ToolContext.mcp_manager` is always `None` today.
- **Gap (verified):** `libs/acp-client` neither advertises `elicitation` in its
  `ClientCapabilities` nor registers an `elicitation/create` handler, so
  `acp.rs` leaves `ask_user` **unregistered** for the pane's sessions
  (`ask_user_tool: None`, acp.rs:1407) and interview questions never reach the
  user. Additionally the pane hardcodes `Arc::new(OpenCodeAgent)`
  (agent_pane.rs:339) → `opencode acp`, while the interview contract
  (`/interview`, `ask_user`, `interview/*`) lives in **agent-cli**. Resolved in
  §17.1.

## 4. Does it fit? — fit analysis

**Fits cleanly (reuse as-is):**

| Need | Existing mechanism |
| --- | --- |
| A run with versioned, authored phase definitions | `workflow_recipes` + `parse_recipe` |
| Run state, params, per-step results | `workflow_runs` |
| Phase steps | `tasks` with `workflow_run_id` / `node_id` |
| Phases as subtasks of the feature task | `tasks.parent_id` |
| Gating a step on the previous phase | `on_complete` / `on_result` edges + lazy spawn |
| Human approval gate | `approval: true` node + `on_result` edges |
| Reject → do it again | `retrigger_on_reject` + `retry_task` |
| "Approve spec → implement" | `on_result` condition `{}` then `{"approved": true}` |
| Run completes / cancel | `check_run_complete`, `cancel_run` |
| Workflow tasks never sync to Todoist | existing sync guard |
| Agent runs per project with a visible transcript | `AgentPane` + ACP session |
| Slash commands surfaced in the pane | `available_commands` dropdown |

**Must be extended:**

1. Recipe nodes have no notion of a **phase** — add `RecipeNode.phase`.
2. Nodes always materialize as top-level tasks or approval subtasks — coding
   phases must materialize **as subtasks of the run root** — add
   `RecipeNode.subtask` + `WorkflowRun.root_task_id`.
3. The reject loop can only re-open the approval's **parent** — spec and review
   both need to re-open the `interview` node — add `RecipeNode.retrigger_node`.
4. Runs have nowhere to store the **branch** (and it must survive cancellation)
   — add `workflow_runs.branch` / `base_branch` / `branch_status`.
5. The **spec** is not storable on a task — add `tasks.spec` + `tasks.spec_path`.
6. The **annotation log** needs ordered history — use a reserved
   `step_results["@notes"]` array (node ids cannot contain `@`, so no clash),
   and make `retry_task` preserve it.
7. Runs are created from the Workflows panel with default params; a coding run
   must attach to an **existing task** — add `create_task_run`.
8. Cancelled runs vanish (`workflow_run_view` → `None`, panel lists active
   only) — a cancelled run with a branch must stay visible for cleanup.
9. `tasks` select lists are **positional** (`task.rs:228`, `:454`, `:589`,
   `workflow.rs` raw SELECTs): new columns must be appended in one place and
   every positional index updated.

## 5. The coding recipe

### 5.1 `coding-task` (seeded built-in, slug `coding-task`)

```json
{
  "name": "Coding task",
  "description": "AI-assisted feature development: interview, spec, implement, review, merge.",
  "params": {
    "branch": { "type": "string", "default": "" }
  },
  "nodes": [
    { "id": "interview", "kind": "action", "title": "Interview & spec the feature",
      "ai": true, "phase": "interview", "subtask": true,
      "description": "Run /interview with the feature title and description, ask clarifying questions in rounds, then save the spec." },
    { "id": "spec", "kind": "action", "title": "Approve the spec",
      "phase": "spec", "subtask": true, "approval": true,
      "retrigger_on_reject": true, "retrigger_node": "interview",
      "description": "Read the spec. Approve to start implementation, or reject with notes to re-interview." },
    { "id": "implement", "kind": "action", "title": "Implement the feature",
      "ai": true, "phase": "implement", "subtask": true,
      "description": "Implement the approved spec on the feature branch, then confirm." },
    { "id": "review", "kind": "action", "title": "Review & annotate",
      "ai": true, "phase": "review", "subtask": true, "approval": true,
      "retrigger_on_reject": true, "retrigger_node": "interview",
      "description": "Review the implementation, annotate findings, then approve to merge or reject to re-spec." },
    { "id": "merge", "kind": "action", "title": "Merge the branch",
      "phase": "merge", "subtask": true,
      "description": "Merge the feature branch into its base branch and mark the feature complete." }
  ],
  "edges": [
    { "from": "interview", "to": "spec", "condition_type": "on_result", "condition_value": {} },
    { "from": "spec", "to": "implement", "condition_type": "on_result", "condition_value": { "approved": true } },
    { "from": "implement", "to": "review", "condition_type": "on_result", "condition_value": {} },
    { "from": "review", "to": "merge", "condition_type": "on_result", "condition_value": { "approved": true } }
  ]
}
```

Validation notes: every `approval` node is fed by an `on_result` edge from an
`ai: true` node ✔; the graph is a DAG ✔; the re-spec loop lives in
`retrigger_node`, not in the edges ✔.

### 5.2 Phase machine

| Phase | Step | Who completes it | Effect |
| --- | --- | --- | --- |
| `interview` | "Interview & spec the feature" | Agent signal (`save_spec`) | Spec stored on the root task + `@notes` entry; `spec` step spawns (reject resets this) |
| `spec` | "Approve the spec" | User approve, or reject with notes | Approve → app creates the branch (`branch`, `base_branch`) and `implement` spawns. Reject → annotations logged, `retrigger_node` re-opens `interview` |
| `implement` | "Implement the feature" | User confirmation (agent signal surfaces as "Agent reports done") | `review` spawns as a subtask of the implement step |
| `review` | "Review & annotate" | User approve / reject | Approve → `merge` spawns. Reject → annotations logged, `interview` re-opens (new round) |
| `merge` | "Merge the branch" | User clicks Merge; app runs git | Branch merged, `branch_status="merged"`; merge step **and** root feature task complete → run completes |

Round counter for the UI = number of `spec` steps in the run.

### 5.3 Reject → re-spec → re-implement (the loop)

`review` rejects with `{"approved": false, "notes": "…"}`. The engine:

1. Records the rejection in `@notes` (`kind: "reject"`, phase, body, timestamp).
2. Follows `retrigger_node: "interview"`: finds the latest done `interview`
   task of the run, re-opens it (`done=false`, clears
   `step_results["interview"]`), and tombstones its pending children.
3. When the agent saves the spec again, `interview` completes → the `on_result`
   edge with `{}` spawns a **new** `spec` step (the previous cycle's steps stay
   as history: done, struck through).
4. Approve → a new `implement` step, then a new `review` step. Repeat.

`retrigger_node` defaults to the current behaviour (the `approval` task's
`parent_id`) so existing recipes — including the Case 4 gatekeeper tests — are
unaffected.

## 6. Data model changes

### 6.1 Migration `0017_coding_workflow.sql`

```sql
ALTER TABLE workflow_runs ADD COLUMN root_task_id INTEGER;
ALTER TABLE workflow_runs ADD COLUMN branch TEXT;
ALTER TABLE workflow_runs ADD COLUMN base_branch TEXT;
ALTER TABLE workflow_runs ADD COLUMN branch_status TEXT;   -- active | merged | abandoned
ALTER TABLE tasks ADD COLUMN spec TEXT;
ALTER TABLE tasks ADD COLUMN spec_path TEXT;
CREATE INDEX idx_workflow_runs_root_task ON workflow_runs(root_task_id);
```

`recipe_json` changes need no migration (recipes are JSON and versioned):
immutable `coding-task` v1 is inserted, and any later change becomes v2.

### 6.2 Engine struct changes

```rust
pub struct RecipeNode {
    // …existing…
    pub phase: Option<String>,          // "interview" | "spec" | "implement" | "review" | "merge"
    pub subtask: bool,                  // materialize under the run's root task
    pub retrigger_node: Option<String>, // re-open this node on reject (default: the feeder)
}
```

`parse_recipe` additions:

- `phase` must be one of the five values (unknown → user-facing error).
- `subtask: true` is only valid on action nodes.
- `retrigger_node` references an existing `ai: true` node, is not the node
  itself, and requires `retrigger_on_reject`.

`WorkflowRun` gains `root_task_id`, `branch`, `base_branch`, `branch_status`
(and its positional `parse_run_row` + all raw SELECT column lists updated).

### 6.3 `@notes` — annotation log (reserved `step_results` key)

```json
{
  "interview": { },
  "implement": { },
  "@notes": [
    { "at": 1757600000, "phase": "review", "node_id": "implement",
      "kind": "annotation", "body": "Guard the empty-input case." },
    { "at": 1757600120, "phase": "review", "node_id": "review",
      "kind": "reject", "body": "Needs a migration test." }
  ]
}
```

`kind` ∈ `annotation | reject | approve | spec | branch | merge`. Entries are
append-only. `retry_task` must preserve `@notes` while clearing
`step_results[node_id]`. The details panel renders this log newest-first,
grouped by cycle. (This is why the log lives on the run and not on the step
result: it must survive the retrigger that clears the step result.)

### 6.4 `create_task_run` (new engine entry point)

```rust
pub async fn create_task_run(
    &mut self,
    task_id: u64,
    recipe_id: u64,
    params: Value,
) -> QueryResult<WorkflowRun>;
```

1. Validate the recipe and params (reuse `insert_run_row`).
2. Reject if the task already has a non-`cancelled` coding run, or if the
   project (task's direct tag → `tag_settings.dirs`) already has an active
   top-level coding run (`Invalid` error with a user-facing message).
3. Insert the run row with `root_task_id = task_id`.
4. Set `tasks.workflow_run_id` on the root task (its `node_id` stays `NULL`, so
   the engine ignores it in `evaluate_step_completion` and it does not render
   as a step; it is what keeps `check_run_complete` pending until merge).
5. Materialize start nodes (`interview`) with
   `parent_id = root_task_id` because `subtask: true`.

### 6.5 `spawn_node_task` change

When `node.subtask` is true and `run.root_task_id` is `Some(root)`, the spawned
task's `parent_id` is `root` (approval nodes keep parenting to their feeder as
today, which is already a subtask of the root).

### 6.6 `workflow_run_view` / visibility changes

- Return the view for `cancelled` runs when `branch` is `Some` (so the branch
  is visible and cleanable).
- Add `list_coding_runs(&mut self) -> Vec<RunView>`-style query (or include
  coding runs in `list_active_run_views` and add a `branches_to_clean_up()`
  query). The Workflows panel renders a **"Branches to clean up"** section from
  it.
- Add `coding_run_for_task(task_id) -> Option<RunView>` (root or nested root,
  including completed/cancelled states) for the details stepper.

## 7. MCP tool surface

### 7.1 Transport and wiring

- todo-2 runs one **loopback MCP server** (streamable HTTP, bound to
  `127.0.0.1:0`, ephemeral port) for the whole app, in-process, sharing the same
  `Store` handle as the UI. A loopback-only bearer/`X-Token` header mirrors the
  webhook pattern from the v1 spec.
- The pane passes the resolved URL + token to the agent. This uses the ACP
  `mcpServers` field the client already sends (`apps/agent-cli/src/acp.rs`,
  currently parsed into the ignored `_mcp_servers`).
- Required agent-side change: build a real `cersei::mcp::McpManager` from the
  client-supplied servers into `ToolContext.mcp_manager` instead of `None`.
- Attached **always** (decision #20). Tool calls carry explicit `task_id` /
  `run_id`, and validate that a coding run is active for that id, returning a
  clear tool error otherwise.
- Permission policy: read-only tools (`get_coding_context`) allowed; mutating
  tools surface through the existing per-project tool-permission card
  (`libs/acp-client/src/permissions.rs`), whose "Always allow" writes a rule
  keyed by the tool name.

### 7.2 Tools

| Tool | Kind | Input | Effect |
| --- | --- | --- | --- |
| `get_coding_context` | read | `task_id?`, `run_id?` | Returns phase, spec, cycle, branch, annotation log, sub-tasks, and the phase prompt hints. |
| `save_spec` | write | `task_id`, `path`, `content` | Stores `tasks.spec` / `tasks.spec_path`, appends a `spec` note, **completes the `interview` step** (agent signal → mixed auto-advance). |
| `create_sub_task` | write | `parent_task_id`, `title`, `description`, `nested: bool` | Creates a subtask under the given step (or the feature task); when `nested` is true also starts a child `coding-task` run rooted at the subtask (exempt from the per-project guard). |
| `request_sub_task_interview` | write | `sub_task_id`, `reason?` | Creates an interview step for the sub-task (a `coding-sub-interview` run rooted at the sub-task) and flags it "needs input" in the UI. |
| `append_note` | write | `task_id`, `kind`, `body` | Appends an `@notes` entry (annotation, finding, decision). |
| `propose_branch` | write | `task_id`, `name`, `summary?` | Records the proposed branch name on the run; the app creates the branch after spec approval. |
| `complete_phase` | write | `task_id`, `phase`, `summary?` | Agent-side completion signal for the current phase: auto-completes `interview`; for `implement` shows "Agent reports done" (user still confirms); ignored for `review`/`merge`. |
| `propose_summary` | write | `task_id`, `commit_message`, `pr_summary?` | Records a merge/commit summary for the merge step (PR creation is out of scope). |

### 7.3 Guard behaviour

Errors are returned as tool errors (never panic): unknown/absent run, phase
mismatch (e.g. `save_spec` while the run is in `implement` after a merge),
branch already taken, project has no directory, git command failure (with
stderr text).

## 8. UX — details panel (the steps, as subtasks)

There is **no "Coding workflow" header and no separate phase stepper**. For the
selected task, when `coding_run_for_task(task_id)` is `Some`, the bottom of the
details panel — after the ordinary fields and the "Linked to" lists — renders
The run's steps as the subtasks they are: one row per phase in run order, and
the model's sub-tasks of a step nested under that step's row.

```
… ordinary fields, Linked to, Subtasks (2) …

  ☑ Interview & spec the feature                        done
  ☑ Approve the spec                                    done
  ◐ Implement the feature       [ Start implementation ]      ← highlighted
      ☐ Add token refresh            [ Interview ]
      ☑ Wire the callback URL
  ☐ Review & annotate
  ☐ Merge the branch
  Round 2 · branch feature/42-add-oauth · active · from main   [ Cancel run ]
  [ Mark implemented ]                                          ← secondaries
  ▾ Round log (2 cycles)
     Round 2 · reject · "Needs a migration test."
     Round 1 · approve · "Approved"
  ▸ Spec (42 lines)                     docs/spec/oauth-spec.md
```

Contents (decision #24 — all four are covered):

1. **Steps** — one subtask-styled row per step (`☐ pending / ◐ active / ☑ done`),
   selected by clicking the title like any other subtask. The **next pending
   step is highlighted** and carries its forward action on the row: the phase's
   start/resume (compose its prompt) for `interview` and `implement`, the
   approve for `spec` and `review`, the git merge for `merge`. The model's
   sub-tasks nest under the step they belong to, with a `nested run` badge or an
   `Interview` affordance.
2. **Round log** — the `@notes` history, newest first, grouped by cycle, with
   phase + kind chips.
3. **Branch + spec artifacts** — the branch with its status and base, and the
   spec with expand.
4. **Run meta** — `Round N`, branch/status, `Cancel run`, and the run's inline
   blocked reason (missing directory, dirty tree, merge conflict, …).

Behaviour:

- Phase actions are **user-initiated**. "Start interview" / "Start
  implementation" compose the phase prompt (§8.1) and insert it into the agent
  pane, then switch `RightPane` to `Agent` and focus the prompt box. The user
  edits/sends; nothing is auto-sent.
- The step's secondary actions sit in a row **below the step list**
  (`Write spec manually`, `Reject…`, `Mark implemented`, `Ask for a summary`).
  Reject opens the notes box, whose text becomes the rejection result
  `{"approved": false, "notes": "…"}` and is also appended to `@notes`.
- Phase steps are still ordinary tasks in the task list (subtasks of the feature
  task), so they can be ticked there too; they are excluded from the panel's own
  `Subtasks (N)` list, which keeps run-level sub-tasks. The panel refreshes on
  `WorkflowsPanelEvent::Changed`, on selection, and on coding-run notifications
  from the app's MCP endpoint.

### 8.1 Phase prompt templates (app-composed)

Built from `build_task_context(root_task)` plus phase-specific material, then
inserted with `AgentPane::insert_prompt_text`:

- **interview**: the full interview prompt is **expanded in the app**
  (`INTERVIEW_BASE_PROMPT` + title + description + re-spec notes) and sent as an
  ordinary prompt. **Not** the raw `/interview <target>` slash form: the pane's
  ACP agent is `opencode`, which registers no `interview` command and silently
  drops unknown `/`-prefixed prompts (opencode issue #27528), so a leading slash
  turns the phase into a no-op. Expanding client-side also makes the phase work
  identically against any ACP agent. The re-spec notes follow the target on a
  repeat round ("round N; the previous review rejected the spec because: …").
- **implement**: spec + annotation log + "work on branch `<branch>`; do not
  merge".
- **review**: "summarise what you changed, then call `complete_phase`; wait for
  the user's annotation".
- **sub-interview**: the same expanded interview prompt
  (`title + description`), sent against the sub-task's project session.

The templates live next to the `TaskDetails` stepper (one function per phase,
`&str` built from the run view) so they are unit-testable.

## 9. UX — agent pane

- **Prompt-driven phases** (decision #13): the pane only receives a composed
  prompt; the user sends it. No changes to the send path.
- **Run strip (optional, cheap):** a slim line in the agent pane header showing
  `Coding run · Round 2 · Implement` when the project has an active coding run,
  so the two panels stay associated when the user is in the agent view.
- **`ask_user` question card — required dependency, resolved in §17.1.** The
  pane advertises ACP form elicitation and answers `elicitation/create`, so the
  agent's `ask_user` questions render as a card instead of being dropped. §17.1
  specifies the bridge, the required agent-side schema fix, the agent choice,
  and the fallback.
- **Auto-complete on answer:** when the user submits answers to a sub-task
  interview round, the app calls
  `complete_workflow_step(interview_task_id, {})` for that sub-task's interview
  step (decision #8).
- **Tool permission cards** for the todo-2 MCP tools reuse the existing
  permission UI; "Always allow" writes the tool rule into the project's
  `tool_permissions` so the steady state is frictionless.
- Switching to `Agent` must not steal focus while a permission card or a
  question card is already pending.

## 10. Workflows panel changes (`workflows.rs`)

- The start-button row **skips recipes whose nodes declare phases** (coding
  recipes are started from a task, not from the panel).
- Coding runs appear as ordinary run cards, with the phase chip
  (`Round 2 · Implement`) and the same Approve/Reject/Complete buttons.
- New **"Branches to clean up"** section listing cancelled/abandoned runs that
  still hold a branch, with `Delete branch` (git `branch -D`, clears the
  columns) and `Keep` actions.
- Cancel keeps the existing tombstone behaviour (steps disappear) but the run
  stays listed with its branch (decision #21).

## 11. Git behaviour (app-managed)

- Project directory = the run root task's direct tag → `tag_settings.dirs[0]`
  (the same resolution the agent pane uses). Missing → phase blocked.
- **Create branch** (after spec approval):
  1. Verify the directory is a git repo and the working tree is clean
     (`git status --porcelain` empty).
  2. `base_branch = git rev-parse --abbrev-ref HEAD`;
     `git switch -c <branch>` with `branch` validated against
     `[A-Za-z0-9._/-]+` and non-empty, defaulting to
     `feature/<run-id>-<title-slug>` if the agent proposed nothing.
  3. Persist `branch`, `base_branch`, `branch_status = "active"`.
- **Merge** (on the merge step):
  1. `git switch <base_branch>` then `git merge --no-ff <branch>`.
  2. Success → `branch_status = "merged"`, append a `merge` note, complete the
     merge step and the root feature task (run completes).
  3. Conflict/failure → `git merge --abort`, keep the tree clean, and report the
     conflict via the inline blocked reason with a "Resolve in agent" action
     that composes a prompt from the conflicted paths.
- All git work runs off the foreground thread (`gpui_tokio::Tokio::spawn_result`
  via `Store`, like every other store call), never blocking the UI.
- No git2 dependency: shell out to `git` via `std::process::Command` with
  explicit `current_dir`, capturing stderr for the inline reason. Errors
  propagate as `anyhow` values to the UI — never swallowed.

## 12. Sub-tasks

Phase steps are themselves subtasks of the run root. The model splits *a step*
when a step is big enough to break down, so the parent of a sub-task is normally
that step's task (`phases[].task_id` / `open_task_id` from
`get_coding_context`); a run-level sub-task uses the feature task instead. The
details panel renders the steps as the subtask rows they are, with the model's
sub-tasks nested under the step they belong to.

- **Plain sub-task**: a task with `parent_id = <step task>` (or `<feature
  task>`), created by the model through `create_sub_task`. No phase machine; it
  appears nested under its step row (or in the panel's Subtasks list when it
  hangs off the feature task).
- **Nested run**: the same call with `nested: true` starts a `coding-task` run
  rooted at the sub-task, so the sub-task gets its own step list; the row shows
  a `nested run` badge and links to it by selection.
- **Sub-task interview**: `request_sub_task_interview` starts a
  `coding-sub-interview` run (single `interview` node, phase `interview`, no
  outgoing edges) rooted at the sub-task. The UI shows `Start interview`; the
  user launches it; the agent runs `/interview`; when the user answers, the
  interview step auto-completes and the spec lands on the sub-task
  (`tasks.spec`). The parent run is unaffected — the model can call
  `create_sub_task`/`append_note` afterwards to continue.
- Nested runs are **exempt** from the one-run-per-project guard.

## 13. Failure, cancel, concurrency

| Situation | Behaviour |
| --- | --- |
| Project has no directory | Phase action disabled, inline reason "Set this project's directory" + affordance to the tag settings. |
| No agent configured / CLI missing | Phase action disabled, inline "Configure an agent for this project" + affordance. |
| Agent busy / permission pending | Phase action queued behind the current turn (pane already queues prompts); the stepper shows "agent busy". |
| Dirty working tree | Branch creation and merge blocked with an inline reason naming the changed paths. |
| Branch name taken / invalid | Agent's proposal rejected, the app falls back to `feature/<run-id>-<slug>` and notes the substitution. |
| Merge conflict | `merge --abort`, inline reason + "Resolve in agent". |
| `git` not on PATH / not a repo | Inline reason; the user can still tick phases manually (the stepper never hard-blocks the human, it only disables the *automated* action). |
| Cancel | `cancel_run` as today (tombstone steps) + `branch_status = "abandoned"`, run stays visible in "Branches to clean up". |
| Second run on the same project | Blocked at start with an inline reason; nested runs are exempt. |
| Step deleted by the user | Existing v2 semantics apply (node becomes unsatisfiable, run auto-completes) — documented, not fixed in v1. |

## 14. Engine/code change list (by file)

**`libs/storage/src/workflow.rs`**
1. `RecipeNode` + parsing/validation for `phase`, `subtask`, `retrigger_node`.
2. `WorkflowRun` + `parse_run_row` + every raw SELECT for the four new columns.
3. `create_task_run` (new), reused by `create_run` internals.
4. `spawn_node_task`: honor `subtask` / `root_task_id`.
5. `retry_task`: take a target node (default = current parent behaviour) and
   preserve `@notes`.
6. `complete_workflow_step` / `evaluate_workflow_completion`: record
   `@notes` entries for approve/reject, and for `save_spec` cross-checks.
7. `coding_run_for_task`, `list_coding_runs` / branch-cleanup query,
   `cancel_run` sets `branch_status`.
8. `ensure_coding_recipes` (idempotent insert of `coding-task` v1 +
   `coding-sub-interview`), called on startup next to the managed-tag/automation
   seeding path.
9. Unit tests (§15).

**`libs/storage/src/task.rs`**
10. `spec` / `spec_path` columns in the model, `TaskCreate` builders, and the
    three positional record parsers.

**`libs/storage/src/todoist.rs`**
11. Nothing: the `workflow_run_id` guard already covers steps; confirm the root
    feature task (also `workflow_run_id`-tagged) is intentionally not synced,
    or exclude the root if the user wants the feature task in Todoist.

**`apps/todo-2/src/store.rs`**
12. Wrappers: `start_coding_run(task_id)`, `coding_run_for_task`,
    `approve_phase`, `reject_phase(notes)`, `save_note`, `merge_branch`,
    `create_sub_task`, `request_sub_task_interview`,
    `branch_cleanup_list/delete`, `set_project_branch`.

**`apps/todo-2/src/coding_git.rs`** (new, small)
13. The `git` shell-out helpers (branch create, status, merge, delete, abort,
    current branch) with stderr capture.

**`apps/todo-2/src/coding_mcp.rs`** (new)
14. The loopback MCP server: tool schemas, dispatch into `Store`, token check,
    lifecycle bound to the app.

**`apps/todo-2/src/ui_parts/task_details.rs`**
15. Coding section: stepper, round log, sub-task list, branch/spec artifacts,
    phase action handlers, inline blocked reasons; new event
    `CodingAction { task_id, action, notes }`.

**`apps/todo-2/src/ui_parts/agent_pane.rs`**
16. Phase-prompt insertion helper (reuse `insert_prompt_text`), optional run
    strip, `ask_user` question card (with `acp-client`).

**`apps/todo-2/src/ui_parts/workflows.rs`**
17. Filter phase recipes from the start row; phase chip on run cards; branch
    cleanup section.

**`apps/todo-2/src/main.rs`**
18. Wire `CodingAction` → store + prompt insertion + `right_pane = Agent`;
    subscribe the details panel to coding-run changes; start the MCP server;
    ensure recipes at startup.

**`libs/acp-client/src/connection.rs`**, **`src/thread.rs`**, **`src/agent.rs`**,
**`src/fake_agent.rs`**
19. Advertise `elicitation.form`; register the typed `CreateElicitationRequest`
    handler plus `AcpEvent::ElicitationRequested` / `ElicitationReply`; add the
    `Question` transcript entry; add the `AgentCliAgent` `AgentServer` impl and
    per-project agent selection; extend the fake agent with elicitation
    coverage (§17.1).

**`apps/agent-cli/src/acp.rs`**, **`src/tools.rs`**, **`src/subagents.rs`**
20. Wire client-supplied MCP servers into `ToolContext.mcp_manager`; build the
    elicitation form payload with the SDK types so the client's typed handler can
    parse it (§17.1); keep `ask_user` bound to the elicitation drainer.

## 15. Testing plan

Unit (storage, `workflow.rs` tests, `TodoStore::for_test()`):
- `coding-task` v1 validates; `phase`/`subtask`/`retrigger_node` validation
  rejects bad values, non-ai retrigger targets, and self-targets.
- `create_task_run` sets `root_task_id`, tags the root task, materializes
  `interview` as a subtask of the root, and rejects a second run on the same
  project / same task.
- Phase walkthrough: `save_spec` → `interview` done → `spec` spawned; approve →
  `implement` spawned; implement done → `review` spawned as a subtask;
  approve → `merge`; merge completes the root → run `completed`.
- Reject at `spec` and at `review` re-opens `interview`, preserves `@notes`,
  spawns a *new* `spec`/`implement`/`review` cycle, and does not disturb cycle-1
  rows.
- Cancelled runs with a branch stay visible; without a branch they stay hidden.
- `retrigger_node` absent → existing Case 4 gatekeeper behaviour unchanged
  (regression against the current tests).
- Sync guard: coding step tasks and root are skipped by the Todoist push.

Integration (todo-2):
- MCP tool calls against a stub client mutate run state and emit
  `CodingAction`-equivalent refreshes.
- Git helpers against a temp repo: branch create, dirty-tree refusal, merge,
  conflict → abort leaves a clean tree.

ACP bridge (`libs/acp-client`):
- The fake agent advertises form elicitation and emits an `elicitation/create`
  mid-turn; the client surfaces the card and returns Accept content, Decline,
  and a multi-select `Array` answer.
- Schema compatibility: the payload built by agent-cli's elicitation builder
  deserializes as `CreateElicitationRequest` (guards the integer-`const` bug).
- A client that does not advertise elicitation still finishes the turn (the
  agent simply has no `ask_user` tool) and the interview phase shows as
  unavailable instead of hanging.

UI (GPUI tests, `TestAppContext` with GPUI timers):
- Stepper renders the right phase/action per run state; blocked reasons disable
  the action.
- "Start interview" inserts the composed prompt and switches the pane.
- The question card renders each property type, maps Submit to Accept content
  and Dismiss to Decline, and does not steal focus from a pending permission
  card.

## 16. Out of scope (v1)

- Unattended/background/scheduled coding runs (every phase is user-present).
- Multi-agent fan-out / parallel implementation across repo areas.
- Remote PR creation and any GitHub integration (local merge only).
- A diff review surface (no line comments) — review is transcript + notes.
- Per-step cancellation, cycle caps, and run-level cost budgets.
- Editing the coding recipe from the UI (JSON in the DB, like other recipes).

## 17. Open questions

### 17.1 Resolved — the `ask_user` bridge in the pane (was open question 1)

**Decision: typed ACP Elicitation in `libs/acp-client` + a question card in
`AgentPane`, with `agent-cli --acp` as the pane's agent for coding runs.**
Nothing new is invented: the ACP SDK already models elicitation, agent-cli
already emits it, and the client already contains the exact handler pattern
(`session/request_permission`).

**Verified state (why this is needed):**

- `connection.rs` builds `ClientCapabilities::new().terminal(true).fs(…)`. It
  does **not** advertise `elicitation`, so agent-cli leaves `ask_user`
  unregistered for the pane's sessions (`ask_user_tool: None`, acp.rs:1407) —
  the model is told to use `ask_user` but has no such tool, so interview
  questions never reach the user.
- The SDK has full typed elicitation (`agent-client-protocol` 2.1.0 →
  `agent-client-protocol-schema` 1.7.0):
  `CreateElicitationRequest: JsonRpcRequest<Response = CreateElicitationResponse>`,
  `ElicitationCapabilities { form, url }`, `ElicitationFormMode`,
  `ElicitationSchema`, `ElicitationPropertySchema`, `MultiSelectItems`, and
  `ElicitationAction::{Accept(ElicitationAcceptAction{content}), Decline, Cancel}`.
- Registration is the same shape as the permission handler:
  `.on_receive_request(async |req: CreateElicitationRequest, responder, _cx| …,
  acp::on_receive_request!())` (the SDK's own `tests/schema_elicitation.rs`
  exercises exactly this round trip on the client side).
- The pane launches **`opencode acp`** (hardcoded `OpenCodeAgent`), while the
  interview contract (`/interview`, `ask_user`, interview notifications) lives in
  **agent-cli**; agent-cli keyed the interview flow on the prompt starting with
  `/interview ` (acp.rs:1585) — only then does it wrap the prompt, emit
  `interview/started|question|completed`, and capture the spec file path.
- Unregistered notifications are **ignored** by the SDK ("Unhandled: requests
  error, notifications ignored", `jsonrpc.rs:463`), so agent-cli's
  `interview/*` notifications need no client work in v1.

**Required work:**

1. **Advertise and handle (client).** In `libs/acp-client/src/connection.rs`:
   add `.elicitation(ElicitationCapabilities::new().form(
   ElicitationFormCapabilities::new()))` to the capabilities, and register
   `.on_receive_request(async |req: CreateElicitationRequest, responder, _cx| …)`
   mirroring the permission handler — offload with `tokio::spawn`, emit
   `AcpEvent::ElicitationRequested { request, reply: ElicitationReply }`, and
   answer from the UI thread through the oneshot:
   `CreateElicitationResponse::new(ElicitationAction::Accept(
   ElicitationAcceptAction::new().content(map)))`, `Decline` when the user
   dismisses, `Cancel` on session teardown. `content` must be an object; values
   are `ElicitationContentValue::{String, StringArray}`.
2. **Question card (pane).** Add a transcript/thread entry kind `Question`,
   modelled on the existing permission card: one field per schema property key
   (`q0`, `q1`, …) with its title/description, radio for `enum`/`oneOf`,
   checkboxes for `Array` items (labels from `anyOf[].title`), free-text fields
   otherwise, plus Submit / Decline / Dismiss. The card takes focus on arrival,
   blocks only that project's send path while pending, and renders the answers as
   a question/answer block afterwards.
3. **Make the agent emit the SDK schema.** `apps/agent-cli/src/acp.rs`'s
   `elicitation_params` hand-rolls the payload and, for multi-select, emits
   `items: {"anyOf": [{"const": <integer index>, "title": …}]}`. The SDK's
   `EnumOption.value` is a **`String`** and `MultiSelectItems` must be either
   `{"type":"string","enum":[…]}` or an untagged `{"anyOf":[{"const":"…"}]}`;
   an integer `const` matches neither, so the whole `CreateElicitationRequest`
   fails to deserialize against a typed handler. Build the request with the SDK
   types instead — `CreateElicitationRequest::new(ElicitationFormMode::new(
   ElicitationSessionScope::new(session_id), ElicitationSchema::new().property(
   "q0", schema, required)), message)` — keep suggestion-bearing questions
   freeform (`StringPropertySchema` with `min_length`/`max_length`/`pattern`,
   suggestions in `description`), encode choice identity as a **string** in
   `const`, and adapt `answer_from_elicitation` to map it back to
   `SelectedIndices` (a value that matches no choice stays freeform `OtherText`).
4. **Run agent-cli for coding runs.** Add an `AgentServer` impl
   (`program: "agent-cli"`, `args: ["--acp"]`, or a configured absolute path — it
   is usually not on `PATH`) plus a per-project agent choice; `id()` is already
   the session key, so sessions stay distinct and existing persisted sessions are
   unaffected. `opencode` remains the default for general chat. When the chosen
   agent does not advertise the interview surface, the `interview` phase action is
   disabled with an inline reason (decision #16) rather than failing mid-phase.
5. **Compose the interview prompt in slash form** (§8.1): the app sends
   `/interview <title + description + round notes>`; agent-cli wraps it and emits
   `interview/*`. `InterviewCompletedParams.spec_file_path` is a fallback for
   `tasks.spec_path`; the MCP `save_spec` content stays authoritative.

**Fallback if the bridge slips (keeps the workflow usable):** the interview phase
becomes a **manual spec** — the user writes or pastes the spec, the app stores it
in `tasks.spec`, and the step is ticked done (the stepper already advances by
tick). Elicitation is needed only by `interview`; `spec`, `implement`, `review`,
and `merge` are plain task phases, so the engine and UI can be built and
exercised end-to-end before the bridge lands.

**Tests:** extend `libs/acp-client/src/fake_agent.rs` to advertise form
elicitation and emit an `elicitation/create` mid-turn (accept, decline, and an
`Array` property) so the bridge is covered end-to-end; add a schema-compat test
that serializes the agent-side request and asserts it deserializes as
`CreateElicitationRequest` (the regression test for the integer-`const` bug); add
a GPUI test for the question card (render, submit → Accept content, dismiss →
Decline).

### 17.2 Still open

- **Is the `spec` approval gate wanted?** It exists so the branch is created at
  a good moment and the user can send the agent back before any code is
  written. If it feels like friction, the alternative is collapsing
  `interview`+`spec` into one node and creating the branch on the first
  implement action.
- **Multi-round sub-task interviews:** the decision says the step auto-completes
  when the user answers. If the model asks a second round, do we create a new
  interview step, or keep the step open until the agent calls
  `complete_phase`? Recommend: create a new step per round, so the state matches
  "answered = done".
- **Root task syncing:** should the feature task sync to Todoist? Today
  `workflow_run_id.is_some()` excludes it. Recommend excluding until the run
  completes, then clearing `workflow_run_id` (or keeping it, TBD).
- **MCP scoping:** one app-wide server with explicit ids (chosen) vs a per-run
  URL/token. The per-run token would let the server reject cross-run writes
  outright.
- **Cost/loop guardrails:** no cap in v1; do we at least surface a "Round N"
  warning and the round count in the run card?
- **`base_branch` in a repo with untracked-only changes:** `status --porcelain`
  includes untracked files — do we refuse branch creation on untracked files too,
  or only on tracked modifications?
- **Where the spec file path points:** the agent writes
  `./docs/spec/<slug>-spec.md` relative to the agent's cwd; the app resolves
  "open file" against the project directory. Confirm the preferred fallback when
  the path is outside the project.
- **`phase` on non-coding recipes:** leave `phase` unset for travel/automations
  (current plan) or migrate them to named phases for a uniform stepper?
- **Agent choice UX:** per-project agent setting vs a single global choice, and
  whether `opencode` sessions can be resumed for a project that switches to
  agent-cli (the session key is the agent `id()`, so a switch starts fresh).

## 18. Suggested implementation order

1. **`ask_user` bridge (§17.1):** client elicitation capability + handler +
   `Question` card, the agent-side SDK-schema fix, and the `AgentCliAgent`
   impl. Independent of storage, and the interview phase is meaningless without
   it.
2. Storage: migration `0017`, model/parsing changes, `retrigger_node`
   generalization, `create_task_run`, `@notes`, `coding_run_for_task`, tests.
3. Seed `coding-task` v1 + `coding-sub-interview`; extend `store.rs`.
4. `coding_git.rs` + store wrappers, with tests against a temp repo.
5. Details panel stepper + round log + blocked reasons. Phases can already be
   advanced by hand here; with a manual spec this exercises the whole engine
   path before any agent integration exists.
6. Workflows panel: hide phase recipes from the start row, run-card phase chip,
   branch cleanup.
7. `coding_mcp.rs` + agent-side MCP wiring.
8. Agent pane phase prompts + run strip (the question card lands in step 1).
9. Sub-task creation, nested runs, sub-task interviews.
10. Git merge on the Completed step; polish + next-action suggestions.

## 19. Implementation status

Implemented and verified (`cargo check --workspace` clean; `cargo test -p
storage` 97 pass, `cargo test -p todo-2` 42 pass, including the new cases).

**Storage (`libs/storage`)**

- Migration `0017_coding_workflow.sql`: `workflow_runs.root_task_id|branch|
  base_branch|branch_status`, `tasks.spec|spec_path`, root-task index.
- `RecipeNode.phase|subtask|retrigger_node` + validation (unknown phase,
  `subtask` on a non-action node, non-`ai` or self `retrigger_node` rejected).
- `@notes`: `RunNote` / `run_notes` / `append_run_note`; approvals and
  rejections are logged by `evaluate_step_completion`, and `retry_task` clears
  only the node's own result so the log survives a re-spec.
- `create_task_run` (root task tagged with the run, phases materialize as its
  subtasks, one-run-per-project guard with nested runs exempt),
  `coding_run_for_task`, `list_branch_cleanup_runs`, `set_run_branch`,
  `propose_run_branch`, `clear_run_branch`, `mark_branch_merged`,
  `complete_coding_merge`, `cancel_run` marking the branch abandoned.
- `seeded coding-task` v1 + `coding-sub-interview` via `ensure_coding_recipes`
  (called at app startup), `save_task_spec`, `RecipeMeta.phased`.
- Tests: recipe validation, phase walkthrough, review rejection → re-spec →
  second `spec` step with the log preserved, one run per project, cancelled run
  keeping its branch, branch normalization, idempotent seeding.

**todo-2**

- `src/store.rs`: `start_coding_run`, `coding_run_for_task`, `save_coding_spec`
  (stores the spec and completes the open interview step), `approve_coding_spec`
  (creates the branch once, reuses it on later cycles, dirty-tree and
  name-collision handling), `merge_coding_branch` (merge, `--abort` on
  conflict), `cancel_coding_run`, `request_sub_task_interview`,
  `list_branch_cleanup_runs`, `delete_coding_branch`, plus project-directory
  resolution through the task's ancestor tags.
- `src/coding_git.rs`: `is_repo`, `current_branch`, `changed_paths`,
  `branch_exists`, `create_branch`, `switch_branch`, `merge_no_ff`,
  `abort_merge`, `delete_branch`, with stderr captured in every error. Tested
  against throwaway repos (create/delete, dirty tree, merge, conflict→abort).
- `src/ui_parts/task_details.rs`: the run renders as a **step list at the bottom
  of the panel** (§8) — one subtask-styled row per phase (☐/◐/☑), the next
  pending step highlighted with its forward action on the row (start interview /
  start implementation / approve spec / approve review / merge branch), the
  model's sub-tasks of a step nested under it with a `nested run` badge or an
  `Interview` affordance, secondaries below the list (Write spec manually /
  Reject… / Mark implemented / Ask for a summary), plus the `Round N` + branch
  chips, `Cancel run`, inline blocked reason, round log grouped by cycle, and the
  expandable spec. Phase steps are excluded from the panel's own `Subtasks (N)`
  list, and the `Start coding workflow` affordance shows on a top-level feature
  task with no run. Phase prompt templates (`interview` in raw slash form,
  `implement`, `review`) are pure functions with unit tests.
- `src/ui_parts/workflows.rs`: phased recipes hidden from the start row, phase
  chip (`Round 2 · Implement`) on coding run cards, **Branches to clean up**
  section with delete/keep.
- `src/main.rs`: `ensure_coding_recipes` at startup, `CodingChanged` /
  `CodingLaunch` wiring (compose → insert into the agent pane → switch to it),
  coding-change notifications refreshing the panels.
- `src/coding_mcp.rs`: the tool surface — 8 tools with JSON schemas and a
  `dispatch` into the store, served over a minimal loopback HTTP/1.1 JSON-RPC
  endpoint (`initialize`, `tools/list`, `tools/call`) bound to `127.0.0.1:0`
  with a per-process bearer token, started with the app and logged at startup.
  Tested in-process and over a real socket (initialize / tools-list / tool call
  / rejected token).

**Still open (in addition to §17.2)**

1. **The `ask_user` bridge (§17.1) is not in.** No elicitation capability or
   handler in `libs/acp-client`, no question card, no agent-cli schema fix, no
   `AgentCliAgent` selection. The `interview` phase prompt is composed and
   inserted as `/interview <title + description>` (which agent-cli already
   understands), so an interview runs, but questions do not reach the user until
   the bridge lands — **Write spec manually** / **Save spec** is the working
   path meanwhile.
2. **The MCP endpoint is not handed to the agent yet.** `acp-client` still
   builds `NewSessionRequest::new(cwd).additional_directories(…)` with no
   `mcp_servers`, and agent-cli's MCP client (`cersei::mcp`) only speaks stdio,
   so the loopback endpoint needs an HTTP client transport (or a stdio shim)
   before it can be attached. The URL and token are logged at startup so an
   external MCP client can use it today.
3. Git merge conflicts report the git error inline but do not yet compose a
   "resolve in agent" prompt; the optional agent-pane run strip is not built;
   the stepper has no GPUI/widget tests (only the pure helpers are covered).

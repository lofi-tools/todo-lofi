# Workflow Engine Spec

Status: Draft v1 (engine + data model in `libs/storage`, minimal UI contract)

Purpose: Define a workflow engine that runs recipe-defined checklists against real-world
events: instant task dumps, relative timers, recurring calendar schedules, AI-assisted
steps with human approval, parallel fan-in, parameter-driven timing, external webhook
waits, and manual real-world awaits.

## 1. Intent

Users of the todo app want **workflows**, not just static task lists: a recipe that, once
started, unfolds over time — some steps appear instantly, some appear after a delay or a
calendar recurrence, some wait for an external signal, and some branch on a result
(human approval, AI-generated output). Each *run* of a recipe is a live checklist the user
can see and tick.

The engine's job is to instantiate a recipe graph into real tasks, evaluate conditions as
tasks complete, and spawn/cancel downstream tasks accordingly. Workflow-generated action
items are **ordinary tasks** (they live in the existing `tasks` table and participate in
the existing priority/urgency/blocked machinery); wait-states (`time`, `event`) are
lightweight task rows that normal todo views hide.

## 2. Scope

### 2.1 In scope

- **Storage + engine in `libs/storage`** — new tables (`workflow_recipes`, `workflow_runs`,
  `schedules`), new nullable columns on `tasks`, and engine functions (`create_run`,
  `complete_task`, `trigger_event`, `tick_timers`, `evaluate_node`, `cancel_task`,
  `cancel_run`) implemented against the existing `TodoStore`.
- **Minimal UI contract** — what queries/events the UI must be able to render (visible vs.
  hidden tasks, manual completion, param collection, event firing) without prescribing
  screens.
- **Eight acceptance cases** (§7) as the conformance target.

### 2.2 Out of scope (v1)

- Full UI screens / node-graph editor (recipe authoring is raw JSON for v1).
- Cron string parsing (schedules use a structured recurrence spec, §4.3).
- A full expression language (only the simple `{"if": "param:x", "then": ..., "else": ...}`
  form, §5.4).
- Direct AI-provider integration. AI steps are ordinary action tasks carrying a flag that
  an AI or script may pick up and complete (§5.5); the engine itself never calls an LLM.
- Distributed/multi-process engine coordination; exactly-one scheduler semantics.
- Schema migration tooling (follows the repo's existing toasty migration pattern).
- Webhook authentication posture (implementation-defined, dev-friendly in v1).

## 3. Concepts

| Term | Meaning |
| --- | --- |
| Recipe | Immutable, versioned JSON describing a workflow graph: nodes, edges, param schema, policies. One row in `workflow_recipes`. |
| Node | A step in the recipe graph: `action`, `time`, or `event` (see §3.1). |
| Edge | A directed dependency between nodes in the recipe, with `condition_type` and `condition_value`. Edges live **in the recipe JSON only**; they are the source of truth. |
| Run | One instantiation of a recipe (one row in `workflow_runs`). Runs are fully isolated: tasks reference `workflow_run_id`. |
| Start node | A node with no incoming edges. Materialized at `create_run`. |
| Waiter | A `time` or `event` task row. Time waiters carry a `blocked_until`; event waiters are tickable tasks ("Await reply"). Waiters are hidden from normal todo views. |
| Spawn | Creating a task row for a node at the moment its prerequisites are satisfied (lazy spawn, §3.2). |
| Result | The JSON payload a task is completed with (`complete_task(task_id, result)`), stored in the task's `workflow_details`. |
| Trigger | Marking a waiter satisfied, by timer tick (`tick_timers`) or external event (`trigger_event`). |

### 3.1 Node kinds

- **`action`** — a real, tickable task. Materializes as an ordinary `tasks` row
  (`item_type='action'`); visible in normal lists and the run's checklist.
- **`time`** — a delay. Materializes as a waiter task row with `blocked_until` set
  (reusing the existing time-block machinery and its sort/hide behavior). Never visible as
  a doable item.
- **`event`** — a wait for an external signal. Materializes as a **tickable waiter task**
  (e.g. "Await reply from client", "Await package") so the user can always resolve it
  manually even without a webhook. The downstream action does not exist until the waiter
  is triggered.

### 3.2 Lazy spawn

Downstream nodes **do not get task rows until their prerequisites are satisfied**. A
"hidden" step is literally absent from the database:

- Case 8's "Mark Delivered disappears for 3 days": `mark_received` has no row while the
  timer waits; it appears only when the timer fires.
- Case 7's "Draft Contract is invisible until the external signal": no row exists until
  the event waiter is triggered.
- Case 5's "Final Summary spawns only when all three are checked": evaluated against the
  recipe edges + run task states, not against pre-created rows.

Consequence: normal task-list queries never show future steps; there is nothing to filter.

### 3.3 Run isolation

Every `create_run` produces a new `workflow_runs` row with a unique id. All materialized
tasks and links reference that run id. Completing tasks in the 2025 birthday run touches
only `workflow_run_id = 2025_run`; the 2026 run is untouched. Runs never interfere.

## 4. Data model

All storage changes live in `libs/storage` (toasty migrations, sequential `.sql` files in
`libs/storage/toasty/migrations/`; next number after `0014`).

### 4.1 New columns on `tasks` (all nullable, no behavior change for ordinary tasks)

| Column | Type | Meaning |
| --- | --- | --- |
| `workflow_run_id` | INTEGER (FK → `workflow_runs.id`, indexed) | The run this task belongs to; `NULL` for ordinary tasks. |
| `item_type` | TEXT, default `'action'` | `'action'`, `'time'`, or `'event'`. Ordinary tasks are `'action'` by default. |
| `node_id` | TEXT | Recipe node id this row materializes (engine bookkeeping). |
| `workflow_details` | JSON | Run-scoped state: `result`, `caused_by`, `scheduled_at`, `event_name` (§4.5). |

Existing columns reused as-is:

- `blocked_until` — carries a time waiter's schedule (`scheduled_at` is stored in
  `workflow_details` for audit, but doability/visibility uses `blocked_until`).
- `done` / `completed_at` — action completion (and event-waiter ticking).
- `title` / `description` — node titles and any node-provided instructions.
- `parent_id` — approval subtasks hang off their automated task (§5.5).

### 4.2 `workflow_recipes` (new table)

| Column | Type | Meaning |
| --- | --- | --- |
| `id` | INTEGER PK autoincrement | |
| `slug` | TEXT | Stable recipe name used by schedules and run creation. |
| `version` | INTEGER | Monotonic per slug. |
| `recipe_json` | JSON | The full recipe definition (§5). |
| `created_at` | jiff::Timestamp | |

**Immutable + versioned.** A recipe row is never edited in place. Editing a recipe inserts
a new row with `version + 1` for the same slug; runs always reference the exact
`workflow_recipes.id` (version) they were created from. In-flight runs are unaffected by
new versions. A `current_version` pointer (or "latest" lookup by max version) is
engine-level, not a stored column.

### 4.3 `schedules` (new table)

| Column | Type | Meaning |
| --- | --- | --- |
| `id` | INTEGER PK autoincrement | |
| `recipe_id` | INTEGER (FK → `workflow_recipes.id`) | Exactly one versioned recipe. |
| `enabled` | BOOLEAN, default true | |
| `recurrence` | JSON | Structured recurrence (§4.4). |
| `timezone` | TEXT | IANA timezone for recurrence evaluation (default: app/account timezone). |
| `last_fired_at` | jiff::Timestamp, nullable | Watermark for skip/catch-up decisions. |
| `created_at` | jiff::Timestamp | |

No cron strings. The `schedules` table plus the engine's background loop (§6.5) replace
cron.

### 4.4 Recurrence spec (JSON)

Mirrors the existing `repeat_task_templates` fields so the engine can share evaluation
machinery with the recurrence engine:

```json
{
  "interval_days": null,
  "time_of_day": "09:00",
  "weekdays": null,
  "month_day": 1,
  "month": 3,
  "strict": false
}
```

- `interval_days` — every N days (from `last_fired_at` or recipe start).
- `weekdays` — list 0–6 (ISO or configured convention; match the existing recurrence
  engine).
- `month_day` — 1–31, or `-1` for last day of month.
- `month` — 1–12, optional; with `month_day` gives "every Nth of month M" (e.g. March 1 =
  birthday case).
- Evaluation happens in the schedule's `timezone`.
- DST: `time_of_day` is a wall-clock `"HH:MM"` in the schedule's timezone, evaluated by
  the existing jiff-based recurrence engine, so it holds across DST transitions. On a
  spring-forward gap where the wall time does not exist, fire at the first valid instant
  after the gap; on a fall-back fold, fire once at the first occurrence.
- Leap day: `month_day=29` with `month=2` fires only in leap years; `month_day=-1` (last
  day of month) resolves to the real last day (Feb 28/29, Apr 30, …).

### 4.5 `workflow_runs` (new table)

| Column | Type | Meaning |
| --- | --- | --- |
| `id` | INTEGER PK autoincrement | The run id referenced by `tasks.workflow_run_id`. |
| `recipe_id` | INTEGER (FK → `workflow_recipes.id`) | Exact recipe version. |
| `schedule_id` | INTEGER, nullable (FK → `schedules.id`) | Set when the run was scheduler-created. |
| `status` | TEXT | `'active'`, `'completed'`, `'cancelled'` (§6.7). |
| `parameters` | JSON | Runtime params collected at run creation (§5.3). |
| `scheduled_for` | jiff::Timestamp, nullable | Scheduled occurrence time for scheduler-created runs. |
| `created_at` | jiff::Timestamp | |
| `completed_at` | jiff::Timestamp, nullable | |

### 4.6 Edges and `task_links`

The existing `task_links` table (`task_id`, `other_id`, `kind`) is reused — not extended —
for **run-level dependency links between materialized task rows**. Links introduced by a
run use the dedicated `kind` `'workflow_dep'` (never `'blocked_by'`, which stays reserved
for user-authored todo dependencies).

A `workflow_dep` link is materialized at spawn time, when both endpoint rows exist, with
`task_id = <prerequisite row>` and `other_id = <spawned row>`. Links serve two purposes:

- **Audit trail** — which completed prerequisite (and result) caused a spawned task.
- **Cancellation cascade** (§6.6, §6.8) — `cancel_task` / `retry_task` walk outgoing
  `workflow_dep` links to find and cancel the tasks spawned from a task's completion.

The recipe JSON remains the source of truth for graph evaluation (§6.4); links never gate
spawning.

### 4.7 Migration

One new migration file (e.g. `0015_workflow_engine.sql`):

1. `CREATE TABLE workflow_recipes (...)` with index on `(slug, version)`.
2. `CREATE TABLE schedules (...)`.
3. `CREATE TABLE workflow_runs (...)` with index on `recipe_id`, `status`.
4. `ALTER TABLE tasks ADD COLUMN workflow_run_id INTEGER` + index.
5. `ALTER TABLE tasks ADD COLUMN item_type TEXT NOT NULL DEFAULT 'action'`.
6. `ALTER TABLE tasks ADD COLUMN node_id TEXT`.
7. `ALTER TABLE tasks ADD COLUMN workflow_details TEXT`.

## 5. Recipe format (`recipe_json`)

```json
{
  "name": "Gatekeeper",
  "params": {
    "urgent": { "type": "boolean", "default": false }
  },
  "missed_policy": "skip",
  "nodes": [
    { "id": "draft", "kind": "action", "title": "Draft the email", "ai": true, "description": "..." },
    { "id": "send", "kind": "action", "title": "Send message" },
    { "id": "edit", "kind": "action", "title": "Edit draft" }
  ],
  "edges": [
    { "from": "draft", "to": "send", "condition_type": "on_result", "condition_value": { "approved": true } },
    { "from": "draft", "to": "edit", "condition_type": "on_result", "condition_value": { "approved": false } }
  ]
}
```

### 5.1 Nodes

- `id` — required, unique within the recipe, `^[A-Za-z0-9_-]+$`.
- `kind` — required: `action` | `time` | `event`.
- `title` — required; the task title when materialized. For `event` nodes this is the
  tickable waiter title (e.g. "Await reply from client").
- `description` — optional node instructions.
- `ai` (action only, default false) — the task is marked doable-by-AI (§5.5).
- `approval` (action only, default false) — this node is a human approval step. It is the
  ONLY node that materializes as a subtask (§5.5); its incoming edge must be an `on_result`
  edge from an `ai: true` action node (§5.8).
- `retrigger_on_reject` (approval nodes only, default false) — when the approval result
  matches a rejection branch, the engine re-opens the automated parent task via
  `retry_task` (§6.8).

Wait parameters live on the **incoming edge**, never on the node: a `time` node's
incoming edge is a `timer` edge carrying the duration (§5.4); an `event` node's incoming
edge is an `event` edge carrying the `event_name`. A `time`/`event` node MUST have
exactly one incoming edge and exactly one outgoing `on_complete` edge (§5.8).

### 5.2 Edges

- `from`, `to` — required node ids (must reference `nodes[].id`, `from != to`).
- `condition_type` — required, one of:
  - `on_complete` — no `condition_value`. Target spawns when the source completes
    (fan-in: target spawns when **all** incoming `on_complete` edges are satisfied).
    Target MUST be an `action` node.
  - `on_result` — `condition_value` required (any JSON value). Target spawns when the
    source's completion `result` matches `condition_value` by key-based matching (§6.4).
    Source and target MUST be `action` nodes. Non-matching `on_result` edges are
    cancelled.
  - `timer` — `condition_value` required: a duration or duration expression (§5.4).
    Target MUST be a `time` node.
  - `event` — `condition_value` required: non-empty string `event_name`. Target MUST be
    an `event` node.
- Fan-in rule: a node with multiple incoming edges spawns only when **all** incoming
  edges are satisfied (AND). OR-triggers via a recipe-level `trigger` expression are
  future work (non-goal for v1).

### 5.3 Params schema

`params` maps param names to `{ "type": "boolean" | "string" | "number", "default": ...,
"required": bool }`. At run creation the UI/API renders a form from this schema and the
collected values are stored in `workflow_runs.parameters`. Params are referenced from
duration expressions (§5.4) as `param:<name>`.

Validation is type-and-presence only: `type` must match, `default` must match `type`,
`required: true` params with no `default` must be provided at `create_run`, and unknown
param names are rejected. No pattern/format validation for `string`/`number` values in
v1 (deferred, §10.9).

### 5.4 Duration expressions

`condition_value` for `timer` edges is either a plain duration (`"4 days"`, `"3 days"`,
`"0 days"`) or the v1 conditional form:

```json
{ "if": "param:urgent", "then": "0 days", "else": "3 days" }
```

Resolution: look up the named param on the run; truthiness of the value picks the branch.
Anything else is a recipe validation error at `create_run` time.

### 5.5 AI steps and approval (Case 4)

- An `action` node with `"ai": true` materializes as an ordinary task carrying an
  `ai_doable` marker in `workflow_details`. An external AI or script (e.g. a future
  "plannotator") may pick the task up, perform the work, and complete it with a `result`
  (or edit its content) via the normal `complete_task` path. The engine itself never
  calls an AI provider.
- **Human review/approval is a subtask** of the automated task: an action node with
  `"approval": true` materializes as a **child task** (`parent_id` → the automated task's
  row) rather than a top-level task. When the automated (`ai: true`) node completes, the
  engine spawns its approval child. `complete_task(approval_task, result)` evaluates the
  approval node's outgoing `on_result` edges exactly like any completion — approval spawns
  `send`, rejection spawns `edit`.
- **Re-trigger:** when an approval node has `"retrigger_on_reject": true` and its result
  matches a rejection branch (an outgoing `on_result` edge with `condition_value =
  {"approved": false}`), the engine spawns the rejection branch as normal and then calls
  `retry_task` on the automated parent (§6.8): the parent is reset to pending and re-runs
  its outgoing edge evaluation on the next completion.

### 5.6 Missed-occurrence policy

`missed_policy` on the recipe: `"skip"` (default) or `"catch_up"`. Governs what the
scheduler does for occurrences that passed while the app was closed (§6.5).

### 5.7 Full JSON Schema (draft-07)

The authoritative structural schema for `workflow_recipes.recipe_json`:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "Workflow recipe",
  "type": "object",
  "additionalProperties": false,
  "required": ["name", "nodes", "edges"],
  "properties": {
    "name": { "type": "string", "minLength": 1 },
    "params": {
      "type": "object",
      "additionalProperties": { "$ref": "#/definitions/param" }
    },
    "missed_policy": { "enum": ["skip", "catch_up"] },
    "nodes": {
      "type": "array",
      "minItems": 1,
      "items": { "$ref": "#/definitions/node" }
    },
    "edges": {
      "type": "array",
      "items": { "$ref": "#/definitions/edge" }
    }
  },
  "definitions": {
    "param": {
      "type": "object",
      "additionalProperties": false,
      "required": ["type"],
      "properties": {
        "type": { "enum": ["boolean", "string", "number"] },
        "default": true,
        "required": { "type": "boolean" },
        "description": { "type": "string" }
      }
    },
    "node": {
      "type": "object",
      "additionalProperties": false,
      "required": ["id", "kind", "title"],
      "properties": {
        "id": { "type": "string", "pattern": "^[A-Za-z0-9_-]+$" },
        "kind": { "enum": ["action", "time", "event"] },
        "title": { "type": "string", "minLength": 1 },
        "description": { "type": "string" },
        "ai": { "type": "boolean" },
        "approval": { "type": "boolean" },
        "retrigger_on_reject": { "type": "boolean" }
      }
    },
    "edge": {
      "type": "object",
      "additionalProperties": false,
      "required": ["from", "to", "condition_type"],
      "properties": {
        "from": { "type": "string", "pattern": "^[A-Za-z0-9_-]+$" },
        "to": { "type": "string", "pattern": "^[A-Za-z0-9_-]+$" },
        "condition_type": {
          "enum": ["on_complete", "on_result", "timer", "event"]
        },
        "condition_value": true
      }
    }
  }
}
```

Note: `condition_value: true` in the schema means "any JSON"; per-`condition_type` shape
constraints are enforced by the validation rules below. `default: true` on params means
"any JSON default"; the default's type must match the declared `type`.

### 5.8 Validation rules

Validation runs at **recipe save time** (structural + graph rules) and again at
**`create_run` time** (params against schema, duration expressions against declared
params). A recipe that fails validation is rejected; existing runs are unaffected
(recipes are immutable, §4.2).

Structural (JSON Schema, §5.7):

1. Root object matches the schema; unknown keys rejected (`additionalProperties:
   false`).
2. `name` is a non-empty string.
3. `params` values have a valid `type`; `default` (if present) matches the declared
   type; `required` (if present) is a boolean.
4. Every node has a non-empty `id`, a valid `kind`, and a non-empty `title`; node ids
   match `^[A-Za-z0-9_-]+$`.
5. `ai` only on `action` nodes. `approval` only on `action` nodes; `retrigger_on_reject`
   only on nodes with `"approval": true`. An `approval` node's incoming edge is an
   `on_result` edge from an `ai: true` action node.

Graph:

6. Node ids are unique within the recipe.
7. Every edge's `from` and `to` reference existing node ids; `from != to`; no duplicate
   `(from, to, condition_type)` triples.
8. The graph is a DAG (topological sort succeeds). Cycles are rejected: lazy spawn and
   fan-in evaluation are defined only for acyclic graphs.
9. At least one node has no incoming edges (guaranteed by 8 when `nodes` is non-empty).
10. `on_complete` edges carry no `condition_value`; their target is an `action` node.
11. `on_result` edges: source and target are `action` nodes; `condition_value` is
    present.
12. `timer` edges: target is a `time` node; `condition_value` is a valid duration or
    duration expression (rule 14).
13. `event` edges: target is an `event` node; `condition_value` is a non-empty string
    (`event_name`).
14. A `time`/`event` node has exactly one incoming edge (of the matching type per rules
    12/13) and exactly one outgoing edge, which is `on_complete`.

Values:

15. Duration strings match `^(0|[1-9]\d*)\s*(second|minute|hour|day)s?$` (singular or
    plural unit; `0` allowed).
16. Duration expressions are exactly `{"if": "param:<name>", "then": <duration>,
    "else": <duration>}`; `param:<name>` must reference a param declared in `params`;
    both branches are valid durations (rule 15).
17. `missed_policy` is `"skip"` or `"catch_up"` (default `"skip"` when absent).
18. At `create_run`: provided params are checked against the schema — every `required`
    param present, no unknown param names, values match declared types; duration
    expressions are resolved against `run.parameters` and any unresolved
    `param:<name>` reference is a validation error.

An implementation MAY tighten the schema further (e.g. `oneOf` per `condition_type`)
as long as it accepts every recipe the rules above accept.

## 6. Engine functions (`TodoStore` methods)

### 6.1 `create_run(recipe_id, params, schedule_id?) → Run`

1. Load recipe; validate params against the schema and duration expressions (§5.4).
2. Insert `workflow_runs` row (`status='active'`, `parameters`, optional `schedule_id`).
3. Find start nodes (no incoming edges). For each, insert a `tasks` row:
   `item_type='action'`, `workflow_run_id`, `node_id`, `workflow_details={"caused_by":
   {"type": "start"}}`.
4. Return the run.

### 6.2 `complete_task(task_id, result)`

1. Mark the task `done` (`done=true`, `completed_at=now`); store `result` in
   `workflow_details.result`.
2. Load outgoing recipe edges from the node's `node_id` in the run's recipe version.
3. For each edge, evaluate (§6.4):
   - `on_complete` → mark edge satisfied; if target's incoming edges are all satisfied →
     spawn target.
   - `on_result` → if `result` matches `condition_value` → spawn target; else mark the
     edge cancelled (no row created).
   - `timer` → resolve duration (§5.4) → compute `scheduled_at = now + duration` → spawn a
     `time` waiter: `item_type='time'`, `blocked_until=scheduled_at`,
     `workflow_details={"scheduled_at": ..., "caused_by": {"type": "timer_started"}}`.
   - `event` → spawn an `event` waiter: `item_type='event'`, title from the event node
     (e.g. "Await reply from client"), `workflow_details={"event_name":
     <condition_value>, "caused_by": {"type": "event_started"}}`.
4. Materialize `task_links` rows (kind `'workflow_dep'`) for satisfied edges whose
   endpoint rows now exist.
5. Run auto-complete check (§6.7).
6. If the completed node had an AI marker and the recipe declares an approval subtask
   (an action node with `"approval": true` whose incoming `on_result` edge's source is
   this node), spawn the approval child task (`parent_id` = this task's row).

### 6.3 `trigger_event(run_id, event_name)`

1. Find pending `event` waiters for the run whose `workflow_details.event_name` matches.
2. Mark them triggered: `done=true` (they are tickable tasks), store
   `caused_by={"type": "event_fired"}`.
3. Evaluate each triggered waiter's outgoing edges → spawn targets (§6.4).
4. Run auto-complete check (§6.7).

Invoked by both the webhook endpoint and the UI event-firing button. Triggering an event
waiter manually (user ticks "Await reply") goes through the same code path via
`complete_task`.

### 6.4 `evaluate_node(run_id, node_id)` / fan-in check

A node is spawnable when **all** incoming edges are satisfied:

- `on_complete` edge satisfied ⟺ source node has a completed task row in this run.
- `on_result` edge satisfied ⟺ source's completed `result` matches `condition_value`
  (matching rules below).
- `timer`/`event` edges are not incoming to spawnable nodes directly — they materialize
  waiters, and the waiter's own outgoing edges drive spawning.

`on_result` matching is **key-based** (not full deep equality), so AI-produced results may
carry extra fields without breaking branches:

- If `condition_value` is an object: it matches when **every key** present in
  `condition_value` exists in `result` and matches recursively with the same rule. Extra
  keys in `result` are ignored. `{}` matches any result.
- If `condition_value` is a scalar (`boolean`, `number`, `string`, `null`): it matches
  only when `result` is exactly that scalar.
- Nested objects inside either value recurse with the same rule.

Fan-in: a node with multiple incoming edges spawns when all are satisfied, in any order.
If any incoming `on_complete` edge is **cancelled** (§6.6), the target can never satisfy
fan-in, so all of its other incoming edges are cancelled too (fan-in collapse) — the
target never spawns and the run can still complete (§6.7). Example (Case 5):
`final_summary` has 3 incoming `on_complete` edges; after each research task completes,
count unsatisfied incoming edges; spawn only at zero.

### 6.5 `tick_timers()` and the scheduler loop

Execution model: **lazy + background** (chosen).

- **Lazy:** after any `complete_task` / `trigger_event` / UI refresh, the engine evaluates
  pending `time` waiters whose `blocked_until <= now` and processes them (mark triggered,
  evaluate outgoing edges, spawn targets).
- **Background:** the app runs a low-frequency loop (e.g. every minute) calling
  `tick_timers()` for wall-clock accuracy, plus a **catch-up pass at app startup** so
  timers that fired while the app was closed still resolve.
- **Schedules:** one loop — the same 1-minute background pass — evaluates `tick_timers()`
  and every enabled schedule's recurrence in its timezone against `last_fired_at`
  (minute resolution; `time_of_day` is `"HH:MM"`):
  - On match → `create_run(recipe_id, {}, schedule_id, scheduled_for=now)` and update
    `last_fired_at`.
  - Missed occurrences (app was closed): `"skip"` policy → no backfill, next evaluation
    is the next future occurrence; `"catch_up"` policy → a run is created for every
    occurrence between `last_fired_at` and now.
  - DST and leap-day behavior follow §4.4.

### 6.6 Cancellation (core engine function)

- `cancel_task(task_id)` → mark the task `cancelled`. Cascade through the run's own
  recipe edges only (never across runs):
  1. Mark every outgoing recipe edge of the task's node cancelled.
  2. Walk outgoing `workflow_dep` links (§4.6) and cancel each linked task the same way
     (recursively), including spawned waiters and branch targets.
  3. If any cancelled edge participates in a fan-in target, apply fan-in collapse (§6.4)
     to that target's remaining incoming edges.
  4. Run the auto-complete check (§6.7).
- `cancel_run(run_id)` → mark all of the run's task rows `cancelled`, mark the run
  `status='cancelled'`.

### 6.7 Run lifecycle / auto-complete

`workflow_runs.status` is `'active' | 'completed' | 'cancelled'`.

A run auto-completes after any engine mutation when it is `'active'` and both:

- **(a) No pending tasks** — every task row of the run is terminal (`done` or
  `cancelled`). A pending `time` or `event` waiter keeps the run active.
- **(b) No pending edges** — every edge of the recipe version is terminal:
  - `satisfied` — its target spawned (or it contributes to a satisfied fan-in); or
  - `cancelled` — its source was cancelled, its `on_result` did not match, or fan-in
    collapse (§6.4) applied.
  - An edge whose source is done but whose target has not spawned (still waiting on a
    fan-in sibling or a waiter) is `pending` and keeps the run active.

On completion, flip the run to `'completed'` and set `completed_at`. Completed runs do
not archive or hide their tasks: done tasks keep the existing todo-list behavior (sorted
to the bottom, hidden after `COMPLETED_TASK_VISIBLE_SECS`), and the run row persists with
its status for the run view. Fan-in collapse (§6.4) guarantees (b) is reachable — a
target with a cancelled incoming edge never blocks run completion.

### 6.8 `retry_task(task_id)` — re-triggering an automated task

Re-opens a completed task so an AI or script can produce a revised result (§5.5). Only
meaningful for `ai: true` action tasks; calling it on any other task is a no-op error.

1. Reset the task: `done=false`, `completed_at=null`, clear `workflow_details.result`,
   set `caused_by={"type": "retriggered"}`.
2. Cancel the cascade of the task's prior completion: walk outgoing `workflow_dep` links
   (§4.6) and cancel each linked task recursively (spawned branches, approval children,
   waiters), exactly like `cancel_task` (§6.6); re-arm the task's outgoing edges to
   pending.
3. Leave the task pending for the AI/script (or human) to complete again; the next
   `complete_task` evaluates its outgoing edges fresh.

Re-triggering never affects other runs: `workflow_dep` links are scoped to the run.

## 7. Acceptance criteria

| Case | Name | Behavior |
| :--- | :--- | :--- |
| 1 | Packing List (Dump) | Running the recipe instantly makes all tasks Available simultaneously. |
| 2 | Relative Timer | Task A (Action) appears. When checked off, a 4-day timer starts *exactly then*. Task B appears exactly 4 days later. |
| 3 | Recurring Birthday | A calendar schedule spawns a **brand new, isolated Run** every year. Runs never interfere. |
| 4 | Gatekeeper (AI + Approval) | AI generates a draft (Action). System waits. User marks "Approved" (result) to spawn "Send". Rejection spawns "Edit". |
| 5 | Parallel Research | Three independent Actions appear immediately. "Final Summary" only spawns when **all three** are checked off (any order). |
| 6 | Conditional Urgency | Runtime param (`urgent: true/false`). If true, "Call" appears immediately. If false, it delays by 3 days. |
| 7 | External Event (Webhook) | After "Send Email", system waits. "Draft Contract" is invisible until an external signal (webhook/button) wakes it. |
| 8 | Manual Real-World Await | After "Place Order", the next task ("Mark Delivered") **disappears** for 3 days, then reappears as a manual checkbox (no webhook, user ticks it when the package arrives). |

## 8. Case walkthroughs (against the final model)

### Case 1 — Packing List

Recipe: 3 `action` nodes, no edges → all start nodes.

`create_run` inserts 3 task rows (`item_type='action'`, pending, `caused_by={"type":
"start"}`). The run's checklist query lists all pending action items → all 3 visible
instantly. No timers, no waits.

### Case 2 — Relative Timer

Recipe: `submit` (action, start) → edge `timer` `"4 days"` → `followup` (action).

1. `create_run` inserts `submit`.
2. `complete_task(submit)` → done. Timer edge resolves `scheduled_at = now + 4d`; engine
   spawns a `time` waiter (`blocked_until = scheduled_at`, hidden from lists).
   `followup` has **no row**.
3. `tick_timers()` (lazy on next refresh, or the background loop) at `scheduled_at`: waiter
   triggered → spawns `followup` (`caused_by={"type": "timer_fired"}`). Exactly 4 days
   after the checkbox, not after run creation.

### Case 3 — Recurring Birthday

Schedule: recurrence `{"month_day": 1, "month": 3, "time_of_day": "00:00"}`,
timezone set; recipe `missed_policy` decides skip/catch-up.

Scheduler pass on March 1 → `create_run(recipe, schedule_id, scheduled_for=...)` → brand
new `workflow_runs` row. All tasks carry the new `workflow_run_id`; the 2025 run and the
2026 run are fully isolated.

### Case 4 — Gatekeeper (AI + Approval)

Recipe: `draft` (action, `ai: true`) → `on_result {"approved": true}` → `send`; →
`on_result {"approved": false}` → `edit`.

1. `create_run` inserts `draft` (carries `ai_doable` marker). An AI or script may pick it
   up; a human can also complete it directly.
2. `complete_task(draft, {"approved": true})` → done, result stored. `send` edge matches →
   spawn `send`. `edit` edge does not match → cancelled, no row.
3. UI shows "Send Message" only. Rejection completes the mirror path.

### Case 5 — Parallel Research

Recipe: start nodes `research_a/b/c`; `final_summary` with 3 incoming `on_complete` edges.

`create_run` inserts the three research tasks. Each `complete_task` satisfies one incoming
edge of `final_summary`; fan-in count stays > 0 until all three are done (any order), at
which point `evaluate_node` spawns `final_summary` and materializes its `workflow_dep`
links to the three research rows.

### Case 6 — Conditional Urgency

Recipe: params schema `{"urgent": {"type": "boolean", "default": false}}`; timer edge to
`call_client` with `condition_value = {"if": "param:urgent", "then": "0 days", "else":
"3 days"}`.

`create_run(recipe_id, {"urgent": true})` stores params on the run. On the previous step's
completion the duration expression resolves against `run.parameters` → 0 days → waiter
with `blocked_until = now` → fires on the next lazy tick → "Call" appears immediately.
`urgent: false` → 3 days.

### Case 7 — External Event

Recipe: `send_email` → `event` edge (`client_replied`) → `draft_contract`.

1. `create_run` inserts `send_email`.
2. `complete_task(send_email)` → engine spawns an `event` waiter: an **ordinary tickable
   task** "Await reply from client" (`item_type='event'`, `event_name='client_replied'`).
   `draft_contract` has no row.
3. `trigger_event(run_id, "client_replied")` — via the HTTP webhook endpoint or the UI
   button, or by the user manually ticking the waiter (same code path) — marks the waiter
   triggered → spawns `draft_contract`.

### Case 8 — Manual Real-World Await

Recipe: `place_order` → `timer` `"3 days"` → `mark_received` (action).

1. `create_run` inserts `place_order`.
2. `complete_task(place_order)` → spawns a `time` waiter (`blocked_until = +3d`).
   `mark_received` does **not** exist. The run's action list is empty — the next step has
   disappeared.
3. `tick_timers()` at +3d → waiter triggered → spawns `mark_received` (`caused_by={"type":
   "timer_fired"}`). The user ticks it manually when the package arrives. No webhook, no
   auto-complete.

## 9. Minimal UI contract

The engine guarantees these queries/events; screens are implementation-defined:

- **Run checklist:** `SELECT * FROM tasks WHERE workflow_run_id = ? AND item_type='action'
  AND status pending` → visible actions only (waiters and unspawned nodes excluded).
- **Run creation:** a form generated from the recipe's `params` schema; submit →
  `create_run`.
- **Completion:** ticking an action task → `complete_task`.
- **Event firing:** a UI button in the run view, or the webhook endpoint below, calls
  `trigger_event(run_id, event_name)`.
- **Waiter visibility:** `time`/`event` waiters are hidden from ordinary todo lists (list
  queries filter `item_type='action'`). The run view shows them in a collapsed "Waiting"
  section: `event` waiters are tickable there (manual resolution), `time` waiters are
  read-only with a countdown until `blocked_until`.
- **Webhook endpoint (Case 7):** the app exposes a loopback HTTP listener:
  `POST /webhook/events` with body `{"run_id": <int>, "event_name": "<string>"}` →
  `trigger_event`. Responses: `204` when at least one waiter was triggered; `404` when the
  run has no pending waiter for that `event_name`; `400` on a malformed body; `405` for
  non-POST. Binding is loopback-only (`127.0.0.1`) on a configurable port (default `8787`;
  `0` = ephemeral, for tests). v1 posture: no auth required because the listener binds
  loopback only; an optional `X-Webhook-Token` header MAY be required when configured.
  Production deployments MUST keep loopback binding or add auth.
- **Run status:** show `active` / `completed` / `cancelled`; cancelled runs (and their
  tasks) remain visible as cancelled, not deleted.
- **Hybrid list behavior:** engine-created `action` tasks appear in normal todo lists like
  any other task (priority/urgency/blocked machinery applies); the run view is an
  additional, filterable surface.

## 10. Resolved decisions

Each former open question is now decided; decisions are normative for v1.

1. **`on_result` matching — key-based.** `condition_value` objects match when every key
   present in `condition_value` exists in `result` and matches recursively; extra `result`
   keys are ignored; `{}` matches any result. Scalars match by exact equality (§6.4).
   Rationale: AI-produced results carry extra fields the recipe author cannot predict.
2. **AI re-trigger — `retry_task` + `retrigger_on_reject`.** Approval nodes
   (`"approval": true`) materialize as subtasks of the automated task; with
   `"retrigger_on_reject": true`, a rejection re-opens the automated parent via
   `retry_task` (§5.5, §6.8).
3. **Waiter representation.** `time` waiters are `tasks` rows with `item_type='time'` and
   `blocked_until = scheduled_at` (existing column reused; no dedicated columns). `event`
   waiters are tickable `tasks` rows with `item_type='event'`. Both are hidden from
   ordinary lists and shown in the run view's collapsed "Waiting" section (§9).
4. **`task_links` kind — `'workflow_dep'`.** Never `'blocked_by'`. Links are materialized
   at spawn for audit and for the cancellation cascade; they never gate spawning (§4.6).
5. **Auto-complete — task + edge terminality.** Precise rule in §6.7, including fan-in
   collapse so cancelled prerequisites never hang a run. Completed runs do not archive
   tasks; existing completed-task visibility applies.
6. **Webhook surface.** Loopback-only `POST /webhook/events` on a configurable port
   (default 8787, `0` = ephemeral); optional `X-Webhook-Token` when configured; response
   codes 204/400/404/405 (§9).
7. **Schedule edge cases.** One 1-minute background loop drives both `tick_timers()` and
   schedule evaluation; `time_of_day` is wall-clock `"HH:MM"` in the schedule's timezone
   via the existing jiff-based recurrence engine (DST gaps fire at the first valid
   instant, folds fire once); `month_day=29` + `month=2` fires only in leap years and
   `month_day=-1` resolves per month length (§4.4, §6.5).
8. **Param validation — type and presence only.** No pattern/format validation for
   `string`/`number` values in v1 (§5.3).
9. **Deferred (deliberately out of v1, not open):** regex/pattern validation for string
   params; OR-triggers via recipe-level `trigger` expressions; a recipe editor UI.

## 11. Non-goals / out of scope (v1)

- No cron string parsing; no recipe editor UI; no full expression language.
- No AI-provider calls by the engine (AI pick-up is metadata only).
- No distributed scheduling; single-process engine in the app.
- No webhook auth standard beyond loopback binding + optional token (§9); no multi-tenant
  run management.
- No schema migration tooling specified here (existing repo migration flow applies).

## 12. Example recipes (all 8 acceptance cases)

Each example is a complete `recipe_json` document (plus the schedule row where a case
needs one). All examples validate against §5.7 and §5.8.

### 12.1 Case 1 — Packing List (Dump)

All three nodes have no incoming edges → all are start nodes; `create_run` materializes
them all immediately. `edges` may be empty.

```json
{
  "name": "Packing List",
  "nodes": [
    { "id": "swimsuit", "kind": "action", "title": "Pack swimsuit" },
    { "id": "sunscreen", "kind": "action", "title": "Pack sunscreen" },
    { "id": "towel", "kind": "action", "title": "Pack towel" }
  ],
  "edges": []
}
```

### 12.2 Case 2 — Relative Timer

The timer starts when `submit` is checked off (`complete_task`), not at run creation;
`followup` has no row until the waiter fires 4 days later.

```json
{
  "name": "Follow-up",
  "nodes": [
    { "id": "submit", "kind": "action", "title": "Submit application" },
    { "id": "followup_wait", "kind": "time", "title": "Wait 4 days" },
    { "id": "followup", "kind": "action", "title": "Follow up" }
  ],
  "edges": [
    { "from": "submit", "to": "followup_wait", "condition_type": "timer", "condition_value": "4 days" },
    { "from": "followup_wait", "to": "followup", "condition_type": "on_complete" }
  ]
}
```

### 12.3 Case 3 — Recurring Birthday

The recipe is plain sequencing; the schedule row drives annual instantiation. Each
scheduler fire creates a brand new, isolated run.

Recipe (`recipe_json`):

```json
{
  "name": "Birthday",
  "missed_policy": "skip",
  "nodes": [
    { "id": "greet", "kind": "action", "title": "Send birthday greeting" },
    { "id": "call", "kind": "action", "title": "Call for birthday" }
  ],
  "edges": [
    { "from": "greet", "to": "call", "condition_type": "on_complete" }
  ]
}
```

Schedule row (`schedules` table):

```json
{
  "recipe_id": 12,
  "enabled": true,
  "recurrence": { "month_day": 1, "month": 3, "time_of_day": "00:00" },
  "timezone": "America/New_York"
}
```

### 12.4 Case 4 — Gatekeeper (AI + Approval)

`draft` carries `ai: true` so an AI or script may pick it up. Completing it with result
`{"approved": true}` spawns `send`; `{"approved": false}` spawns `edit` (the other
`on_result` edge is cancelled). For the approval-subtask variant (§5.5), the recipe adds
an `"approval": true` action node between `draft` and the branches; its `on_result`
edges branch to `send`/`edit`, and `"retrigger_on_reject": true` re-opens `draft` via
`retry_task` (§6.8).

```json
{
  "name": "Gatekeeper",
  "nodes": [
    { "id": "draft", "kind": "action", "title": "Draft the email", "ai": true },
    { "id": "send", "kind": "action", "title": "Send message" },
    { "id": "edit", "kind": "action", "title": "Edit draft" }
  ],
  "edges": [
    { "from": "draft", "to": "send", "condition_type": "on_result", "condition_value": { "approved": true } },
    { "from": "draft", "to": "edit", "condition_type": "on_result", "condition_value": { "approved": false } }
  ]
}
```

### 12.5 Case 5 — Parallel Research

Three start nodes plus a fan-in target. `final_summary` spawns only when all three
incoming `on_complete` edges are satisfied, in any order.

```json
{
  "name": "Parallel Research",
  "nodes": [
    { "id": "research_a", "kind": "action", "title": "Research topic A" },
    { "id": "research_b", "kind": "action", "title": "Research topic B" },
    { "id": "research_c", "kind": "action", "title": "Research topic C" },
    { "id": "final_summary", "kind": "action", "title": "Write final summary" }
  ],
  "edges": [
    { "from": "research_a", "to": "final_summary", "condition_type": "on_complete" },
    { "from": "research_b", "to": "final_summary", "condition_type": "on_complete" },
    { "from": "research_c", "to": "final_summary", "condition_type": "on_complete" }
  ]
}
```

### 12.6 Case 6 — Conditional Urgency

`urgent` is declared in the params schema; the run-time UI/API collects it. The `timer`
edge's duration expression resolves against `run.parameters`: `true` → 0 days (fires on
next tick), `false` → 3 days.

```json
{
  "name": "Conditional Urgency",
  "params": {
    "urgent": { "type": "boolean", "default": false, "description": "Call immediately when true" }
  },
  "nodes": [
    { "id": "review", "kind": "action", "title": "Review the lead" },
    { "id": "call_wait", "kind": "time", "title": "Wait before calling" },
    { "id": "call_client", "kind": "action", "title": "Call client" }
  ],
  "edges": [
    {
      "from": "review",
      "to": "call_wait",
      "condition_type": "timer",
      "condition_value": { "if": "param:urgent", "then": "0 days", "else": "3 days" }
    },
    { "from": "call_wait", "to": "call_client", "condition_type": "on_complete" }
  ]
}
```

### 12.7 Case 7 — External Event (Webhook)

After `send_email` completes, the engine spawns the tickable waiter `await_reply`
("Await reply from client", `item_type='event'`). `trigger_event(run_id,
"client_replied")` (webhook or UI button) — or ticking the waiter manually — marks it
done and spawns `draft_contract`.

```json
{
  "name": "Contract Follow-up",
  "nodes": [
    { "id": "send_email", "kind": "action", "title": "Send email" },
    { "id": "await_reply", "kind": "event", "title": "Await reply from client" },
    { "id": "draft_contract", "kind": "action", "title": "Draft contract" }
  ],
  "edges": [
    { "from": "send_email", "to": "await_reply", "condition_type": "event", "condition_value": "client_replied" },
    { "from": "await_reply", "to": "draft_contract", "condition_type": "on_complete" }
  ]
}
```

### 12.8 Case 8 — Manual Real-World Await

After `place_order` completes, a `time` waiter is spawned (`blocked_until = now + 3
days`); `mark_received` has no row, so the run's action list is empty. When the waiter
fires, `mark_received` appears as a plain manual checkbox.

```json
{
  "name": "Order Delivery",
  "nodes": [
    { "id": "place_order", "kind": "action", "title": "Place order" },
    { "id": "delivery_wait", "kind": "time", "title": "Wait for delivery" },
    { "id": "mark_received", "kind": "action", "title": "Mark package as received" }
  ],
  "edges": [
    { "from": "place_order", "to": "delivery_wait", "condition_type": "timer", "condition_value": "3 days" },
    { "from": "delivery_wait", "to": "mark_received", "condition_type": "on_complete" }
  ]
}
```
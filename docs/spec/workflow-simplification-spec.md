# Workflow Simplification Spec (v2 of the workflow engine)

Status: Draft v1 (analysis + recommended revised model; supersedes the data model of
`workflow-engine-spec.md`)

Purpose: Answer whether the workflow engine can be simplified by re-using or refactoring
the existing todo-2 database schema concepts, assess problems and UX clarity for each
change, and define the recommended simplified data model.

## 1. Intent

The workflow engine spec (v1) invented run-level machinery — `workflow_runs` with
task-level bookkeeping columns (`item_type`, `node_id`, `workflow_details`), a dedicated
`'workflow_dep'` link kind, `schedules` tables, and a `tick_timers()` loop. The todo-2
schema already models most of what a workflow needs: time-based waiting (`blocked_until`
with a built-in "hide far future" list filter), dependency gating and a computed `blocked`
flag (`task_links` / `blocked_by`), recurrence (`repeat_task_templates` with a materializer),
follow-up tasks (`source_task_id`), subtasks (`parent_id`), and tombstones (`deleted_at`).

This spec evaluates which of those concepts can be reused or refactored to carry
workflows, notes the problems and UX consequences of each, and defines the recommended
simplified model (v2).

## 2. Decisions from the interview (normative for v2)

| Topic | Decision |
| --- | --- |
| Deliverable | Analysis of each reuse/change option + the revised v2 data model (this document) |
| License to change | Full refactor allowed: existing concepts may be renamed/repurposed if the simplification demands it, as long as old user behavior is preserved where it matters |
| Run model | **Hybrid**: a slim `workflow_runs` table for run metadata; workflow steps are ordinary `tasks` rows with a nullable `workflow_run_id` |
| Materialization | **Lazy**: a step's task row is created only when its prerequisites are satisfied (time waits pre-create the downstream task carrying `blocked_until`; `on_result` branches create only the matching branch) |
| Why lazy | The next step's timing can depend on the completion time of the previous task (relative timers start at completion); and users cannot tick a step that does not exist yet |
| Time waits | The downstream action task is created at timer start with `blocked_until = now + duration`; the existing near-only filter hides it when far out; blocked tasks cannot be ticked early |
| Event waits | A visible, tickable task ("Await reply from client") is created when the prerequisite completes; the webhook, a UI button, or manually ticking it all resolve it |
| Fan-in (Case 5) | Lazy create: the engine creates the target when all incoming edges are satisfied |
| Priority interplay | Steps opt in: workflow steps are ordinary tasks; authors may set importance/urgency/deadline, and normal priority scoring applies |
| Result storage | `workflow_runs.step_results` JSON map (`node_id → result`); no result column on tasks |
| Schedules | Reuse `repeat_task_templates` (add a `recipe_id` column); the existing materializer creates a run instead of a task when `recipe_id` is set |
| Cancellation | Rejection branches are never created (nothing to cancel); `cancel_run` tombstones the run's tasks and marks the run cancelled. No per-task cancelled state in v1 |
| Versioning | Keep immutable versioned recipes (slug + version rows); runs pin the exact version |
| Run visibility | A collapsed run header/banner in the list view showing the run's steps; steps also appear as normal tasks |
| Wait visibility | Accept the existing 2-day hide window as-is (a step peeks into view before becoming doable) |

## 3. Existing concept inventory (what each does today)

| Concept | Location | Behavior today |
| --- | --- | --- |
| `parent_id` | `tasks` | Subtask tree; subtasks inherit tags from ancestors |
| `blocked_until` | `tasks` | Time-based block: task sorted to the bottom while blocked, hidden entirely when > 2 days out (`near_only_where_sql`), becomes doable when the timestamp passes |
| `deadline` | `tasks` | Priority pressure: score grows as the deadline approaches and past it; also used by the recurrence materializer as the occurrence anchor |
| `done` / `completed_at` | `tasks` | Completion; completed tasks sort to the bottom and hide after 24h (`COMPLETED_TASK_VISIBLE_SECS`) |
| `task_links` (`blocked_by`) | `task_links` | Dependency edges with cycle protection; a task is `blocked` while any blocker is unfinished; UI shows "blocked by …" |
| `source_task_id` | `tasks` | Follow-up tasks: created via `create_follow_up`, blocked by their source, copy the source's tags, shown under the source's "linked to" list |
| `deleted_at` | `tasks` | Tombstone: rows hidden from lists, kept for sync/mirroring bookkeeping |
| `repeat_task_templates` + `repeat_task_occurrences` | storage | Recurrence engine: `interval_days`, `time_of_day`, `start_time_of_day`, `weekdays`, `month_day`, `strict`, `timezone`, `blocked_by_template_id`; a daily materializer creates one open occurrence per template within a 2-day window, with `blocked_until`/`deadline` from the template times |
| `tags` + `tag_implications` + `direct_task_tags` | storage | Tag DAG with inheritance/inference (not used by v2) |
| `is_seed` | `tasks` | Marker: demo content that must never sync to any integration |
| Priority scoring | queries | `importance_factor × urgency_factor × deadline_pressure` orders the main list |

## 4. Analysis: reuse / change options per concept

For each concept: the options considered, the problems each creates, and the UX clarity
assessment. Every option below is evaluated against the full change license (additive,
extended, or repurposed).

### 4.1 `blocked_until` — time waits (REUSE, chosen)

- **Option A (chosen):** pre-create the downstream action task when the prerequisite
  completes, with `blocked_until = completion + duration`. The relative-timer property
  ("starts exactly when the previous task is checked off") is preserved because the
  timestamp is computed at completion time.
- **Option B:** a separate waiter row or waiter table tracks the delay, and the action is
  created when it fires (v1 model). More state, a tick loop, and a second representation
  of "waiting".
- **Option C:** reuse `deadline` instead of `blocked_until`. **Rejected** — see §4.2.

**Problems (Option A):**
- The existing near-only filter hides tasks > 2 days out. A 4-day wait (Case 2) is
  invisible for ~2 days, then visible at the bottom ("not doable") for ~2 days, then
  doable. A 3-day wait (Case 8) peeks into view for its last day. This softens v1's
  literal "disappears for 3 days, then reappears" wording into "hidden while far out,
  doable exactly at the deadline" — accepted by decision (§2).
- `blocked_until` is a single timestamp, so only one wait per step (sufficient: a step
  has at most one incoming timer edge).
- Nothing re-triggers on the timestamp — there is nothing to do: doability is computed
  by existing list queries. This is a feature: **no `tick_timers()` loop is needed at
  all**.

**UX clarity:** good. The task exists but is visibly "not doable" (bottom of the list,
or hidden while far out), matching what users already understand about recurring-start
tasks. Ticking early is impossible, exactly as with any blocked task.

### 4.2 `deadline` — NOT for waits (REUSE only as authored opt-in)

**Problem:** `deadline` drives priority escalation — the score grows as the deadline
approaches and past it. A workflow wait would escalate priority while it is still
waiting, which is wrong. Using `deadline` for waits is **rejected**.
**Reuse:** unchanged for authored deadlines on steps (opt-in priority, §2).

### 4.3 `task_links` / `blocked_by` — dependencies (REUSE, narrowed role)

- **Option A (chosen):** the engine materializes ordinary `blocked_by` links from a
  satisfied prerequisite task to the task it spawned (audit + "linked to" display). No
  new link kind; the existing cycle protection and UI apply.
- **Option B:** reuse `blocked_by` as the actual gate by pre-creating fan-in targets at
  run start (target shows "blocked by A, B, C" and unlocks when all are done). Zero
  engine fan-in logic, but the target is visible from the start (contradicts the lazy
  "spawns when all done" wording). Rejected by interview (§2: lazy create).

**Problems (Option A):**
- Because steps are created only when prerequisites are satisfied, the linked blockers
  are always already done, so the `blocked` flag never lights up for workflow steps.
  Users won't see a "blocked by" reason for why a step exists. Mitigated by the run
  header/banner (§8), which shows the run's step order.
- The engine must take care that its audit links never create cycles (it never links a
  task to itself or to a descendant; the existing `link_would_create_cycle` guard is
  still applied).
- User-deleted (tombstoned) workflow tasks break edge satisfaction: see §7.4.

**UX clarity:** neutral-to-good. The "linked to" list on a completed task shows the steps
it unlocked ("Follow up", "Await reply"), which reads naturally.

### 4.4 `source_task_id` — follow-ups (REUSE for post-completion steps, chosen)

- **Option A (chosen):** steps created *after* a prerequisite completes are created with
  `source_task_id = <prerequisite task>` and a `blocked_by` link to it, exactly like
  `create_follow_up` (which also copies tags). Case 2's "Follow up" becomes a literal
  follow-up task.
- **Option B:** keep follow-ups user-only; engine steps carry no `source_task_id`.

**Problems (Option A):**
- Follow-ups today are user-created and copy tags from their source. Tag copying for
  workflow steps is desirable (a step belongs to the same project as its run context),
  but the recipe may want its own tags; the spec allows recipe-declared tags to
  override the copy.
- Start nodes (Case 1/5) have no source and are created at `create_run` — they carry no
  `source_task_id`.

**UX clarity:** excellent. "Follow up" appearing under "Submit application → linked to"
is the existing mental model for "do this after that".

### 4.5 `parent_id` — approval subtasks (REUSE, chosen)

- **Option A (chosen):** an approval node (`"approval": true`) materializes as a **child
  task** of the automated task (`parent_id` = automated task row), per v1 §5.5.
- **Option B:** approvals are top-level steps like any other.

**Problems (Option A):** the existing UI treats subtasks as independent tickable items;
there is no "parent cannot complete before children" rule, which is fine — approval is a
separate human decision, not a blocking dependency.

**UX clarity:** good — the approval visibly nests under the AI-generated draft it
reviews.

### 4.6 `repeat_task_templates` — schedules (REUSE with a `recipe_id` column, chosen)

- **Option A (chosen):** add a nullable `recipe_id` column to `repeat_task_templates`.
  When set, the existing daily materializer creates a **run** (`create_run`) instead of a
  task occurrence. Recurrence fields (`interval_days`, `time_of_day`, `weekdays`,
  `month_day`, `strict`, `timezone`) are reused unchanged; the recurrence evaluator is
  shared.
- **Option B:** keep a new `schedules` table (v1) that calls the same evaluator. Duplicate
  shape for no gain.

**Problems (Option A):**
- The materializer's "one open occurrence per template" rule and its 2-day
  `MATERIALIZE_WINDOW_SECS` are tuned for repeating *tasks*. For recipe schedules:
  - The window must widen to the schedule's next-fire date (annual schedules must look
    ~1 year ahead); the anchor logic already advances by `interval_days` steps, so this
    is an adaptation of the loop, not a new engine.
  - "One open occurrence" must be relaxed for schedules: each scheduled date spawns a new
    run regardless of whether the previous run completed (a birthday run must fire every
    year even if last year's is unfinished). Gap handling stays `missed_policy`
    (`skip`/`catch_up`) on the recipe; the template's latest run `created_at` is the
    `last_fired` watermark.
- `repeat_task_occurrences` links occurrences to a *task*; run-based templates need a
  parallel mapping (a `workflow_runs.schedule_id` column suffices — no new link table).

**UX clarity:** one recurrence mental model for both repeating tasks and recurring
workflows; advanced feature, acceptable.

### 4.7 `deleted_at` tombstone — cancellation (REUSE, chosen)

- **Option A (chosen):** `cancel_run` tombstones the run's task rows and marks the run
  `cancelled`. Rejected `on_result` branches were never created (lazy), so there is
  nothing to cancel on the branch level.
- **Option B:** a visible "cancelled" state column on tasks. More schema, more UI.

**Problems (Option A):** tombstones hide rows, so cancelled steps vanish from lists
(accepted: cancellation is a run-level action in v1). A `cancelled` run remains visible
in the run history with its status. Users cannot cancel a single step in v1 (deferred,
§9).

**UX clarity:** acceptable for v1; the run header makes cancellation discoverable.

### 4.8 Priority/urgency/deadline on steps — opt in (REUSE, chosen)

Steps are ordinary tasks; authors set factors and deadlines per node, and the normal
scoring applies. **Problem:** active workflow steps can crowd the main priority list;
mitigated by the run header grouping (§8) and by authors leaving factors at defaults.
**UX:** a deliberate choice — workflow steps compete like real tasks when the author
wants them to.

### 4.9 What is NOT reused

- **Tags as runs** — rejected in interview; tags are user-organizational, runs are
  engine state.
- **`is_seed`** — not reused as the workflow marker; instead the sync layer must skip
  tasks with `workflow_run_id IS NOT NULL` (a distinct, explicit rule, §7.2).
- **`workflow_details` / `item_type` / `'workflow_dep'` / `schedules` / `tick_timers` /
  waiter tables** — all dropped in v2 (§5, §6).

## 5. Recommended revised data model (v2)

### 5.1 New/changed tables and columns

**`workflow_recipes`** (unchanged from v1): `id`, `slug`, `version`, `recipe_json`,
`created_at`. Immutable; runs pin the exact row.

**`workflow_runs`** (new, slim):

| Column | Type | Meaning |
| --- | --- | --- |
| `id` | INTEGER PK autoincrement | Run id; referenced by `tasks.workflow_run_id`. |
| `recipe_id` | INTEGER FK → `workflow_recipes.id` | Exact recipe version. |
| `schedule_id` | INTEGER, nullable FK → `repeat_task_templates.id` | Set when scheduler-created. |
| `status` | TEXT | `'active'`, `'completed'`, `'cancelled'`. |
| `params` | JSON | Runtime params (§5.4 of v1). |
| `step_results` | JSON | Map `node_id → result` recorded by `complete_task`; consumed by `on_result` edges. |
| `created_at` | jiff::Timestamp | |
| `completed_at` | jiff::Timestamp, nullable | |

**`tasks`** (two new nullable columns — nothing else):

| Column | Meaning |
| --- | --- |
| `workflow_run_id` | FK → `workflow_runs.id`, indexed. `NULL` for ordinary tasks. |
| `node_id` | The recipe node this row materializes (engine bookkeeping; needed to evaluate outgoing edges and to find event waiters). |

No `item_type`, no `workflow_details`, no status column — a workflow step is a task like
any other; waits use `blocked_until`, dependencies use `task_links`, results live on the
run.

**`task_links`** (unchanged schema): reused with the existing `kind = 'blocked_by'` for
audit links (§4.3). No `'workflow_dep'` kind.

**`repeat_task_templates`** (+1 nullable column): `recipe_id` FK → `workflow_recipes.id`;
when set, materialization creates a run (§4.6). No `schedules` table.

**Dropped from v1:** `schedules` table, `tasks.item_type`, `tasks.workflow_details`,
`task_links` kind `'workflow_dep'`, `tick_timers()`, waiter rows.

### 5.2 Migration sketch

1. `CREATE TABLE workflow_recipes (...)`; index on `(slug, version)`.
2. `CREATE TABLE workflow_runs (...)`; index on `recipe_id`, `status`.
3. `ALTER TABLE tasks ADD COLUMN workflow_run_id INTEGER`; index.
4. `ALTER TABLE tasks ADD COLUMN node_id TEXT`.
5. `ALTER TABLE repeat_task_templates ADD COLUMN recipe_id INTEGER`.
6. No `schedules` table, no `item_type`, no `workflow_details`.

## 6. Revised engine behavior

### 6.1 `create_run(recipe_id, params, schedule_id?)`

1. Validate params/durations against the recipe (§5.8 of v1, unchanged).
2. Insert `workflow_runs` row (`status='active'`, `params`, `step_results={}`,
   optional `schedule_id`).
3. Materialize **start nodes** (no incoming edges) as task rows:
   `workflow_run_id`, `node_id`, title/description from the recipe, `caused_by`
   equivalent not needed (v2 has no `workflow_details`; provenance is the run + `node_id`).
4. Return the run.

### 6.2 `complete_task(task_id, result)`

1. Mark `done=true`, `completed_at=now`; record `step_results[node_id] = result` on the
   run row.
2. Load the node's outgoing recipe edges (from the run's pinned recipe version).
3. For each edge, evaluate:
   - **`on_complete`** — if the target's incoming edges are all satisfied (every source
     node has a done, non-tombstoned task row in this run), create the target task row
     (lazy). Add `blocked_by` audit links from each satisfied prerequisite row.
   - **`on_result`** — key-based match (§6.4 of v1, unchanged): match → create the target
     task row (+ audit links); no match → create nothing (branch never existed).
   - **`timer`** — resolve the duration against `run.params` (§5.4 of v1, unchanged),
     compute `scheduled_at = now + duration`, then **create the target task row
     immediately with `blocked_until = scheduled_at`**. No waiter, no loop — existing
     queries handle visibility/doability. If the target is an `event` edge instead, see
     below.
   - **`event`** — create the **event-waiter task row**: title from the event node (e.g.
     "Await reply from client"), tickable. It is found later by `node_id` + the recipe
     (`trigger_event`, §6.3). Completing it manually is a normal `complete_task`.
4. Run the auto-complete check (§6.4).

Approval subtasks (§4.5): when an `ai: true` node completes and the recipe declares an
`"approval": true` node fed by its `on_result` edge, the approval node materializes as a
**child task** (`parent_id` = the automated task row) instead of a top-level task.

`retry_task` (unchanged in spirit from v1 §6.8, simplified): reset the automated task
(`done=false`, `completed_at=null`, delete `step_results[node_id]`), tombstone tasks that
were created from its previous completion (walk its outgoing `blocked_by` audit links),
re-arm its edges. Triggered by `"retrigger_on_reject": true` on an approval node.

### 6.3 `trigger_event(run_id, event_name)`

Find the run's pending event-waiter task rows: `workflow_run_id = ? AND node_id IN
(<event node ids of the recipe>) AND done = 0 AND deleted_at IS NULL`, where the recipe
node's incoming `event` edge carries `event_name`. For each matching row: mark it done
(`completed_at=now`) and evaluate its outgoing edges (§6.2) — this creates the downstream
action lazily. Manual ticking of the waiter is the identical code path via
`complete_task`.

### 6.4 Run lifecycle / auto-complete

`status = 'active' | 'completed' | 'cancelled'`.

A run auto-completes when **every task row of the run is done** (none pending, none
tombstoned). Because targets are created lazily the moment their prerequisites are
satisfied, "all tasks done" implies no pending work: any still-satisfiable node would
already have been created; rejected `on_result` branches have no row and nothing pending.
A pending task — including a pre-created `blocked_until` step or an event waiter — keeps
the run active.

`cancel_run(run_id)`: tombstone (`deleted_at`) every task row of the run, set
`status='cancelled'`.

### 6.5 Scheduling (Case 3)

The existing daily materializer, extended: for templates with `recipe_id` set, an
"occurrence" is a run. The recurrence evaluator (`interval_days`, `time_of_day`,
`weekdays`, `month_day`, `strict`, `timezone`) is shared unchanged; the window is
widened to the schedule's next-fire date and the "one open occurrence" rule is relaxed
to "one run per scheduled date" (§4.6). `missed_policy` (`skip`/`catch_up`) and the
watermark (`last_fired_at` = the template's latest run `created_at`) follow v1 §6.5.

## 7. Existing-behavior impact and problems to watch

### 7.1 List queries

Workflow tasks flow through the existing `list_tasks_by_priority` / `list_tasks_by_tag`
machinery (priority scoring, `blocked_until` sorting/hiding, done-task visibility). No
query changes needed; the run header (§8) queries `workflow_runs` directly.

### 7.2 Sync (Todoist) — REQUIRED guard

Workflow tasks are engine state, not user tasks: the push path must **skip tasks with
`workflow_run_id IS NOT NULL`** (same treatment as `is_seed`), and imports must never
relink or resurrect them. Without this guard, workflow steps would leak to Todoist and
come back as duplicates. This is the one hard behavioral change the reuse imposes on
existing code.

### 7.3 Deletion semantics

`blocked_by` audit links are engine-written; the existing cycle guard must be applied to
them (it already is, since they go through `add_blocker`). User-created links are
unaffected.

### 7.4 User deleting a workflow step

Deleting a workflow task (tombstone) abandons its node: outgoing edges are treated as
unsatisfiable — fan-in collapse applies (v1 §6.4), the run auto-completes once everything
else is done. Documented behavior; per-step skip/cancel is deferred (§9).

### 7.5 Wait-visibility wording delta

The v1 acceptance wording "disappears for 3 days, then reappears" becomes, with the
2-day hide window: "hidden while more than 2 days out; listed at the bottom as not
doable until `blocked_until` passes; doable exactly on schedule" (accepted, §2).

## 8. UI (minimal contract)

- **Run header/banner:** a collapsed run group at the top of the relevant view listing
  the run: recipe name, status, params, and its steps (pending steps with wait
  countdowns, event waiters tickable, done steps struck). Driven by `workflow_runs` +
  the run's task rows.
- **Steps as normal tasks:** engine-created action tasks appear in the main list with
  normal behavior (opt-in priority); waiters peek at the bottom while `blocked_until` is
  future and are hidden when far out.
- **Start a run:** a form from the recipe's `params` schema → `create_run`.
- **Fire an event:** a button in the run header, or the loopback webhook
  `POST /webhook/events` `{"run_id", "event_name"}` (v1 §9, unchanged: 204/400/404/405,
  loopback-only, optional `X-Webhook-Token`).

## 9. Deferred / non-goals (v2)

- Per-step user cancellation or "skip" (v1 had `cancel_task`; v2 deliberately drops it —
  only `cancel_run`).
- A dedicated runs list/history screen (the header/banner is the v1 surface).
- Recipe editor UI, OR-triggers, string-param pattern validation (unchanged from v1).
- Tags as run identity (rejected in interview).

## 10. Delta — changes to apply to `workflow-engine-spec.md`

When v2 is adopted, the v1 spec changes as follows:

1. **§4 Data model:** delete `schedules` table; delete `tasks.item_type` and
   `tasks.workflow_details`; `workflow_runs` gains `schedule_id → repeat_task_templates`
   and `step_results`; `tasks` gains only `workflow_run_id` + `node_id`;
   `repeat_task_templates` gains `recipe_id`; §4.6 rewrite (reuse `blocked_by`, no
   `'workflow_dep'`).
2. **§4.4 recurrence:** move schedule evaluation under `repeat_task_templates`; keep the
   DST/leap rules.
3. **§6.2/6.3:** materialize timer targets with `blocked_until` instead of waiter rows;
   `trigger_event` finds waiters by `node_id`; **delete §6.5 `tick_timers()`** (nothing to
   tick); **delete §6.6 `cancel_task`** (keep `cancel_run` = tombstone); simplify §6.7
   auto-complete to "all task rows done"; §6.8 `retry_task` cascade via `blocked_by` links.
4. **§8 case walkthroughs:** update Case 2/8 wording per §7.5.
5. **§9 UI:** add the run header/banner; waiter visibility wording per §8.
6. **New §7.2:** sync guard (skip `workflow_run_id IS NOT NULL` rows).
7. **§10 resolved decisions:** replace waiter-representation and `task_links`-kind items
   with the v2 decisions in §2 of this document.

## 11. Summary

Reusing the existing schema shrinks the workflow engine from five new concepts
(`schedules`, `item_type`, `workflow_details`, `'workflow_dep'`, `tick_timers`) to two
new tables (`workflow_recipes`, `workflow_runs`) plus three nullable columns
(`tasks.workflow_run_id`, `tasks.node_id`, `repeat_task_templates.recipe_id`). Time waits,
dependencies, recurrence, follow-ups, approval subtasks, and cancellation all map onto
machinery users already understand (`blocked_until`, `blocked_by`, `source_task_id`,
`parent_id`, `repeat_task_templates`, `deleted_at`). The one hard cost is the sync guard
(§7.2) and the softened wait-visibility wording (§7.5); the one deliberate loss is
per-step cancellation (§9).
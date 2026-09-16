# Coding Workflow Subtasks and Spec Coverage — Spec

Status: Proposed (no code written; this spec is the output of the interview)
Relates to: `docs/spec/coding-workflow-runs-spec.md` (the run, its phases, the
stepper), `docs/spec/github-integration-spec.md` (§5.5 and §5.9 — sub-issue ↔
subtask sync), `docs/spec/workflow-simplification-spec.md` (v2 data model,
`RecipeNode.subtask` lazy spawn), `docs/spec/todoist-sync-spec.md` (§3.5 nesting).

## 1. Intent

Today a coding run's phase steps **are** subtasks of the feature task
(`RecipeNode.subtask: true` → `tasks.parent_id = run root`), and the details
pane already hides them from its own `Subtasks (N)` list. That makes two very
different things look like the same thing:

- a **phase step** — run scaffolding the engine materializes, owns and advances;
- a **subtask** — a breakdown item the user or the model adds, which may be a
  one-line detail ("bump the changelog") or a real chunk of work.

The request this spec answers:

> Currently workflow steps are subtasks. Subtasks can also be small details
> added manually that don't necessarily need their own AI interview. How to mix
> the possibility for the user to add small steps, big steps, and the workflow
> to add steps?
> If the user specifies multiple things to do in a task's subtasks, surely the
> interview should ask about all of them, then update the spec individually for
> each subtask. The user can then modify a subtask, or add more subtasks, which
> might invalidate the interview (missing spec for a smaller newer subtask). How
> to make this clearer?

So there are two problems, and they are separable:

1. **Identity.** Steps and subtasks are the same rows, so nothing in the UI, and
   nothing in the app's own queries, can say "this is scaffolding" versus "this
   is work I added". The fix is to give a step an explicit role and keep it out
   of every subtask surface — for coding recipes only, leaving the generic
   recipe-node `subtask` lazy spawn intact for other workflows.
2. **Coverage.** The interview's input today is `build_task_context(task)`:
   title, description, tags — **never the subtasks**
   (`apps/todo-2/src/ui_parts/agent_pane.rs:197`). So a feature's subtasks are
   invisible to the interview that is supposed to spec the feature, and a
   subtask added afterwards is silently uncovered. The fix is an explicit,
   visible coverage model: an umbrella spec on the feature task, a spec per real
   subtask, an explicit "covered by the feature spec" mark, and a coverage line
   that says what is missing.

Design stance, stated up front: **coverage is documentation of scope, not a
gate.** A missing spec is flagged, never silently ignored and never blocking —
except at the merge step, which will not finish while subtasks are still open.

## 2. Interview decisions (normative)

| # | Topic | Decision |
| --- | --- | --- |
| 1 | Steps vs subtasks | Separate the two for the **coding workflow only**. Generic recipes keep `RecipeNode.subtask` lazy spawn, so approval subtasks and other workflows are untouched. |
| 2 | Step storage | Keep `parent_id` on steps (smallest engine change) and add a **persisted role** on the task that every subtask surface filters on. |
| 3 | Role declaration | The recipe node declares the role explicitly (`role: "step" \| "subtask"`); it is **not** implied by `phase`. |
| 4 | Step visibility | A step exists **only in the run's Workflow section**. Not in the task list, not in `Subtasks (N)`, not in GitHub sub-issue sync. |
| 5 | Interview scope | Every subtask that exists **when the interview starts** (steps are never in scope). |
| 6 | What counts as covered | A stored subtask spec, or an explicit "covered by the feature spec" mark. **A description never counts.** |
| 7 | Who decides | **The model, during the interview**, decides per subtask: write its spec, or mark it covered. |
| 8 | Spec shape | The root keeps the **umbrella** spec; every real subtask may hold **its own** `tasks.spec`. |
| 9 | Who writes subtask specs | The **root interview** writes them. The user may launch a sub-interview later to deepen one. |
| 10 | Coverage state | Derived: `own` = `tasks.spec` non-empty; `covered` = a small mark on the task; `unspecced` = neither. |
| 11 | Coverage count | `spec covers x/y subtasks`, denominator = **open** subtasks. Done subtasks are never flagged and leave the count. |
| 12 | New subtask after the interview | **Flagged, never blocking.** The state and fix actions live in that subtask's own details. |
| 13 | Edited subtask | Editing a subtask's title/description **never** invalidates a spec; only missing coverage is flagged. |
| 14 | Model-created subtasks mid-run | Same rule as any other subtask — unspecced until specced or covered. |
| 15 | Breakdown under a step | **No more parenting to steps**: in a coding run `create_sub_task` always parents to the run root, so every subtask is a direct child and step rows have no children. |
| 16 | Promotion | Any subtask, any time ("give it its own run"). No spec required; with none, its run starts at the interview phase. |
| 17 | Nested depth | Recursive: a promoted run uses the same umbrella + per-subtask coverage model. |
| 18 | Implement input | Umbrella **+ every open subtask spec** inlined in the one composed prompt. |
| 19 | Merge gate | The merge/PR action **refuses while open subtasks exist**, with a waiver affordance. |
| 20 | Gap signal | Only in the subtask's **own details** — no row markers, no banner by the spec artifact — plus the coverage line in the run meta. |
| 21 | Spec artifact | The umbrella spec block lists per-subtask specs as **nested expandable rows**. |
| 22 | Fix actions | All three, in this order: reopen the interview (a **real rewind**, #26) / start a sub-interview / write the spec by hand. Plus promote-to-run. |
| 23 | Tool shape | **One `save_spec`** carrying the umbrella plus the subtask specs and the covered ids. |
| 24 | Interview prompt | The app **enumerates** the open subtasks (id + title) and states the coverage contract. |
| 25 | Vocabulary | Run section = **Workflow** (rows are *steps*); tree = **Subtasks (N)**; states = *specced* / *unspecced* / *covered by the feature spec*. |
| 26 | Rewind scope | **Reopen the interview** works in **any phase**, including mid-implement, and is a *real* rewind: the current phase stops and the run goes back to the interview. It is not a late "write specs only" pass. |
| 27 | Rewind gate | Blocked while the phase's agent turn is **running**. The active phase row carries a **Stop** control (the agent pane's stop-turn, surfaced on the row); stopping leaves the step open and the branch as it is, and only then does the rewind become available. A stopped step reads as an ordinary pending step — no separate "Stopped" state. |
| 28 | Rewind confirm | A confirm dialog **only when a phase is open** (past the spec gate): it names what stops and states that commits stay on the branch. At the interview-only state there is nothing to interrupt, so no dialog. |
| 29 | Rewind accounting | Logged as **`kind: "reject"`** with a body — no new note kind — so `Round N` (1 + rejections) numbers every cycle truthfully. |
| 30 | Step reuse | Nothing is **tombstoned** by a rejection or a rewind: the interview row is **re-used in place** (today's `retry_task`: `done=false`, `step_results["interview"]` cleared), the spec gate gets a **fresh row each round** (round 1's approved spec stays visible), and pending implement/review/merge rows are **re-used**. Engine rule: a node with an open step row in the run re-uses it; a node whose rows are all done spawns a fresh one. |
| 31 | Step specs | `tasks.spec` is **never** written on a step row: the pane offers the spec editor only for `role <> 'step'`, and `save_spec` / `save_task_spec` reject a step id. |
| 32 | Promoted subtask | It keeps **syncing its own GitHub issue**, its nested run's branch/PR name and `Closes #n` use **its own** issue, it **stays in `Subtasks (N)`** and in `spec covers N/M` **counted as covered**, its row shows the nested run's state and links into it (no coverage chip — #20), its own details read **`Covered by its own run`**, and the parent's merge refuses while it is open (waivable — #19). |
| 33 | Coverage history | `spec_covered_at` stays a plain timestamp (no round number stored); the round log and the spec versions carry that history. |

Three earlier answers were superseded during the interview and are recorded here
so the transcript is not misleading:

- "a model-created subtask with a description counts as specced" →
  **superseded by #6**: descriptions never count, whoever wrote them.
- "the coverage denominator is every subtask" → **superseded by #11**: done
  subtasks drop out (and are never flagged).
- "interview and spec rows are both respawned each round, one row per phase
  always" → **superseded by #30**: the interview row is re-used in place and
  only the spec gate is fresh per round.

## 3. Current state (verified in this repo)

### 3.1 Steps are subtasks

- `RecipeNode.subtask` (`libs/storage/src/workflow.rs:63`) makes the engine
  materialize a node's task under the run root (`create_task_run`,
  `spawn_node_task`); the seeded `coding-task` recipe sets `"subtask": true` on
  all five nodes (`coding_recipes()`, `libs/storage/src/workflow.rs:241`).
- The details pane therefore filters steps out of its own list by hand:
  `plain_subtasks` keeps `subtask.node_id.is_none()`
  (`apps/todo-2/src/ui_parts/task_details.rs`, in `relationships_section`).
- The task list does **not** filter them: `compute_row_specs` +
  `subtasks_map` (`apps/todo-2/src/ui_parts/task_list.rs:489`, `:901`) nest
  every task whose `parent_id` is visible, so phase steps render as collapsed
  subtask rows under the feature task today.
- The MCP context filters the same way (`coding_mcp.rs:434`,
  `.filter(|task| task.node_id.is_none())`).
- GitHub sync skips them by a different rule — anything with
  `workflow_run_id` set, and anything under one (`github-integration-spec.md`
  §5.5, §5.9.5).
- So the same concept is currently expressed three ways (`node_id`,
  `parent_id`, `workflow_run_id`) in three places, with no single fact to read.

### 3.2 The interview never sees subtasks

- `phase_prompt("interview")` (`task_details.rs`) is `INTERVIEW_BASE_PROMPT` +
  the task's title + description (+ re-spec notes on later rounds).
- `build_task_context` (`agent_pane.rs:197`) returns `Task: <title>`,
  description, `Tags:` — nothing else, and its doc comment calls it "the single
  seam for what the agent is told about the task".
- `get_coding_context` does return `sub_tasks[]` (`coding_mcp.rs`, built from
  `list_subtasks(root).filter(node_id.is_none())`), but only for a run that
  already exists and only if the model asks — the prompt never points at it.

### 3.3 What exists to build on

- **Sub-interviews**: `coding-sub-interview` (single `interview` node, phase
  `interview`, no edges) rooted at a subtask, created by
  `request_sub_task_interview` (`store.rs:1555`, MCP tool at `coding_mcp.rs:267`),
  launched from the model's sub-task row in the pane. Its `save_spec` writes
  `tasks.spec` on the subtask (the run's root), so **per-subtask specs already
  work** — they are simply not part of the feature's coverage story.
- **Umbrella spec**: `tasks.spec` / `tasks.spec_path` on the root, rendered by
  `coding_spec_artifact`, written by the agent's `save_spec` tool or by hand via
  the root's `Write manually` box (`save_coding_spec`, `store.rs:1337`).
- **Recipe versioning**: recipes are immutable; `ensure_coding_recipes`
  (`workflow.rs:1814`) inserts only when the slug is missing, and
  `recipe_id_by_slug` returns the highest version.
- **Migration tail**: `0024_isolated_build_cache.sql`, so this spec takes
  `0025_*`.
- **No staleness concept exists anywhere** (grep for `stale`/`invalidat` finds
  nothing in the task/spec paths), so §5 is greenfield.
- **Retrigger path**: `retry_task` re-opens the feeder of a rejecting approval
  (`done=false`, `step_results[node_id]` cleared) and tombstones its pending
  children. For `coding-task` the feeders are the `interview` node (`retrigger_node`
  of the spec gate) and `review`, so a rejection already re-opens the interview
  **row in place** — decision #30 only removes the tombstoning.
- **A phase runs in the agent pane**: the details pane emits
  `CodingLaunch { phase, prompt }`, the Layout inserts the text into the agent
  pane and the *user* sends it (`main.rs`), and the pane owns the turn
  lifecycle including its `Stop the turn` button (`agent_pane.rs`). So "is this
  phase running" is a pane fact, not a run fact, and there is no per-phase stop
  on the run today (only the run-level `Cancel run`, which tombstones the steps).

## 4. The model on one page

```
feature task (run root)                          Workflow section (steps only)
├─ umbrella spec  (tasks.spec on the root)        ◐ interview & spec
│                                                 ☐ approve the spec
├─ subtask A   spec: own     → specced            ☐ implement the feature
├─ subtask B   mark: covered → covered            ☐ review & annotate
├─ subtask C   (open, none)  → UNSPECCED ← flagged ☐ merge the branch
└─ subtask D   (done, none)  → ignored
                                                run meta: Round 1 · spec covers 2/3 subtasks · branch …
```

- **Subtask surfaces** = the task list's collapsed rows, `Subtasks (N)` in the
  pane, the GitHub sub-issue tree, the coverage count. All four read one
  persisted fact (role) and all four handle only `role <> 'step'` rows.
- **Workflow section** = the run's steps, in run order, with their actions.
  Steps are never subtasks and subtasks are never steps.
- **Coverage** = per-subtask state, surfaced where the user can act on it (the
  subtask's own details) and summarised once (`spec covers x/y subtasks`).
- **Promotion** = a subtask becomes a run of its own (decision #16). It stays a
  subtask of the parent (in the list, in the count, counted covered) and keeps
  syncing its own issue; the work moves into the nested run (#32).

## 5. Data model and engine

### 5.1 Migration `0025_coding_step_role.sql`

```sql
-- 'step' for engine-materialized run steps; NULL for everything else
-- (plain tasks, user subtasks, model subtasks, nested-run roots).
ALTER TABLE tasks ADD COLUMN role TEXT;

-- when the feature spec was accepted as covering this subtask
ALTER TABLE tasks ADD COLUMN spec_covered_at TEXT;

CREATE INDEX IF NOT EXISTS idx_tasks_role ON tasks(role);

-- backfill: every existing engine step is exactly the rows that carry a node
-- (user and model subtasks have node_id NULL), so no recipe read is needed.
UPDATE tasks SET role = 'step' WHERE node_id IS NOT NULL AND workflow_run_id IS NOT NULL;
```

`role` is deliberately nullable with a single meaningful value today: `'step'`.
A "subtask" is structural (`parent_id` set, `role IS NULL`), which keeps the
distinction free for subtasks created by the UI, the model, a recipe's lazy
spawn, or a nested-run root — none of which need a new value written.

The `tasks` select lists are positional (`libs/storage/src/task.rs`, and the raw
`SELECT`s in `workflow.rs`): both new columns must be appended in one place and
every positional index updated, as the runs spec already warns.

### 5.2 Recipe node role

```rust
pub struct RecipeNode {
    // …existing, including `subtask: bool`…
    /// Coding recipes: `step` materializes a run step that is not a subtask.
    /// `subtask` keeps today's lazy spawn. Exactly one of the two.
    pub role: Option<String>,           // "step" | "subtask"
}
```

`parse_recipe` additions:

- `role` ∈ {`step`, `subtask`} (anything else → the existing user-facing error
  path).
- `role` and `subtask: true` together are rejected (one declaration, no
  ambiguity); `role: "subtask"` is the explicit spelling of today's `subtask:
  true`.
- `role: "step"` is only valid on `action` nodes, and requires `phase` (a step
  always belongs to a phase; the pane's workflow section is keyed on phases).

### 5.3 Seeding `coding-task` v2

`coding_recipes()` gains `coding-task` **v2**: same five nodes, `"subtask":
true` replaced by `"role": "step"`, everything else (ids, phases, approval,
retrigger, edges) unchanged; `coding-sub-interview` stays as it is (its single
node is not a step — it is a standalone interview run, and a one-node step list
would be noise).

`ensure_coding_recipes` must change from "insert if missing" to **"insert if
missing, upsert a newer version of a changed built-in"**: compare the declared
version with `recipe_id_by_slug`'s highest version and call `create_recipe` for
the new one. Runs pin a recipe row, so existing runs keep v1 and its
`subtask: true` semantics — a run started before the migration keeps its steps
as subtasks (see §10, "Run started before the change").

### 5.4 Engine changes

- `spawn_node_task`: when the node's role is `step`, set `tasks.role = 'step'`
  on the spawned row. `parent_id` stays the run root (decision #2), so every
  existing engine path (completion walk, retrigger, the step list read) keeps
  working untouched.
- **Re-use before spawn** (decision #30): `spawn_node_task` first looks for an
  **open** step row for that node in this run (not done, not tombstoned) and
  returns it instead of inserting. A node whose rows are all done gets a fresh
  row. That one rule produces the intended geometry with no extra state: the
  re-opened `interview` row is picked up again, each round's spec gate is a new
  row (the previous one completed), and a pending `implement` survives a rewind
  instead of being duplicated by the new cycle.
- New read helper used by everything:

  ```rust
  /// The run's real subtasks: direct children of the root that are not steps.
  pub async fn run_subtasks(&mut self, root_task_id: u64)
      -> QueryResult<Vec<TaskWithMeta>>;
  ```

- Coverage helpers (pure, testable):

  ```rust
  pub enum SubtaskCoverage { Own, Covered, Unspecced, NotApplicable } // done → NotApplicable

  pub struct CoverageSummary { pub covered: usize, pub total: usize }

  pub fn subtask_coverage(task: &TaskWithMeta) -> SubtaskCoverage;
  pub fn coverage_summary(subtasks: &[TaskWithMeta]) -> CoverageSummary;
  ```

  Rules: `done == true` → `NotApplicable` (excluded from `total`, never
  flagged); else `spec` non-empty → `Own`; else `spec_covered_at.is_some()` →
  `Covered`; else `Unspecced`.

- `save_subtask_specs(root_task_id, umbrella, umbrella_path, subtask_specs,
  covered_ids)`: one transaction that writes the umbrella on the root, each
  subtask's spec, each covered id's `spec_covered_at`, appends the `spec` note,
  and completes the open `interview` step. Guard: every id must be a **direct
  child of `root_task_id` with `role <> 'step'`** (tool error otherwise), so a
  hallucinated id cannot write into another task.
- `cover_subtask_at` is set (not cleared) when the same subtask is later given
  its own spec: an own spec wins over the mark in `subtask_coverage` ordering,
  and the mark stays as history.
- `retry_task` / re-spec (decision #30): it **no longer tombstones the pending
  children**. It re-opens the approver's `retrigger_node` (the `interview` step:
  `done=false`, `step_results["interview"]` cleared) and appends the `reject`
  note. Everything else stays: the interview row in place, the completed steps
  (an approved or rejected spec gate) as history, and the pending
  implement/review/merge rows as pending. Subtask specs and coverage marks are
  **kept** (still true statements about those subtasks); the new round only
  handles what it enumerates.
- `stop_coding_phase(step_id)`: ends that step's phase agent turn (the same stop
  the agent pane's **Stop the turn** performs, keyed on the run's checkout) and
  leaves the step open (`done = false`) with whatever the turn already wrote
  left on the branch. Nothing else about the run changes — the phase simply
  stops running, which is what unblocks the rewind (decision #27).

### 5.5 Filtering (the four subtask surfaces)

| Surface | Change |
| --- | --- |
| Task list (`subtasks_map` / `list_subtasks`, `task_list.rs:489`, `:901`) | exclude `role = 'step'` rows, so steps no longer render under the feature task. |
| Details pane `Subtasks (N)` (`task_details.rs:2901`, `plain_subtasks`) | same filter (today it filters `node_id.is_none()`); with #15 the list and the task list now agree exactly. |
| MCP `get_coding_context.sub_tasks[]` (`coding_mcp.rs:434`) | same filter, plus the per-subtask `spec` state (§6). |
| GitHub sub-issue sync (`libs/storage/src/github.rs`, spec §5.9) | the exclusion becomes explicit and narrower: a task syncs unless it **or an ancestor** is `role = 'step'`. A promoted subtask (which carries a run of its own — decision #32) therefore **keeps syncing its own issue**, and an outer run root no longer blocks its child's sync. |

## 6. MCP tool surface

### 6.1 `get_coding_context` — the model's enumeration

`sub_tasks[]` entries gain the coverage facts:

```json
{ "id": 41, "title": "Add token refresh", "done": false,
  "spec": "own" | "covered" | "none",
  "description": "…", "nested_run": false }
```

`covered`/`own` are the same states the pane shows, computed by
`subtask_coverage`, so the model reads exactly what the user sees.

### 6.2 `save_spec` — one call, umbrella plus coverage

```json
{
  "task_id": 12,                    // the run root (unchanged meaning)
  "content": "…umbrella markdown…", // required
  "path": "./docs/spec/add-oauth-spec.md",
  "subtasks": [                     // optional; ids must be direct subtasks
    { "task_id": 41, "spec": "…subtask spec markdown…" }
  ],
  "covered": [42, 43]               // optional; ids marked covered by the umbrella
}
```

Behaviour: writes the umbrella, each subtask spec, each covered mark, appends
the `spec` note, and completes the open `interview` step of that run — exactly
as today, so the phase machine is unchanged. A subtask id that is not a direct,
non-step child of `task_id` returns a tool error and writes nothing, and so does
a **step** id in either list (decision #31).

### 6.3 `create_sub_task` — never under a step

In a coding run (the parent resolves to a run root with phases), the tool
**always** parents to the run root, even when the model passes a step's task id
as `parent_task_id`; the answer echoes the parent actually used so the model can
see the correction. `nested: true` still starts a nested `coding-task` run
(decision #16 is the *user* path; the model keeps its existing ability).

### 6.4 No new tools

`request_sub_task_interview` stays as it is (it is fix action #2). Promotion is
user-only (no tool) — the model asks the user in prose instead, and the pane is
the only place that starts a promoted run.

## 7. UX — details pane

### 7.1 Sections, in order (run root selected)

```
… ordinary fields, Links, Subtasks (2) …

Workflow                                        ← steps only, run order
  ☑ interview & spec                                       done
  ◐ approve the spec                          [ Approve spec ]
  ☐ implement the feature
  ☐ review & annotate
  ☐ merge the branch
Round 1 · spec covers 2/3 subtasks · branch feature/42-add-oauth · active   [ Cancel run ]
[ Mark implemented ]                            ← step secondaries (unchanged)
▾ Round log (1 cycle)
▾ Spec (42 lines)                               ← umbrella + nested subtask specs
    ▾ Add token refresh (11 lines)
    ▸ Wire the callback URL (6 lines)
```

Changes against today:

- The step list is labelled **Workflow** (decision #25). Nothing else about the
  rows changes (glyphs, actions, PR sub-items, nested-run badges stay).
- The run meta line gains the coverage clause when the run root has subtasks:
  `spec covers 2/3 subtasks`, and, when any are uncovered, the count is tinted
  like the existing warning chips (not an error style — decision #6/#12).
- The spec block lists the umbrella first, then one collapsible row per subtask
  that has its own spec, labelled with the subtask title and its line count.
- The row of a phase that is **running** carries a **Stop** action, and the
  rewind only becomes available once it stops (decisions #27/#28).
- A rewind adds a round without rewriting history (decision #30): the same
  interview row re-opens, a **new `approve the spec` row** appears for the new
  round, and the earlier cycle's rows stay. Rows stay ordered by **phase**, so
  each round's spec gate sits directly under the interview row it belongs to:

  ```
  Workflow
    ☑ interview & spec                                  re-opened, round 2
    ☑ approve the spec                                  approved · round 1
    ◐ approve the spec                                  [ Approve spec ] · round 2
    ☐ implement the feature
    ☐ review & annotate
    ☐ merge the branch
  ```

### 7.2 A subtask's own details (selected subtask)

A new **Spec** block under the ordinary fields, only for real subtasks
(`role <> 'step'`) of a feature task that has an active or completed run:

```
Spec
  No spec yet                                  ← state line, one of:
                                                  "No spec yet"
                                                  "Specced · 11 lines"
                                                  "Covered by the feature spec"
                                                  "Covered by its own run"  ← promoted (#32)
  [ Reopen the interview ]  [ Start a sub-interview ]  [ Write the spec ]
  [ Give it its own run ]                      ← promotion, always available
```

- **Reopen the interview** — a **real rewind** of the run (decision #26),
  available in any phase. It is disabled while the current phase's agent turn
  runs, with the tooltip naming the **Stop** control that clears it (#27); once
  the phase is stopped, a phase is open, so a confirm dialog names what stops
  and notes that commits stay on the branch (#28). Underneath it is the engine's
  retrigger path with reuse instead of tombstoning (#30), so the pane shows the
  re-opened interview row with its `Start` action and a prompt enumerating the
  uncovered subtasks (§8.1). The rewind is logged as a rejection (#29), which
  is why the run's `Round N` chip advances.
- **Start a sub-interview** — the existing `request_sub_task_interview` path:
  a `coding-sub-interview` run rooted at this subtask, launched by the user.
- **Write the spec** — a manual spec editor for this subtask, mirroring the
  root's `Write spec manually` box, storing into the subtask's `tasks.spec`.
  This is the no-agent path and the "small detail, I'll just say what it is"
  path.
- **Give it its own run** — starts a `coding-task` run rooted at the subtask
  (nested runs are exempt from the one-run-per-project guard). With no spec on
  it, the new run starts at `interview`; with one, the user can still launch the
  interview or move straight on. The nested run's branch and PR belong to the
  subtask's **own** issue (which keeps syncing), the parent counts the subtask as
  covered, and the parent's merge refuses while the nested run is open
  (waivable) — decision #32.
- The row itself (in `Subtasks (N)`) is **unchanged**: no badge, no chip
  (decision #20). The only in-list hint that something is off is the run meta
  count.

### 7.3 The merge step with open subtasks

The merge/PR action refuses while any subtask of the run is open, listing them
on the row's secondary line (path-style, like the dirty-worktree refusal), with
**Waive the open subtasks** to complete the run anyway. Unspecced subtasks are
*not* part of this refusal (decision #12 vs #19): they are named in the same
line as a warning but only the open ones block. A subtask promoted to its own
run is open for this purpose while that nested run is unfinished, and it is the
one refusal that the user will hit in normal use — the waiver is the escape
hatch (decision #32).

### 7.5 Stopping a phase, and rewinding the interview

```
Workflow
  ☑ interview & spec                                               done
  ☑ approve the spec                                              approved · round 1
  ☐ implement the feature              ◐ running       [ Stop ]
  ☐ review & annotate
  ☐ merge the branch
```

- **Stop** appears on the row whose phase is running and delegates to the agent
  pane's stop-turn for the run's checkout; the step stays open and the branch
  keeps what the turn wrote. The row then reads as an ordinary pending step
  again (decision #27).
- The pane needs exactly one new input: whether the current phase's turn is
  running. The Layout (`main.rs`) already owns both the agent pane and the
  details pane, so it can tell the pane on refresh (the way the checkout already
  follows the run), or the details pane can read the agent pane entity's session
  state.
- Rewinding is then the subtask's **Reopen the interview** action, which is the
  same rewind the user reaches from a subtask's details (decisions #26–#29).

### 7.4 Vocabulary and microcopy

| Where | String |
| --- | --- |
| Run section heading | `Workflow` |
| Tree heading | `Subtasks (N)` (unchanged) |
| Run meta | `spec covers 2/3 subtasks` (omitted when there are no subtasks) |
| Subtask state | `No spec yet` / `Specced · 11 lines` / `Covered by the feature spec` / `Covered by its own run` |
| Fix actions | `Reopen the interview` · `Start a sub-interview` · `Write the spec` |
| Promotion | `Give it its own run` |
| Merge refusal | `3 open subtasks — close them or waive` · `Waive the open subtasks` |
| Phase stop | `Stop` (tooltip: `Stop this phase's agent turn; the branch keeps what it wrote`) |
| Rewind disabled | `Stop the implement phase first` |
| Rewind confirm | `Reopen the interview?` · `This stops the implement phase and starts a new round. Commits stay on the branch.` → `Reopen the interview` |

## 8. Phase prompts

### 8.1 Interview (root) — enumerate and state the contract

Appended to `INTERVIEW_BASE_PROMPT` + the target, before the agent starts:

```
## Subtasks that must be covered

This feature has N open subtasks. For each one, either save its own spec
(`save_spec` with `subtasks: [{ task_id, spec }]`) or mark it as covered by the
feature spec (`covered: [task_id]`). Do not leave one undecided.

- #41 Add token refresh
- #42 Wire the callback URL
```

Subtasks already done are omitted (decision #11). With no open subtasks the
section is omitted entirely, so a plain feature's prompt is unchanged from
today.

### 8.2 Implement — umbrella plus every open subtask spec

`phase_prompt("implement")` inlines the umbrella (as today) and then, per open
subtask with a spec:

```
### Add token refresh (subtask #41)
<spec>
```

Subtasks marked covered contribute a one-line bullet with their title only
(their detail is in the umbrella). The closing instruction keeps today's "work
on branch `<branch>`; do not merge".

### 8.3 Review — read the coverage

The review prompt gains one line naming the subtasks and their state, so the
reviewer's summary can say whether the branch actually did what the specs asked.
No mechanical checking in this spec (no diff parsing).

## 9. Failure and edge-case matrix

| Situation | Behaviour |
| --- | --- |
| Subtask added after the interview, run still before the spec gate | Flagged; the spec gate still approves. The fix actions are on the subtask. |
| Subtask added after implement started | Same: flagged, never blocking; `save_spec` on a later round picks it up if the user re-interviews. |
| Subtask edited (title/description) | Nothing changes; the spec stays (decision #13). |
| Subtask completed | Leaves the count, never flagged (decision #11). |
| Subtask deleted | The spec and mark go with the row; the count shrinks. No tombstone ceremony. |
| Agent never calls `save_spec` (or no agent configured) | The manual paths cover it: `Write the spec` per subtask, `Write spec manually` for the umbrella, and the spec gate advances by hand. Coverage accepts hand-written specs (they are just `tasks.spec`). |
| Agent returns ids that are not subtasks of the run | Tool error, nothing written (§6.2). |
| Agent marks a subtask covered and later specs it | Own spec wins in `subtask_coverage`; the mark stays as history. |
| Re-interview (new round) after the spec gate | A rewind: the interview row re-opens, a fresh spec gate row spawns for the round, pending steps are re-used, and the previous cycle's rows stay as history (decisions #26–#30). |
| Rewind while the phase's agent turn runs | Disabled, with the tooltip pointing at `Stop` on the active row. Stopping ends the turn (commits kept); the rewind then confirms (decisions #27/#28). |
| Rewind mid-implement | Confirm, then the implement step stops and returns to open; the interview re-opens; when the new spec is approved the *same* implement row picks up again in the same worktree with the new spec appended to its prompt. |
| Rewind logged | Appends `kind: "reject"`, so `Round N` and the log agree (decision #29). |
| Spec written on a step row | Rejected: the pane has no spec editor for a step, and `save_spec` refuses a step id (decision #31). |
| Promoted subtask and GitHub | It keeps syncing its own issue; its nested run's branch name and PR `Closes #n` name its issue, not the parent's (decision #32). |
| Promoted subtask and the parent's counts | Counted in `spec covers N/M` as covered; its row shows the nested run's state and links into it; the parent's merge refuses while the nested run is open, waivable (decisions #19/#32). |
| Run cancelled with uncovered subtasks | Nothing special; the flags live with the tasks, not the run. |
| Promoted subtask (nested run) | Its own run has the same model, recursively (decision #17); while it is active it is an open subtask of the parent, so the parent's merge refuses until it finishes or is waived. |
| Run started before this change (v1 recipe, steps are subtasks) | Steps keep `role = NULL` and stay visible as subtasks in the task list for that run. Acceptable: the recipe is immutable per install and only new runs get steps. Alternative (not chosen): backfill `role='step'` for any `node_id` row whose run's recipe has phases. |
| Step row in the task list, deep-linked from somewhere | Not possible in the new model; a step is only reachable from the run's Workflow section. |
| Todoist/GitHub sync | Steps were already excluded via `workflow_run_id`; the new filter is equivalent for them and now explicit. |

## 10. Engine/code change list (by file)

**`libs/storage/`**

1. `toasty/migrations/0025_coding_step_role.sql`: `tasks.role`,
   `tasks.spec_covered_at`, index, backfill.
2. `task.rs`: model fields, `TaskCreate` builders, the three positional record
   parsers.
3. `workflow.rs`: `RecipeNode.role` + `parse_recipe` validation; `coding-task`
   v2; `ensure_coding_recipes` version upsert; `spawn_node_task` writes
   `role = 'step'`; `run_subtasks`; `subtask_coverage` / `coverage_summary`;
   `save_subtask_specs` (+ `save_task_spec` reuse) and `cover_subtasks`;
   `subtasks_map` / `list_subtasks` exclude steps; `spawn_node_task`'s
   re-use-before-spawn; `retry_task` stops tombstoning pending children;
   `stop_coding_phase`.
4. `github.rs`: make the step exclusion explicit and narrow it (`role = 'step'`
   on the task or an ancestor) in the sub-issue push/pull paths, so a promoted
   subtask keeps syncing.

**`apps/todo-2/`**

5. `coding_mcp.rs`: `get_coding_context.sub_tasks[]` gains `spec` (+
   `description`); `save_spec` gains `subtasks` / `covered` with the guard;
   `create_sub_task` parents to the run root in a coding run and echoes the
   parent used.
6. `store.rs`: wrappers `reopen_coding_interview(task_id)`,
   `write_subtask_spec(task_id, spec)`, `promote_subtask(sub_task_id)`,
   `stop_coding_phase(step_id)`, and `run_subtasks(task_id)`; `save_coding_spec`
   grows the subtask payload.
7. `ui_parts/task_details.rs`: `Workflow` heading; coverage clause in the run
   meta; nested subtask-spec rows in `coding_spec_artifact`; the subtask `Spec`
   block with the four actions; merge refusal + waiver; `Stop` on the running
   phase row and the rewind's disabled/confirm states; the interview prompt's
   subtask section; the implement prompt's per-subtask specs.
8. `ui_parts/task_list.rs`: steps filtered out of `subtasks_map`.
9. `main.rs`: wiring for the new events/actions (re-interview, promote) since
   both need the run to refresh and the pane to follow, plus the phase-busy
   signal the disabled rewind reads.

## 11. Testing plan

**Storage (`cargo test -p storage`)**

- `coding-task` v2 validates; `role` + `subtask` together is rejected; a bad
  `role` value is rejected; `role: "step"` without `phase` is rejected.
- `ensure_coding_recipes` twice: v1 present → v2 appended once, idempotent, and
  `recipe_id_by_slug` returns v2; a run created against v1 keeps its steps as
  subtasks.
- Migration backfill: a pre-existing step row (node_id + run) ends up
  `role = 'step'`; a user subtask does not.
- `run_subtasks` / `subtasks_map` / `list_subtasks` never return step rows.
- Coverage: `subtask_coverage` table test (own / covered / none / done), and
  `coverage_summary` excluding done.
- `save_subtask_specs`: writes the umbrella + subtask specs + marks in one
  commit; rejects a foreign id, a step id, and a grandchild id; completes the
  interview step; a re-opened interview keeps earlier specs and marks.
- Promotion starts a nested run and is exempt from the one-run-per-project
  guard; a promoted subtask still syncs its own issue while a step (or a step's
  descendant) does not; a promoted subtask counts as covered in the parent's
  summary.
- Re-use before spawn: re-opening the interview does not duplicate the pending
  implement row; a completed spec gate yields a **new** spec row for the next
  round; `retry_task` leaves pending children in place instead of tombstoning.
- `stop_coding_phase` leaves the step open with `done = false`.

**todo-2 (`cargo test -p todo-2`)**

- `coding_mcp`: `get_coding_context` reports per-subtask state;
  `save_spec` with `subtasks`/`covered` writes and advances; a bad id errors
  without partial writes; `create_sub_task` with a step id as parent lands on
  the run root.
- Prompt builders (pure functions, already unit-tested): the interview prompt
  enumerates open subtasks and omits the section when there are none; the
  implement prompt inlines the umbrella and each open subtask spec and lists
  covered ones by title.
- Merge gate: refuses with open subtasks, proceeds after waiver.
- Task list: a step row never appears in `compute_row_specs`.
- `save_spec` with a step id in `subtasks` or `covered` errors and writes
  nothing.

**UI (GPUI, `TestAppContext`)**

- The subtask `Spec` block renders the three states and the four actions; the
  coverage clause appears only when the run has subtasks.
- The spec artifact expands to nested subtask-spec rows.
- Reopen the interview: disabled with its tooltip while the phase is running,
  enabled once stopped, and the confirm dialog appears only when a phase is
  open.

## 12. Out of scope

- Reading the code to check that a subtask spec was *implemented* (review stays
  transcript + notes; no diff surface — unchanged from the runs spec).
- Blocking the spec gate on uncovered subtasks (explicitly rejected: decision
  #12).
- Auto-promoting a subtask to its own run, or a size heuristic that decides
  "big vs small" without the model.
- Per-subtask branches/worktrees/PRs without a nested run.
- Editing the recipe from the UI (recipes stay JSON in the DB).
- Changing Todoist sync behaviour or the generic `RecipeNode.subtask` lazy spawn
  for non-coding recipes.
- A diff of specs between rounds (no spec versioning beyond "the current one").
- Rewinding the **branch**: a rewind never resets the worktree or the commits a
  stopped phase already wrote (decision #27); it only turns the run back to the
  interview.

## 13. Open questions

The six carried over from the first two rounds are all resolved (decisions
#26–#33). Mapping, for the record:

| Was open | Resolved as |
| --- | --- |
| 1. Does "Reopen the interview" rewind an advanced run? | Yes — a real rewind, any phase (#26), gated on a running turn (#27), confirmed when a phase is open (#28), logged as a rejection (#29), with rows re-used rather than tombstoned (#30). |
| 2. GitHub sync of a promoted subtask | It keeps syncing its own issue (#32). |
| 3. Coverage wording | `spec covers 2/3 subtasks` (#11). |
| 4. Round recorded with the mark | No — `spec_covered_at` stays a timestamp (#33). |
| 5. Promotion and the parent's coverage | Counted as covered in the parent's count, shown as a nested run on the row (#32). |
| 6. Manual spec for a step | Never; the pane has no such editor and the tools refuse a step id (#31). |

Two copy-level things are deliberately left open, both cheap to settle during
implementation:

- The exact confirm-dialog wording beyond the sketch in §7.4.
- Whether a stop that is followed by a rewind gets its own round-log line or is
  only implied by the `reject` entry.

## 14. Implementation order

1. **Storage foundation**: migration 0025, model/parsers, `RecipeNode.role` +
   validation, `coding-task` v2 + `ensure_coding_recipes` version upsert,
   `spawn_node_task` writes the role, the four filters, `run_subtasks`,
   coverage helpers, `save_subtask_specs`. Tests from §11.
2. **MCP surface**: `get_coding_context` state, `save_spec` payload + guard,
   `create_sub_task` parenting rule. Tests.
3. **Prompts**: interview enumeration + contract, implement per-subtask specs,
   review coverage line. Prompt unit tests.
4. **Details pane**: `Workflow` heading, coverage clause, nested spec rows, the
   subtask `Spec` block with its four actions, the manual subtask spec editor.
5. **Gates and rewinds**: merge refusal + waiver; promotion wiring (`main.rs`
   events); the phase **Stop** control and the rewind's disabled/confirm
   states plus the phase-busy signal; `stop_coding_phase`; `retry_task`'s reuse
   (no tombstoning) and the re-use-before-spawn rule.
6. **Task list**: step filter; sweep for any other surface that should not see
   steps.
7. **Polish**: empty states (a feature with no subtasks reads exactly as
   today), tooltips, and a run started before the change.

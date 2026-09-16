# todo2: Unified Managed Tags / Sections / Tasks Spec

Status: Draft v2 — all open questions from v1 resolved by interview; no code
changes made yet.

Purpose: Replace the single-purpose "managed tag" mechanism (a workflow
recipe owning an entire tag) with one unified model in which an **app** (a
workflow recipe, an integration such as Todoist, or a built-in) can manage a
tag, a section, or individual tasks — including *part* of an existing
user-owned tag — while the user keeps adding their own tasks alongside.

---

## 1. Problem statement

Today there are three unrelated mechanisms for "something other than the user
creates content":

1. `tags.managed_by_recipe_id` — an automation owns an **entire tag** (only
   `managed:packing-list` today, via `trip.rs`). Enabling creates the tag,
   disabling deletes the tag and all of its content.
2. `external_task_links` / `external_tag_links` — the Todoist sync maps remote
   projects/sections/labels onto local tags/tasks, with no notion of who may
   edit what.
3. `Task.workflow_run_id` — workflow steps are grouped by their run but are not
   otherwise marked as "not the user's".

This is too coarse for the desired use cases:

- An app should manage **only a section** inside a tag that otherwise belongs
  to the user.
- An app should manage **a handful of tasks** in a normal tag, with no section
  of its own.
- An app should be able to **take partial ownership of an existing tag** the
  user already has. Canonical use case: *"I already have a Travel checklist in
  Todoist. I want the travel mini-app to add templated tasks to it when I
  create a new trip."*
- The app decides whether the content it manages is **editable**.
- The user can still add **unmanaged tasks** to the same tag/section when the
  app's settings allow modification.

---

## 2. Design decisions

### 2.1 Core shape (interview round 1)

| # | Question | Decision |
|---|----------|----------|
| 1 | Owner identity | **Unify recipes + integrations as "apps"**, stored in a new `apps` table. |
| 2 | Tracking model | **Nullable owner columns on each table** (`tags`, `tag_sections`, `tasks`). |
| 3 | Co-ownership | **Multiple apps may manage one tag** (disjoint sections/tasks). |
| 4 | Section scope | App owns the **section shell + generated tasks**; the user may add unmanaged tasks into it. |
| 5 | Edit policy | **Per-item flag set by the app only.** |
| 6 | Allowed ops on read-only items | **Complete / reopen only.** Subtasks, scheduling/priority edits, extra tags, delete, and reorder are blocked. |
| 7 | User edits a managed field | **Hard block** — disabled in the UI and rejected at the store layer. |
| 8 | Disable/remove app | **Remove app-owned rows, keep the user's.** |
| 9 | Attach to existing tag | **User picks the tag in the app's settings panel.** |
| 10 | Visual marking | **Lock icon on read-only fields only.** Minimal treatment; no badges on sections/tags, no group tint. |
| 11 | Regeneration strategy | **Replace app-owned items in the scope, keep user items.** |
| 12 | User tasks in managed sections | **Fully normal and unmarked**; the app never touches them. |
| 13 | Grouping for regeneration | **Owner column is enough** — no batch/instance id required. |
| 14 | Sync interplay | **Managed tasks sync like any other** (the travel checklist appears in Todoist). |
| 15 | Project/dir tags | **Off-limits.** `project:/path` tags are user-owned; apps cannot attach to them. |
| 16 | Existing `managed:packing-list` | **Migrate to partial ownership** — the tag becomes an ordinary user tag that survives disabling the app. |
| 17 | Escape hatch | **None** locally. A user can complete items, or a remote edit unlocks an item (see #27), or they disable the app. |
| 18 | Ownership controls live in | App settings panel, tag settings, section context menu, task **detail pane** (not the task context menu, no navbar marker). |
| 19 | Managed tasks in global list | **Yes** — listed in All tasks/priority, usable as blockers/follow-ups. |
| 20 | Multi-item generation failure | **All-or-nothing** transaction. |
| 21 | App deletion flow | UI asks whether to delete managed items; then **one DB transaction: tombstone app-owned incomplete items → disable app**. |
| 22 | Editable flag semantics | **Dynamic, app-controlled** — the app can flip editable ↔ read-only at any time. |
| 23 | Orphans | Handled by the deletion flow (which only ever follows a confirmed UI action); no unconditional startup GC. |
| 24 | Capture | **Apps decide whether they "capture" new tasks added to a tag/section they manage.** Todoist's app captures new tasks added to a linked tag so they are also added to Todoist. |

### 2.2 Resolutions of v1 open questions (interview rounds 4–6)

| # | Question | Decision |
|---|----------|----------|
| 25 | Capture vs managed storage | **One `managed_by` column plus a `managed_mode` enum** (`'managed'` \| `'captured'`). No separate column. |
| 26 | Rights of a captured task | **Fully normal in every way** — rename, delete, re-tag, move. Capture only governs propagation; moving out of the captured scope simply stops syncing it. |
| 27 | Remote edit to an owned item | **A remote edit unlocks the whole item**: set `managed_editable = true` and `user_modified = true`; the app stops regenerating it. |
| 28 | Two apps claiming one item | **First owner wins.** A section/task already owned by another app cannot be claimed; the second app's attach or create fails with a clear error. |
| 29 | Regeneration vs user edits | **Spare user-modified items.** An item-level `user_modified` boolean; such items are never rewritten or deleted by regeneration. |
| 30 | Does completion count as a modification? | **No.** Completing/reopening never sets `user_modified`. Only content edits (title, description, tags, scheduling) do. |
| 31 | Completed app-owned items | **Kept on regeneration and kept on removal** — they become unowned user history. |
| 32 | Lock in list rows | **Hover-only lock glyph** next to the title; double-click rename silently does nothing on a managed row. |
| 33 | Seed/demo data | **Becomes a `kind='builtin'` app ("Demo data"); `is_seed` is retired.** |
| 34 | Post-migration tag name | **Rename to a plain user tag** (e.g. "Travel"); the app label lives only in tag settings. Bindings are by tag id, so later renames are safe. |
| 35 | `managed_tag` manifest field | **Replaced by binding config.** An app may manage **multiple tags, fully or partially**; `managed_tag` is dropped. |
| 36 | Transactions | **Raw `BEGIN`/`COMMIT`/`ROLLBACK` helper** (`TodoStore::with_transaction`) via `toasty::sql::statement`. |
| 37 | Sections after removal | **Any app-owned section still containing items (user's or completed) survives and is downgraded to a normal unmanaged section.** Empty sections are removed. |
| 38 | `full_tag` vs `partial` | **A tag with a `full_tag` binding is closed to partial attachment** until that binding is released or converted. |
| 39 | Removal UI scope | **One app-wide dialog** listing every binding and what will go; one confirm runs the whole removal in one transaction. |

---

## 3. Model changes

### 3.1 New `apps` table

The single registry of things that can own content.

```
apps
  id            INTEGER PRIMARY KEY AUTOINCREMENT
  kind          TEXT NOT NULL            -- 'recipe' | 'integration' | 'builtin'
  slug          TEXT NOT NULL UNIQUE     -- stable identifier, e.g. 'travel', 'todoist', 'demo'
  label         TEXT NOT NULL            -- human name, e.g. 'Travel checklists'
  description   TEXT
  enabled       INTEGER NOT NULL DEFAULT 0
  created_at    TEXT NOT NULL
```

Back-references so an app can be resolved from its source object:

```
recipes.app_id        INTEGER NULL   -- set for kind='recipe'
integrations.app_id   INTEGER NULL   -- set for kind='integration'
```

An app's **manifest** gains management declarations. There is no longer a
single `managed_tag` field: an app may manage **many tags**, so the manifest
describes *preferences*, and each concrete relationship is an
`app_tag_bindings` row.

```
manifest (recipe JSON / integration config)
  slug: Option<String>            -- stable app identity, e.g. 'travel'
  label: Option<String>           -- display name; falls back to recipe name
  description: Option<String>
  default_editable: bool          -- default editability for newly created items
  default_capture: bool           -- default capture policy for new bindings
  suggested_tags: Vec<String>     -- optional hints for the attach picker
  new_tag_name: Option<String>    -- label to use if the user chooses "create a new tag"
  co_ownable: bool                -- may other apps hold partial bindings on a tag
                                  -- this app fully owns? (default false; see §3.2)
```

The demo/builtin app (`kind='builtin'`, `slug='demo'`) owns all seed content.

`managed_by_recipe_id` on `tags` is **deprecated** and replaced by
`managed_by`; the column can be dropped once the migration is done.

### 3.2 Bindings: which app is attached to which tag

Partial ownership needs a binding row, because co-ownership is allowed and the
attach/capture policy is per (app, tag). An app may hold **any number of
bindings**, across different tags, in either role.

```
app_tag_bindings
  id                 INTEGER PRIMARY KEY AUTOINCREMENT
  app_id             INTEGER NOT NULL     -- -> apps.id
  tag_id             INTEGER NOT NULL     -- -> tags.id
  role               TEXT NOT NULL        -- 'full_tag' | 'partial'
  capture_new_tasks  INTEGER NOT NULL DEFAULT 0
  created_at         TEXT NOT NULL
  UNIQUE (app_id, tag_id)
```

Rules:

- `role = 'full_tag'` preserves today's managed-tag behavior for apps that
  still create and own a whole tag.
- `role = 'partial'` is the attach-to-existing-tag case. The tag itself stays
  `managed_by = NULL` and survives the app being disabled.
- A tag holding a `full_tag` binding is **closed to partial attachment**
  (decision #38); the user must release or convert that binding first.
- Within a tag, ownership of a given section or task is **first-come**
  (decision #28): an item whose `managed_by` is already set cannot be claimed
  by another app, and the attempt errors rather than silently stealing it.
- Directory-backed `project:/path` tags are **not eligible** for bindings.

### 3.3 Owner columns

Nullable owner columns on the three content tables:

```
tags
  managed_by        INTEGER NULL   -- -> apps.id; non-NULL only for full-tag ownership
  -- (managed_by_recipe_id is deprecated)

tag_sections
  managed_by        INTEGER NULL   -- -> apps.id; NULL = user-created or downgraded section
  managed_capture   INTEGER NULL   -- per-section override of the binding's capture policy

tasks
  managed_by        INTEGER NULL   -- -> apps.id; NULL = plain user task
  managed_mode      TEXT NULL      -- 'managed' | 'captured' (NULL when unowned)
  managed_editable  INTEGER NULL   -- dynamic, app-controlled per-item editability
  user_modified     INTEGER NOT NULL DEFAULT 0
```

Semantics:

- **`managed_mode = 'managed'`** — the app owns the task: hard-blocked edits,
  completion still allowed, regeneration may replace it (unless
  `user_modified`).
- **`managed_mode = 'captured'`** — the task only propagates (syncs) through
  the app. It is **fully normal** to the user: rename, delete, re-tag, move.
  Moving it out of the captured scope simply ends the capture; the app never
  regenerates or removes it. Todoist tasks created from a captured new task
  therefore never become locked.
- `managed_editable` is the **dynamic** flag the owning app flips, and also
  the flag a remote edit sets. It only governs local editing; the app always
  writes through its own privileged store path.
- `user_modified` is set by any user content edit to an owned item (never by
  completing/reopening). Regeneration and removal **skip** it; the item is
  effectively the user's from then on, while `managed_by` remains as
  provenance.
- A remote edit sets `managed_editable = true` **and** `user_modified = true`
  on the whole item, so the app neither overwrites nor regenerates it
  (decision #27). This makes a remote edit the one real escape hatch.
- Ownership is exclusive per item (decision #28), so `managed_by` never needs
  to be a list.

### 3.4 Capture

When a task is created (or moved/assigned) into a scope where an app's capture
policy is on:

1. Resolve the applicable bindings for the destination tag and, if the task
   lands in a section, for that section.
2. For each app with `capture_new_tasks` (binding) or `managed_capture`
   (section) set, run the app's capture hook.
3. Capture writes `managed_by = app_id` and `managed_mode = 'captured'` on the
   task, and the app records its own identity link (for Todoist, an
   `external_task_links` row).
4. Capture is non-destructive: a failing hook logs and leaves the local task
   untouched (the existing sync push path already behaves this way).
5. If the task later leaves the captured scope, `managed_by`/`managed_mode`
   are cleared and the app's link is dropped (Todoist: unlink only — the
   remote task is not deleted).

### 3.5 Sections are represented twice — keep both in sync

Sections currently exist as a `tag_sections` row **and** as a child tag via
`tag_implications` (tasks attach to the child tag; `section_groups_for_tasks`
uses the child tags; `tag_sections` supplies names and order). Any ownership
change must therefore be written to **both** representations, or the model
must pick a single source of truth. This spec keeps both but requires that:

- creating an app-owned section writes the `tag_sections` row and the child tag
  with the same `managed_by`;
- removing a section removes both;
- **downgrading** a section (decision #37) clears `managed_by` in both places
  and leaves the section as an ordinary user section;
- a consistency check (test + optional repair on load) asserts that for every
  app-owned `tag_sections` row there is a child tag with the same owner.

### 3.6 Migrations (proposed numbering: 0018+)

1. `0018_apps.sql` — create `apps`; add `recipes.app_id` and
   `integrations.app_id`.
2. `0019_managed_scope.sql` — add `tags.managed_by`,
   `tag_sections.managed_by`, `tag_sections.managed_capture`,
   `tasks.managed_by`, `tasks.managed_mode`, `tasks.managed_editable`,
   `tasks.user_modified`; create `app_tag_bindings`.
3. `0020_backfill_managed.sql` — data migration:
   - create an `apps` row for the travel recipe (`kind='recipe'`,
     `slug='travel'`) and one per integration (`kind='integration'`);
   - create the built-in demo app and re-mark all `is_seed` tags/tasks as owned
     by it (`managed_mode='managed'` for tags: `tags.managed_by`, and for
     tasks: `managed_by` + `managed_mode='captured'` so they keep full edit
     rights while never syncing);
   - for `managed:packing-list`: create a `partial` binding, set `managed_by`
     on its section child tags / `tag_sections` rows and on the tasks carrying
     those sections, clear `tags.managed_by_recipe_id`, leave `tags.managed_by`
     NULL, and **rename the tag to a plain name ("Travel")** (decision #34);
   - derive bindings from `external_tag_links` for integrations, with
     `capture_new_tasks = 1`, reproducing today's "assign to tag then push"
     behavior.
4. `0021_drop_is_seed.sql` — drop `tasks.is_seed` / `tags.is_seed` once the
   built-in app owns the behavior (decision #33).
5. `0022_drop_managed_by_recipe_id.sql` — drop `tags.managed_by_recipe_id`
   once the Rust backfill has been retired (the column is only historical
   now; see §10).

Note: the migration runner splits statements and has no transaction support
today; the backfill must be written as plain `UPDATE ... WHERE` statements and
wrapped by the new helper once it exists (§4).

---

## 4. Store API changes (libs/storage)

New/renamed functions, roughly:

```
transactions
  with_transaction<T>(f: impl FnOnce(&mut TodoStore) -> Future<Output = QueryResult<T>>) -> QueryResult<T>
    // BEGIN; run f; COMMIT, or ROLLBACK on Err (decision #36)

apps
  create_app(kind, slug, label, description) -> App
  app_by_slug(slug) -> Option<App>
  app_for_recipe(recipe_id) -> Option<App>
  app_for_integration(integration_id) -> Option<App>
  list_apps() -> Vec<App>
  set_app_enabled(app_id, enabled)

bindings
  attach_app_to_tag(app_id, tag_id, role, capture_new_tasks) -> AppTagBinding
  detach_app_from_tag(app_id, tag_id)
  bindings_for_tag(tag_id) -> Vec<AppTagBinding>
  bindings_for_app(app_id) -> Vec<AppTagBinding>

ownership
  tag_owner(tag_id) -> Option<u64>                  // tags.managed_by
  section_owner(section_id) -> Option<u64>
  task_owner(task_id) -> Option<u64>
  set_task_managed(task_id, app_id, mode, editable)
  set_task_editable(task_id, editable)              // dynamic flag flip
  mark_task_user_modified(task_id)                  // idempotent
  set_section_managed(section_id, app_id, capture)  // writes both representations
  downgrade_section(section_id)                     // clears managed_by, keeps the section
  managed_tasks_in_tag(tag_id, app_id) -> Vec<Task> // regeneration scope
  managed_sections_in_tag(tag_id, app_id) -> Vec<TagSection>

lifecycle
  disable_app(app_id, remove_owned_items: bool)     // one transaction (decisions #21, #36)
  capture_task(task_id)                             // run capture hooks for the destination scope
  release_app_from_tag(app_id, tag_id)              // detach one binding; downgrade its sections
```

Every existing mutating task API (`update_task_title`,
`update_task_description`, `set_task_tags`, `delete_task`, …) calls a guard and
returns a typed `TaskLocked { task_id, app_id }` error when the task is
`managed_mode = 'managed'` and not `managed_editable`. `update_task_done` is
explicitly exempt. `managed_mode = 'captured'` bypasses the guard entirely.
Content edits through the guard set `user_modified = 1`.

`disable_app` runs as a single transaction:

1. for every binding, for every section it owns: if the section still contains
   any tasks (user or completed), **downgrade** it; otherwise remove it;
2. if `remove_owned_items`: tombstone app-owned tasks that are **not**
   completed and **not** `user_modified`; completed and user-modified items
   keep their rows and lose ownership (`managed_by`/`managed_mode` cleared);
3. delete the app's bindings;
4. for `full_tag` bindings only, remove the tag it created;
5. set `apps.enabled = 0`.

`release_app_from_tag` performs steps 1–3 for one binding, leaving the app
enabled for its other tags.

---

## 5. App behavior

### 5.1 Travel app (migrated)

- Becomes an app of `kind='recipe'`, `slug='travel'`.
- Its migrated tag is renamed to a plain user tag ("Travel") and gets a
  `partial` binding; the user may add further tags (fully or partially) from
  the app's settings, and the app may hold several bindings.
- "New trip" still runs the recipe and generates its checklist, but now:
  - it **replaces** the app-owned items in the sections it owns that are
    neither completed nor `user_modified`;
  - user tasks and user-modified items in the same tag/sections are untouched;
  - completed items are kept as history.
- Disabling the app removes its incomplete generated items; sections that still
  hold anything are downgraded to plain sections; the tag survives.

### 5.2 Todoist app (migrated to the unified model)

- Becomes an app of `kind='integration'`, `slug='todoist'`.
- Its project/section tag links become bindings with
  `capture_new_tasks = 1`, which reproduces today's desired behavior: adding a
  task to a Todoist-linked tag also creates it in Todoist.
- Linked tags/sections become owned by the Todoist app. Local user edits still
  work exactly as today (the sync path is a privileged writer, not subject to
  the guard).
- An edit made *in Todoist* to an app-owned item unlocks that item locally
  (`managed_editable = true`, `user_modified = true`) so the app stops
  regenerating it, and the remote value wins (decision #27).
- Content the Todoist app itself created for a user task (capture) uses
  `managed_mode='captured'`, so capturing never locks a user's task.

### 5.3 Any app

- Apps declare preferences in their manifest (`default_editable`,
  `default_capture`, `suggested_tags`, `new_tag_name`, `co_ownable`) and own
  items by setting `managed_by`/`managed_mode`/`managed_editable` when creating
  them.
- Apps may flip `managed_editable` at any time (dynamic, decision #22).
- Apps are responsible for their own regeneration strategy through the shared
  helpers; the model only provides the scope query and the sparing rules.

---

## 6. UX

### 6.1 Attaching to an existing tag

1. The user opens the app's settings panel (installed apps list).
2. They choose **"Manage an existing tag"**, which lists eligible ordinary tags
   (directory-backed `project:` tags are excluded and grayed out with a reason;
   tags held by a `full_tag` binding are excluded with "release first").
3. Selecting a tag creates a `partial` binding; the app's panel then appears
   when that tag is open.
4. Tag settings shows the binding with a **Detach** action; the app's panel
   shows all its bindings.
5. Attaching a second app to a tag where the first owns sections works; any
   attempt to claim an already-owned section or task fails with an explicit
   "already managed by X" error.

### 6.2 Locked fields

- Read-only managed tasks render with a small **lock glyph beside the title on
  row hover** (decision #32), and the editable fields in the task detail pane
  (title, description, tags, deadline/priority, delete) are disabled with a
  tooltip naming the owner ("Managed by Travel checklists").
- Double-clicking a managed row title does nothing (no edit mode, no error).
- Completion (checkbox, and the done toggle in details) stays enabled.
- Section rows show their owner only in the **section context menu**; there is
  no section badge or header treatment (decision #10).

### 6.3 Regeneration

- "Create new trip" / "refresh" computes the app's desired items, then in one
  transaction:
  - removes the app's own items in the owned scope that are incomplete and not
    `user_modified`;
  - recreates them from the template.
- Completed items are kept (history); `user_modified` items are kept and are no
  longer regenerated.
- User tasks are never moved, renamed, or removed; because app items are
  recreated, user tasks may visually shift relative to them.

### 6.4 Disabling / removing an app

- One app-wide dialog lists every binding and what will go, and asks whether to
  delete the app's items (decision #39).
- Confirm runs `disable_app` in a single transaction (§4).
- Completed and user-modified items always survive and become ordinary tasks.

### 6.5 Global list

- Managed tasks appear in the All-tasks/priority list like any other task, can
  be blocked-by/blockers, and can be completed from there.

---

## 7. Problems this design must confront

### 7.1 Data-model problems

1. **Double representation of sections.** As in §3.5, ownership must be written
   to both the `tag_sections` row and the child tag; divergence is a real
   corruption risk (an orphan section row, or a child tag pointing at a section
   that no longer exists). Needs an invariant + test, and ideally a repair path.
   Downgrade-on-removal (#37) adds a third write path that must do this.
2. **Capture vs. managed is now a mode on one column.** Resolved in principle
   (#25/#26), but the mode must be respected by *every* read path: the guard,
   regeneration scope, removal, the lock UI, and the global list. Any path that
   treats "has `managed_by`" as "is locked" will silently lock captured user
   tasks — the exact failure the mode exists to prevent.
3. **Hard block vs. sync.** Resolved as "remote edit unlocks the item"
   (#27). Remaining subtlety: the unlock is written by the sync engine, so two
   remote renames (or a rename plus a later template change) must be
   idempotent, and the local row should not flip editable/`user_modified` for
   remote changes the merge would have discarded anyway.
4. **`managed_by` vs `workflow_run_id`.** Workflow steps already belong to a
   run; a travel recipe step is also an app-owned task. Proposal: the recipe's
   app is the owner and `workflow_run_id` stays engine bookkeeping — but this
   must be stated explicitly wherever run steps are queried.
5. **No transaction support in the storage layer.** Resolved by adding the
   `with_transaction` helper (#36), which is raw `BEGIN`/`COMMIT`/`ROLLBACK`.
   The helper must be re-entrant-safe (nested calls should join or error, not
   issue a second `BEGIN`) and must roll back on panic.
6. **No FK enforcement / orphans.** SQLite does not enforce FKs here, so a
   `managed_by` can dangle. The deletion flow is now the only deliberate path
   (#23), and it is transactional, which closes the main hole; a crash or a
   direct DB edit can still orphan rows and no startup GC exists.
7. **`is_seed`, `workflow_run_id`, and `managed_by` were three "not a normal
   user task" markers.** Resolved for seed data (#33): it becomes the built-in
   demo app. `workflow_run_id` remains a separate engine concept by design.
8. **Binding uniqueness vs. co-ownership.** `UNIQUE(app_id, tag_id)` prevents
   one app attaching twice to a tag, and first-come ownership (#28) stops two
   apps claiming the same item. What is *not* covered: two apps both attaching
   and both wanting to create a section with the same name. With #28 this
   becomes an error; the UX must make that error legible and suggest a
   different section name.
9. **Dynamic editability vs. replace-on-regenerate.** Resolved by
   `user_modified` (#29/#30): the moment the user edits an item it is spared,
   so a deliberate edit is never silently discarded. Completed items are spared
   independently (#31).
10. **`user_modified` is item-level, not field-level.** An edit to a
    description therefore freezes the whole item, including fields the app
    might still want to refresh. Accepted for simplicity; worth revisiting if
    apps start managing multi-field items.

### 7.2 UX problems

1. **Low-visibility ownership.** With a hover-only lock glyph and no
   section/tag badges, a user can see tasks that cannot be renamed and never
   learn why, unless they hover the title or open the details pane.
2. **No local escape hatch.** Decision #17 stands: locally, an unwanted
   generated item can only be completed or escaped by disabling the app
   (removing its other content too). A remote edit or the details pane's
   explanation is the only other route. This remains the most likely
   complaint.
3. **Mixed editability in one list.** A user's own tasks in an app-managed
   section are fully normal, while adjacent app items are locked; the only
   visual cue is the hover glyph, so the two regimes are easy to confuse.
4. **Two settings entry points.** Attaching lives in the app's settings panel,
   detaching in tag settings and the section menu. Users will look in the wrong
   place, especially now that one app can own several tags.
5. **Removal is app-wide.** With #39 there is no "just remove it from this one
   tag" action in the app dialog; the per-tag detach lives in tag settings, so
   the two flows must be cross-linked or users will over-delete.
6. **Completed items outlive the app.** #31 means a removed app leaves finished
   checklists behind in downgraded sections; users who expected a clean removal
   may be surprised by the leftovers.
7. **Capture surprise.** A user adds a task to a synced tag and it silently
   appears in Todoist. Capture is now an explicit per-binding/per-section
   policy, so it should be visible in tag settings or it will feel like magic.
8. **No sidebar marker.** #18 was kept: a user cannot tell which tags have an
   app attached, which matters more now that one app can attach to many tags.
9. **`project:` tags being off-limits is surprising.** A user may reasonably
   expect a travel-checklist app to work on a project tag; the picker must
   explain the exclusion rather than silently omit those tags.
10. **Section ordering with mixed ownership.** App sections and user sections
    share one `position` sequence; the app reorders its own on regeneration,
    which can move user sections around.
11. **"Managed by X" errors on attach.** First-come ownership is correct but
    produces dead ends ("that section name is taken by Travel checklists") that
    must be explained rather than shown as a raw error.

### 7.3 Things most likely to confuse

- Why a task I can check off cannot be renamed.
- Which "Travel" thing is mine: the tag, or the app's binding to it.
- Why disabling the app deleted content yesterday but now leaves sections and
  completed items behind (the consequence changed during migration).
- Why a completed item I can see outlived the app that made it.
- Why adding a task to a Todoist-tagged list also created it in Todoist.
- Why a user task captured by Todoist is fully editable while an app-generated
  task beside it is locked.
- Why the app's panel appears inside a tag that is otherwise mine.
- Why `project:` tags can't be attached.
- Why a section shows a lock in its menu but no visible badge in the list.

---

## 8. Non-goals

- No change to the recurrence engine, workflow node semantics, or the coding
  agent pipeline.
- No app sandboxing/permission system beyond `editable` + `capture` + mode.
- No remote app marketplace or install/update mechanism; apps are recipes,
  integrations, and built-ins the app already knows about.
- No startup garbage collection of orphaned owners (deletion flow only).
- No local per-item detach action (explicitly rejected; remote edits and app
  removal are the escape routes).
- No visual redesign of sections or tag headers (no badges, no group tint).
- No field-level `user_modified` tracking in v1.

---

## 9. Resolution status

All v1 open questions are resolved:

| v1 question | Resolution |
|-------------|------------|
| `captured_by` split vs read-only capture | One `managed_by` + `managed_mode` enum (#25); captured tasks fully normal (#26) |
| Remote Todoist edits to app-owned tasks | Remote edit unlocks the whole item and spares it (#27) + `user_modified` (#29) |
| Precedence when two apps claim an item | First owner wins; second claim errors (#28) |
| Regeneration vs `managed_editable` edits | Item-level `user_modified`; spared (#29); completion never counts (#30) |
| Completed items on regen / removal | Kept on both; become user history (#31) |
| Lock rendering in list-only usage | Hover-only lock glyph; double-click rename is inert (#32) |
| Seed data as built-in app | Yes; `is_seed` retired (#33) |
| Post-migration tag name | Renamed to a plain tag, e.g. "Travel" (#34) |
| `managed_tag` manifest field | Replaced by binding config; an app may manage multiple tags (#35) |
| Transactions | Raw `BEGIN`/`COMMIT`/`ROLLBACK` helper (#36) |

Residual items that are mechanical rather than product decisions:

1. The precise `with_transaction` signature and nesting behavior.
2. Whether a captured task moved out of its scope should have its `managed_by`
   cleared immediately or lazily on the next capture pass.
3. The names/labels of the built-in demo app and of the migrated travel tag.
4. Whether the attach picker sorts by `suggested_tags` or alphabetically.
5. Whether `co_ownable` (decision #38's escape valve) is in v1 at all, or a
   `full_tag` tag is simply closed to partial attachment.

---

## 10. Implementation status

Implemented and covered by tests (`cargo test -p storage`: 109 passed;
`cargo test -p todo-2`: 44 passed; `cargo check --workspace` clean):

- Schema: `libs/storage/toasty/migrations/0018_apps.sql` (apps table,
  `recipes.app_id`, `integrations.app_id`) and `0019_managed_scope.sql`
  (`app_tag_bindings`, `managed_by`/`managed_capture` on sections,
  `managed_by`/`managed_mode`/`managed_editable`/`user_modified` on tasks).
- `libs/storage/src/managed.rs`: `App`, `AppTagBinding`, `BindingRole`,
  `ManagedMode`, `TaskOwnership`, and the full API — app registry, bindings
  with first-come/full-tag conflict rules and project-tag rejection,
  ownership flags, `assert_task_editable` guard, `capture_task`,
  `release_app_from_tag`, transactional `disable_app`, `with_transaction`.
- Startup backfill `ensure_builtin_apps`: registers the builtin demo app, gives
  integrations and managed-tag recipes app rows, and converts legacy
  `tags.managed_by_recipe_id` tags into partial bindings (a Rust-side
  replacement for the spec's 0020 SQL backfill).
- Hard-block guard wired into `update_task_title`, `update_task_description`,
  `set_task_tags`, `delete_task`, and `update_blocked_until`;
  `update_task_done` stays exempt; guarded edits set `user_modified`.
- `TaskWithMeta` now carries `managed_by`, `managed_label`, `managed_mode`,
  `managed_editable`, `user_modified`, plus `is_managed_read_only()`.
- Travel app migrated to partial ownership (`trip.rs`): enable creates the app,
  attaches it partially, and owns only its sections; disabling tombstones open
  items and downgrades sections that still hold anything; the tag survives.
- Todoist: a remote edit to an owned task calls `unlock_task_from_remote`, so
  the remote value wins and the app spares it.
- UI: task rows block rename and show a hover-only lock with "Managed by X";
  the details pane refuses to start title/description/tag edits on read-only
  tasks.
- App settings are embedded per app (`ui_parts/apps.rs`, an `AppSettings`
  entity shared by the Automations and Integrations panels): each card shows a
  gear that expands its settings as a list of one-per-line rows — one row per
  managed tag with a capture toggle and per-tag **Detach**, an "Add a tag" row
  whose tag list marks project folders and tags held by a `full_tag` binding
  with the reason, an "Active runs" row with **Stop**, and an app-wide
  **Disable…** dialog asking whether its unfinished items go too (decision
  #39). There is no standalone Apps page; the builtin demo content is shipped
  data and never gains settings, so it is not listed at all.
- The old **Workflows** page is gone too: the Automations panel absorbed its
  run cards (per-recipe, with Complete / Approve / Reject / Fire event / Cancel
  run) and the coding "Branches to clean up" rows, and the Start buttons that
  duplicated each card's Enable. The navbar footer now has Automations and
  Integrations only, and each card's Enable opens the settings it must have:
  for a tag-owning automation that is the tag list, from which the user either
  attaches an existing tag or creates the recipe's own.
- Attaching is functional, not just a row: `ensure_recipe_sections`
  provisions a recipe's sections inside the chosen tag, and `create_trip`
  now targets the *selected* tag, replacing the app's own unfinished items
  there while sparing the user's tasks and completed items.
- Todoist capture push (`push_todoist_new_task`): `item_add` is sent with a
  client `temp_id` and the real remote id is read back from
  `temp_id_mapping` and linked. Linking a remote project/section gives the
  integration's app a capturing binding (`ensure_capture_binding`), and
  `insert_task` runs capture after assigning the tag, and the push that
  capture triggers runs in the background so the new row never waits on the
  provider round trip; a task typed into a linked tag still appears in
  Todoist. The details-pane tag editor captures the same way (and pushes in
  the background too) when an existing task is tagged into a linked tag, and
  a task already mirrored for that integration is never added twice.
  Disconnecting an integration disables its app and releases those
  bindings.
- `is_seed` is retired: `0020_seed_ownership.sql` moves existing demo rows
  onto the builtin app (captured, so they stay editable but never sync) and
  `0021_drop_is_seed.sql` drops `tasks.is_seed` / `tags.is_seed`. Seeding
  and the Todoist push guard now read app ownership instead of the marker.
- `tags.managed_by_recipe_id` is retired too: the Rust backfill
  (`migrate_legacy_managed_tag`) is gone — every database opened by a build
  that contained it has already had its legacy tags converted into partial
  bindings — and `0022_drop_managed_by_recipe_id.sql` drops the column, so
  `app_tag_bindings` is the single source of recipe ownership.
- Startup integrity check: `check_managed_integrity` reports (never repairs)
  `managed_by` / `app_id` columns naming a missing app and `app_tag_bindings`
  naming a missing app or tag, as a `ManagedIntegrityReport` with
  `is_clean()`, `issue_count()` and a capped per-category `describe()`. It is
  logged at startup via `log_managed_integrity` from `TodoStore::new`, and
  never fails startup.

Not yet implemented (deliberately staged, no code left in a broken state):

- Disabled-with-lock rendering in the details pane (fields are inert rather
  than visibly disabled).
- `with_transaction` was added but is only used by `disable_app`; multi-item
  generation in `create_trip` is not yet wrapped.
- Only two of the four surfaces of decision #18 exist: the per-app settings
  block (in the Automations and Integrations panels) and the task
  row/details. There is still no tag-settings panel or section context menu,
  so capture policy and section ownership are only visible on the app's own
  card.
- The integrity check does not cover the *double representation* of
  sections (§7.1.1): a `tag_sections.managed_by` disagreeing with its child
  tag's `managed_by` is not reported, because matching the two needs the
  section's display label. It also does not repair anything, so a report
  currently has no in-app surface beyond the log.

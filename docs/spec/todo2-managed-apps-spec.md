# todo2: Unified Managed Tags / Sections / Tasks Spec

Status: Draft v1 — design interview complete, no code changes made yet.

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
- An app should manage **a handful of tasks** in a normal tag or a project/dir
  tag, with no section of its own.
- An app should be able to **take partial ownership of an existing tag** the
  user already has. Canonical use case: *"I already have a Travel checklist in
  Todoist. I want the travel mini-app to add templated tasks to it when I
  create a new trip."*
- The app decides whether the content it manages is **editable**.
- The user can still add **unmanaged tasks** to the same tag/section when the
  app's settings allow modification.

---

## 2. Design decisions (from the interview)

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
| 17 | Escape hatch | **None.** A user can only complete items or disable the app (which removes its content). |
| 18 | Ownership controls live in | App settings panel, tag settings, section context menu, task **detail pane** (not the task context menu, no navbar marker). |
| 19 | Managed tasks in global list | **Yes** — listed in All tasks/priority, usable as blockers/follow-ups. |
| 20 | Multi-item generation failure | **All-or-nothing** transaction. |
| 21 | App deletion flow | UI asks whether to delete managed items; then **one DB transaction: tombstone app-owned items → disable app**. |
| 22 | Editable flag semantics | **Dynamic, app-controlled** — the app can flip editable ↔ read-only at any time. |
| 23 | Orphans | Handled by the deletion flow (block/cleanup); no unconditional startup GC. |
| 24 | Capture (new) | General case: **apps decide whether they "capture" new tasks added to a tag/section they manage.** Todoist's app captures new tasks added to a linked tag so they are also added to Todoist. |

Additional detail captured from the interview:

- The Todoist app should behave like every other app: when a task is added to a
  tag the Todoist app is linked to, that task is *captured* and pushed to
  Todoist. This is a per-binding/per-section policy, not a property of Todoist
  alone.

---

## 3. Model changes

### 3.1 New `apps` table

The single registry of things that can own content.

```
apps
  id            INTEGER PRIMARY KEY AUTOINCREMENT
  kind          TEXT NOT NULL            -- 'recipe' | 'integration' | 'builtin'
  slug          TEXT NOT NULL UNIQUE     -- stable identifier, e.g. 'travel', 'todoist'
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

An app's **manifest** (declared in the recipe JSON for kind='recipe', in
integration config for kind='integration') gains management declarations:

- `managed_tag: Option<String>` — keeps the "create my own tag on enable"
  behavior for apps that want it.
- `default_editable: bool` — the app's default per-item editability.
- `default_capture: bool` — whether new user tasks in a managed scope are
  captured by default.

`managed_by_recipe_id` on `tags` is **deprecated** and replaced by
`managed_by` (see below); the column can be dropped once the migration is done.

### 3.2 Bindings: which app is attached to which tag

Partial ownership needs a binding row, because co-ownership is allowed and the
attach/capture policy is per (app, tag).

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

- `role = 'full_tag'` preserves today's managed-tag behavior for apps that
  still create and own a whole tag.
- `role = 'partial'` is the attach-to-existing-tag case. The tag itself stays
  `managed_by = NULL` and survives the app being disabled.

Directory-backed `project:/path` tags are **not eligible** for bindings.

### 3.3 Owner columns

Nullable owner columns on the three content tables:

```
tags
  managed_by        INTEGER NULL   -- -> apps.id; non-NULL only for full-tag ownership
  -- (managed_by_recipe_id is deprecated)

tag_sections
  managed_by        INTEGER NULL   -- -> apps.id; NULL = user-created section
  managed_capture   INTEGER NULL   -- per-section override of the binding's capture policy

tasks
  managed_by        INTEGER NULL   -- -> apps.id; NULL = user task
  managed_editable  INTEGER NULL   -- dynamic, app-controlled per-item editability
```

Semantics:

- `tasks.managed_by` non-NULL means the app owns the task: hard-blocked edits,
  completion still allowed, and regeneration may replace it.
- `tasks.managed_editable` is the **dynamic** flag the owning app flips. Even
  when true, blocked operations remain blocked until the app flips it; the flag
  never grants the app itself extra rights (the app writes through its own
  privileged store path).
- A **captured** task (see §3.4) is *not* necessarily a managed task. Capture
  must not silently make the user's own task read-only. Proposed split:
  - `managed_by` = ownership/read-only/regeneration.
  - `captured_by` (new nullable column on `tasks`, plus reuse of the existing
    link rows for sync identity) = propagation, e.g. push this task to
    Todoist.
  - A user task created in a Todoist-linked tag is `captured_by = todoist app`
    and `managed_by = NULL`, so the user keeps full edit rights while the task
    still syncs.

### 3.4 Capture

When a task is created (or moved/assigned) into a scope where an app's capture
policy is on:

1. Resolve the applicable bindings for the destination tag and, if the task
   lands in a section, for that section.
2. For each app with `capture_new_tasks` (binding) or `managed_capture`
   (section) set, run the app's capture hook.
3. Capture hooks are provider-specific; the Todoist hook creates/link the
   remote task immediately and records an `external_task_links` row.
4. Capture must be non-destructive: a failing capture hook logs and leaves the
   local task untouched (the existing sync push path already behaves this way).

### 3.5 Sections are represented twice — keep both in sync

Sections currently exist as a `tag_sections` row **and** as a child tag via
`tag_implications` (tasks attach to the child tag; `section_groups_for_tasks`
uses the child tags; `tag_sections` supplies names and order). Any ownership
change must therefore be written to **both** representations, or the model
must pick a single source of truth. This spec keeps both but requires that:

- creating an app-owned section writes the `tag_sections` row and the child tag
  with the same `managed_by`;
- removing a section removes both;
- a consistency check (test + optional repair on load) asserts that for every
  app-owned `tag_sections` row there is a child tag with the same owner.

### 3.6 Migrations (proposed numbering: 0018+)

1. `0018_apps.sql` — create `apps`, add `recipes.app_id` and
   `integrations.app_id`.
2. `0019_managed_scope.sql` — add `tags.managed_by`,
   `tag_sections.managed_by`, `tag_sections.managed_capture`,
   `tasks.managed_by`, `tasks.managed_editable`, `tasks.captured_by`; create
   `app_tag_bindings`.
3. `0020_backfill_managed.sql` — data migration:
   - create an `apps` row (`kind='recipe'`, `slug='travel'`) for each recipe
     with a `managed_tag`, and a `apps` row (`kind='integration'`) per
     integration;
   - for the existing `managed:packing-list` tag: create a `partial` binding;
     set `managed_by` on its section child tags / `tag_sections` rows and on
     the tasks carrying those sections; clear `tags.managed_by_recipe_id` and
     leave the tag's `managed_by` NULL so the tag becomes an ordinary tag;
   - derive bindings from `external_tag_links` for integrations, with
     `capture_new_tasks` matching today's "assign to tag then push" behavior.
4. Later: drop `tags.managed_by_recipe_id`.

Note: SQLite `ALTER TABLE ADD COLUMN` is fine, but backfills with joins need
plain `UPDATE ... WHERE` statements; the migration runner splits statements and
has no variable/transaction support (§7).

---

## 4. Store API changes (libs/storage)

New/renamed functions, roughly:

```
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
  set_task_managed(task_id, app_id, editable)
  set_task_editable(task_id, editable)              // dynamic flag flip
  set_section_managed(section_id, app_id, capture)  // writes both representations
  managed_tasks_in_tag(tag_id, app_id) -> Vec<Task> // regeneration scope
  managed_sections_in_tag(tag_id, app_id) -> Vec<TagSection>

lifecycle
  disable_app(app_id, remove_owned_items: bool)     // one transaction
  capture_task(task_id)                             // run capture hooks for the destination scope

guard
  fn assert_task_editable(task_id) -> QueryResult<()>  // hard-block enforcement
```

Every existing mutating task API (`update_task_title`,
`update_task_description`, `set_task_tags`, `delete_task`, `update_task_done`,
…) calls the guard and returns a typed `TaskLocked { task_id, app_id }` error
when the task is app-owned. `update_task_done` is explicitly exempt.

`disable_app` runs as a single transaction: tombstone all items with
`managed_by = app_id` (when `remove_owned_items`), delete its bindings,
optionally delete the tag for `role='full_tag'`, then set
`apps.enabled = 0`.

---

## 5. App / automation behavior

### 5.1 Travel app (migrated)

- Becomes an app of `kind='recipe'`, `slug='travel'`, with a `partial` binding
  to the user-chosen tag (defaulting to the pre-existing `managed:packing-list`
  tag after migration, or to any tag the user picks in settings).
- "New trip" still runs the recipe and generates its checklist, but now:
  - it **replaces** the app-owned items in the sections it owns (see §6.3)
    rather than assuming it owns the whole tag;
  - user tasks in the same tag/sections are untouched.
- Disabling the app removes its sections and items; the tag survives.

### 5.2 Todoist app (migrated to the unified model first, behavior unchanged)

- Becomes an app of `kind='integration'`, `slug='todoist'`.
- Its project/section tag links become bindings with
  `capture_new_tasks = true`, which reproduces today's desired behavior: adding
  a task to a Todoist-linked tag also creates it in Todoist.
- Synced tags/sections get `managed_by = todoist app` **only for the sync
  fields**; the sync path (import/merge/push) is a privileged writer and is not
  subject to the hard-block guard. Local user edits to synced fields continue to
  work exactly as today.

### 5.3 Any app

- Apps declare defaults in their manifest (`default_editable`,
  `default_capture`, optional `managed_tag`).
- Apps own items by setting `managed_by`/`managed_editable` when creating them,
  and may flip `managed_editable` later (dynamic).
- Apps are responsible for their own regeneration strategy through the shared
  helpers; the model only provides the scope query.

---

## 6. UX

### 6.1 Attaching to an existing tag

1. The user opens the app's settings panel (installed apps list).
2. They choose **"Manage an existing tag"**, which lists eligible ordinary tags
   (directory-backed `project:` tags are excluded and grayed out with a reason).
3. Selecting a tag creates a `partial` binding; the app's panel then appears
   when that tag is open.
4. Tag settings shows the binding with a **Detach** action; the app's panel
   shows all its bindings.

### 6.2 Locked fields

- Read-only managed tasks render with a **lock icon on the editable fields**
  (title, description, tags, deadline/priority, delete, reorder) in the task
  detail pane and task row.
- The lock's tooltip names the owning app ("Managed by Travel checklists").
- Completion (checkbox, and the done toggle in details) stays enabled.
- Section rows show their owner only in the **section context menu**; there is
  no section badge or header treatment, per decision #10.

### 6.3 Regeneration

- "Create new trip" / "refresh" computes the app's desired items, then in one
  transaction removes the app's own uncompleted items in the owned scope and
  recreates them from the template.
- Completed app-owned items are kept (they are history) unless the app's
  manifest says otherwise.
- User tasks are never moved, renamed, or removed; because app items are
  recreated, user tasks may visually shift relative to them.

### 6.4 Disabling / deleting an app

- The disable/remove dialog lists what will go, based on the app's
  `managed_by` items, and asks whether to delete them.
- Confirm runs the single transaction of §4 (`disable_app`).
- Previously completed app-owned items are included in the tombstone set.

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
2. **Capture vs. managed is overloaded.** Without the `captured_by` split,
   capturing a user's new task into Todoist would instantly make it read-only,
   which is unacceptable. This is the single biggest modeling pitfall.
   The split is proposed here as a decision; it is not yet confirmed.
3. **Hard block vs. sync.** Managed tasks sync like any other (decision #14),
   so a remote client (or Todoist UI) can rename a managed task without going
   through the local guard. The hard block becomes local-only. Options: accept
   it (remote edits win, the app re-asserts on next generation), or refuse to
   sync app-owned fields. **Unresolved.**
4. **`managed_by` vs `workflow_run_id`.** Workflow steps already belong to a
   run. Under the new model a travel recipe step is both a run step and an
   app-owned task. Precedence must be defined (proposal: the recipe's app is
   the owner; `workflow_run_id` stays engine bookkeeping).
5. **No transaction support in the storage layer today.** Decisions #20 and #21
   require atomic multi-row writes; the codebase currently issues single
   statements through `toasty::sql`. Either a `BEGIN`/`COMMIT` raw-SQL helper
   or a toasty transaction API is needed. This is a hard prerequisite, not a
   nice-to-have.
6. **No FK enforcement / orphans.** SQLite does not enforce FKs here, so a
   `managed_by` can dangle. The chosen deletion flow covers app-initiated
   deletion, but a crash mid-transaction or a direct DB edit can still orphan
   rows; without a startup GC, "managed by unknown" reads are possible.
7. **`is_seed`, `workflow_run_id`, and `managed_by` are three "not a normal
   user task" markers.** They overlap in intent and will be confusing in
   queries and UI; consider whether seed data should be expressed as a
   built-in app.
8. **Binding uniqueness vs. co-ownership.** `UNIQUE(app_id, tag_id)` prevents
   one app attaching twice to a tag, but nothing stops two apps from claiming
   the same section or the same task. A precedence rule (first writer wins?
   last writer wins? conflict error?) is needed.
9. **`managed_editable` is dynamic, but item identity for regeneration is the
   owner column only (decision #13).** If an app flips an item to editable and
   the user then renames it, the next "replace app-owned" pass will still
   delete it, silently discarding the user's edit. Dynamic editability and
   replace-on-regenerate contradict each other unless regeneration spares
   items the user modified.

### 7.2 UX problems

1. **Invisible ownership.** With only a lock icon and no section/tag badges, it
   is easy to open a tag, see tasks that cannot be renamed or deleted, and not
   understand why. Discoverability relies entirely on the tooltip and the
   detail pane.
2. **No escape hatch.** Decision #17 means a single unwanted generated item can
   only be escaped by completing it (wrong — it may not be done) or disabling
   the whole app and losing all of its content. This will be the most common
   complaint. A per-item "detach" was explicitly rejected, but the
   consequences should be revisited before implementation.
3. **Locked delete with no alternative.** A user who adds a task to a managed
   section (allowed, fully normal) and later wants it out of that section must
   rely on re-tagging, which is blocked on the *app's* items but not theirs —
   so the same list mixes two editability regimes with no visual difference
   except a small lock.
4. **Two settings entry points.** Attaching lives in the app's settings panel,
   detaching in tag settings and the section menu. Users will look in the wrong
   place.
5. **Deleting an app silently removes completed history too.** The dialog lists
   what goes, but completed items are easy to overlook.
6. **Regeneration reshuffles user tasks.** Because app items are recreated, the
   user's own items may jump around the list on every "create new trip", which
   feels unstable.
7. **Capture surprise.** A user adds a task to a synced tag and it silently
   appears in Todoist (and vice versa, remote edits appear locally). This is
   existing behavior, but the new model makes capture an explicit app setting,
   so it needs to be visible somewhere (tag settings) or it will feel like
   magic.
8. **Partial ownership has no obvious home in the sidebar.** There is no
   navbar marker (decision #18), so a user cannot tell which tags have an app
   attached without opening each one.
9. **`project:` tags being off-limits is surprising.** A user may reasonably
   expect a travel checklist app to work on a project tag; the UI must explain
   the exclusion rather than silently omitting those tags.
10. **Section ordering conflicts.** Sections can be app-owned and user-created
    in the same tag; the app controls its own order and the user controls
    theirs, but only one `position` sequence exists.

### 7.3 Things most likely to confuse

- Why a task I can check off cannot be renamed.
- Which of the two "Travel" things is mine (the tag, or the app's binding).
- Why disabling the app deleted a tag yesterday but not today (migration from
  full-tag ownership to partial ownership changes the consequence).
- Why a completed item I can see will disappear if I remove the app.
- Why adding a task to a Todoist-tagged list also created it in Todoist.
- Why the app's panel appears in a tag that is otherwise mine.
- Why `project:` tags can't be attached.
- Why a section shows a lock in its menu but no visible badge in the list.

---

## 8. Non-goals

- No change to the recurrence engine, workflow node semantics, or the coding
  agent pipeline.
- No app sandboxing/permission system beyond `editable` + `capture` flags.
- No remote app marketplace or install/update mechanism; apps are recipes and
  integrations the app already knows about.
- No startup garbage collection of orphaned owners (deletion flow only).
- No per-item escape hatch / detach (explicitly rejected).
- No visual redesign of sections or tag headers.

---

## 9. Open questions

1. Confirm the `captured_by` vs `managed_by` split (§7.1.2), or accept that
   capture implies read-only.
2. How should remote (Todoist) edits to app-owned tasks be handled given the
   local hard block? (§7.1.3)
3. Precedence when two apps claim the same section or task (§7.1.8).
4. Does regeneration spare user-modified items when `managed_editable` is on?
   (§7.1.9)
5. Do completed app-owned items survive regeneration, and are they tombstoned
   on app removal? (Assumed yes to both.)
6. Where exactly does the lock render for a task row that never enters the
   detail pane (list-only usage)?
7. Should seed data become a built-in app, retiring `is_seed`? (§7.1.7)
8. Does `managed:packing-list` keep its name after migration, or is it renamed
   to the user's tag with the app's label shown in tag settings?
9. Should the recipe manifest's `managed_tag` remain for "I create my own tag"
   apps, or be expressed as an automatic binding created on enable?
10. Is a `BEGIN`/`COMMIT` helper acceptable for atomicity, or should the
    storage layer grow a real transaction abstraction first? (§7.1.5)

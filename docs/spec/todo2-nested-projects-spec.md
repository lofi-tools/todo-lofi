# todo2: Nest project dirs under tags

Status: Draft v1 — request interviewed over 7 rounds plus a follow-up round on
expansion, all product questions resolved. No code changes made yet.

Purpose: let a tag own a place in the navbar tree, so that a project directory
(a local git repo) can be nested *under* the tag it belongs to instead of
sitting at the top level next to every other tag and project. The placement is
made from a new **tag settings popover** — a gear beside the tag title in the
task list — which also grows into the general tag settings surface the
managed-apps work has been missing (dirs, sections, app bindings).

---

## 1. Request and how it was read

The prompt was: *"todo2: nest project dirs under tags (or tags under project
dirs) — foresee problems?"*

The parenthetical offered a direction choice. It was resolved towards **tags as
parents, project dirs nested under them**; the reverse (project dirs as
parents, tags nested under them) is explicitly not built. The "foresee
problems" half of the request is answered by §5, which is the substantive part
of this document: the feature is small in UI terms and touches several
structural assumptions (a DAG used as a tree, a name that disagrees with the
new source of truth, an existing dangling-edge bug, lazily-loaded children).

Facts about today's code that the design leans on:

- A project dir **is** a tag. The home-directory scan
  (`apps/todo-2/src/projects.rs`) and the project picker create
  `project:{abs-path}` tags whose `display_name` is the directory name
  (`libs/storage/src/tag.rs::get_or_create_project_tag`). There is no separate
  "project" entity.
- Tags already nest via `tag_implications(implier_id, implied_id)` where the
  *implier* is the child and the *implied* is the parent
  (`get_children(parent_id)` selects rows whose `implied_id = parent_id`). The
  edge is a DAG edge, not a tree edge: multi-parent is allowed and
  `add_tag_implication` only rejects self-edges and cycles
  (`would_create_cycle`).
- **Nothing in the app UI can create an implication edge today.**
  `add_tag_implication` is called only from storage-internal code: Todoist
  sections (`todoist.rs`), app-managed sections (`managed.rs`), the travel
  recipe (`trip.rs`), and seed data (`testing.rs`). The DAG exists as an
  implementation detail of sections and sync.
- Sections are **double-represented** as noted in `todo2-managed-apps-spec.md`
  §3.5: a `tag_sections` row plus a child tag linked by an implication.
  `add_tag_section` writes only the `tag_sections` row; callers create the
  child tag.
- Selecting a tag already aggregates its whole descendant closure:
  `list_tasks_by_tag_impl` seeds from the tag *and* `get_all_descendants`.
- The navbar shows top-level tags flat, and expands children **only along the
  selected path** (`collect_visible_tags` descends when
  `selected_path.iter().any(|p| p == &tag.name)`), fetching them lazily through
  `get_children` into `children_cache`. `get_children` and
  `get_top_level_tags` have **no `ORDER BY`**. (#14 / #31 replace this walk with
  a bulk-loaded, fully expanded tree.)
- `VisibleTag::has_children` is computed but discarded at render
  (`has_children: _has_children`); there is no chevron or disclosure marker.
- `delete_tag` deletes the tag row only — it does **not** clean
  `tag_implications`.

---

## 2. Decisions (interview rounds 1–7, plus a follow-up round on expansion)

| # | Question | Decision |
|---|----------|----------|
| 1 | Direction | **Tags are parents; project dirs nest under them.** No tags-under-projects view. |
| 2 | What defines the relationship | **An explicit parent/child assignment** — a real `tag_implications` edge, not membership derived from task tags. |
| 3 | Persisted or a lens | **A pure view over existing edges: no schema change, no new tables, no migration.** (Writing edges with the existing API is expected — see #4.) |
| 4 | How a project gets placed | **A new tag settings popover**: a gear right of the tag title at the top of the main task list pane; clicking it overlays a popover card over the pane content. It contains a **tag picker** (the same chip-based picker the task details pane uses) and, for directory-backed tags, a **dirs picker**. |
| 5 | Scope | **Navbar plus selection semantics.** Creation flows are untouched (#29). |
| 6 | Rendering of children | **Uniform** — any child tag renders the same way. A project tag, a plain tag, and a section tag are all just child rows; the tree may reach 3+ levels (`work > todo-lofi > backlog`). |
| 7 | Which nodes count as a "project dir" | `project:{path}` tags for the purposes of the *name* only; **dirs are the real source of truth** (#25), so project-ness in the UI is directory-backed-ness (#19, #25). |
| 8 | Unplaced projects | **Stay top-level, exactly as today**, alongside normal tags with the folder icon. |
| 9 | Number of parents | **Multiple parents allowed (DAG).** A project can sit under several tags and appears once under each; duplicated rows are expected and accepted. |
| 10 | Allowed parents | **Any tag, including another project tag** (a project may nest under a project). |
| 11 | Popover contents | **All of it**: parent tag picker, dirs picker, section management, and app bindings (capture toggles, per-tag detach, section ownership). One full tag settings surface. |
| 12 | Dirs picker semantics | **The full list, replaceable.** Any tag can be given dirs and thereby become directory-backed. |
| 13 | Picker guards | **Filter invalid choices out of the list**: the tag itself, its descendants, and tags it is already a child of never appear. |
| 14 | Expansion | **Fully expanded, always** — revised in the follow-up round (originally "keep today's lazy path-only behavior"). Children of every tag render with no interaction: the navbar bulk-loads the whole tag graph. No chevron, no counts, no expand state. |
| 15 | Deleting a parent tag | **Placed projects fall back to the top level.** Requires `delete_tag` to clean up the implication rows (it does not today). |
| 16 | Spec boundary | **This spec covers the whole panel**, not just parent + dirs. |
| 17 | Aggregation on select | **Unchanged: the whole subtree.** Selecting `work` shows its own tasks plus every task in every project placed under it, and their sections. Nesting therefore changes what a tag view shows, by design. |
| 18 | Breadcrumb | **None.** The navbar's highlighted row is the only indication of where you are. |
| 19 | Project look | **Any directory-backed tag gets the folder icon** (project: tags and tags with dirs alike), matching the agent pane's notion of a project. |
| 20 | Tests | **Storage tests plus GPUI UI tests.** |
| 21 | Child ordering | **Grouped by kind, then alphabetical within each group**: projects, then plain tags, then sections. |
| 22 | Rename / delete in the panel | **Neither.** The popover is placement and settings only; tag lifecycle stays where it is. |
| 23 | Entry points | **The task list header gear plus a navbar row context menu** on the tag row, since that is where nesting is visible. |
| 24 | `project:` name vs dirs | **Keep the name; dirs are the source of truth.** A stale `project:{path}` name is tolerated; the prefix becomes legacy. |
| 25 | App-binding eligibility | **Any directory-backed tag is ineligible for app bindings** (`is_project()` OR non-empty dirs) — the managed-apps rule is widened to match the agent pane's definition of a project. |
| 26 | Removing a placement | **In the popover's tag picker**, which is the details-pane chip editor: an input with inline tag chips, each gaining a small cross on hover. Clicking the cross prompts a **confirmation modal** before the edge is removed. The picker itself only ever *adds* (it filters out existing parents, #13). |
| 27 | Save model | **Immediate, per action.** Every pick/toggle/reorder writes through as it happens; no staged edits, no Save button, no revert. |
| 28 | Picker row labels | **Flat label with the path as secondary text** (smaller/secondary), in every tag picker. |
| 29 | Creation flows | **Unchanged.** The `+` picker (Tag / Project / Todoist tabs) and the new-tag modal gain no parent step; placement happens afterwards in tag settings. |
| 30 | Collapsed-tree signal | **None needed — nothing is collapsed.** Project tags appear pre-expanded (their sections render with them), so there is no collapsed state to signal and no chevron or count affordance. |
| 31 | Navbar data loading | **Bulk-load the tag graph**: all tags plus every `tag_implications` edge in one load, tree built in memory, replacing `list_top_level_tags` plus the lazy `get_children` walk along the selected path. |

---

## 3. Behavior

### 3.1 Placement

- A placement is one `tag_implications` row: `add_tag_implication(child_id: project_tag_id, implied_id: parent_tag_id)`.
  `get_children(parent_id)` therefore returns the projects placed under a tag
  with no new query and no new column.
- Placement is per-edge, so the same project under three tags is three rows and
  three visible rows in the navbar. Removing one placement removes one edge.
- The validity rules are the store's existing ones (reject self, reject
  cycles) plus the picker's filter (#13). The picker is the only writer, so a
  rejected choice should be unreachable — but the store errors are still
  surfaced if one slips through.

### 3.2 What the tree renders

- The row decoration is kind-driven: **folder icon** for any directory-backed
  tag (project: *or* dirs, decision #19 / #25), `#` for everything else
  (existing behavior).
- **Every child row renders, always** (#14 as revised). `collect_visible_tags`
  no longer gates descent on `selected_path`; it walks the whole in-memory
  graph, so `work > todo-lofi > backlog` is fully laid out on load. Nothing is
  collapsed, so there is no chevron, count, or expand state (#30).
- Any implication child renders uniformly (#6). No new row kinds, no group
  headers inside the tree, no badges for "this is a placed project".
- The tree is a DAG rendered as a tree: a multi-parented project appears under
  each parent, and each copy renders its own subtree. Depth is unbounded
  (project under project under tag), which is why the walk needs a per-path
  visited guard (§5.2.1).

### 3.3 Ordering (decision #21)

Children of a tag sort by kind group, then alphabetically by `label()`:

1. projects (directory-backed tags),
2. plain tags,
3. sections.

Two implementation notes that fall out of this:

- `get_children` has no `ORDER BY`, so the order must be imposed either by a
  new/changed store query or by sorting in the navbar. Either way the sort must
  be **total and deterministic** (ties broken by id) — otherwise duplicated
  rows from multi-parent placement reorder between renders. The same fix is
  wanted for `get_top_level_tags`, which is also unordered today.
- Detecting "is a section" needs more than the tag row: a child tag is a
  section of its parent when it corresponds to a `tag_sections` row of that
  parent (names are built by `section_tag_name`). If the navbar cannot resolve
  that cheaply, either derive it from the name shape or fall back to
  "sections last, sorted alphabetically among themselves" using the same
  `tag_sections` fetch it already needs for §3.10.

### 3.4 Unplaced projects (decision #8)

Top-level rows, unchanged, folder icon and all. No "Projects" pseudo-node, no
"Ungrouped" group.

### 3.5 Selection and aggregation (decisions #17, #18)

- Clicking any row keeps the existing path-based navigation
  (`selected_path: Vec<String>` of tag names → `TagSelected(path)`).
- Because `list_tasks_by_tag_impl` already includes `get_all_descendants`, a
  parent tag's task view now includes the tasks of every project placed under
  it and those projects' sections. This is intended and is the visible payoff
  of the feature.
- No breadcrumb, no parent chips: the task list header stays as it is today.
- The same project reached through two parents is the same task set; only the
  navbar highlight differs.

### 3.6 Removing a placement (decision #26)

- The popover's tag picker is the details-pane chip editor: an input field with
  inline tag chips. Each chip gains a small cross on the right on hover.
- Clicking the cross does **not** write immediately: it opens a confirmation
  modal (the app's `window.open_dialog` pattern, as used by the app-disable
  dialog), and only on confirm is the edge removed
  (`remove_tag_implication(child, parent)`).
- The modal names the project and the parent it is being removed from, so a
  multi-parented project cannot be un-placed from the wrong tag by accident.
- The picker's list never contains a current parent (#13), so removal is only
  ever reachable through a chip.

### 3.7 Tag settings popover (decisions #4, #11, #22, #23, #27)

- Trigger: a gear icon right of the tag title at the top of the main task list
  pane, shown while a tag is selected (including a project tag; the gear is
  what exposes its dirs). Not shown for `All Tasks`.
- Second trigger: the navbar tag row's context menu, so placement can be read
  and edited where the nesting is visible. (The navbar rows have no context
  menu today — this is new plumbing; see §5.2.7.)
- Shape: the app's existing popover card pattern — an absolute card rendered as
  the **last child of the main layout** so it paints above the pane content,
  with outside-click dismissal state, exactly as `travel.rs::popover` and
  `repeat_picker.rs` do.
- Contents, top to bottom:
  1. **Placed under** — the tag picker (chips) for parent tags, with the
     hover-cross + confirm removal of §3.6. Chips show the flat label with the
     path as secondary text (#28). Because the picker is multi-parent, several
     chips may be present.
  2. **Directories** — the dirs picker (§3.8).
  3. **Sections** — list / add / rename / reorder the tag's sections (§3.9).
  4. **Apps** — bindings on this tag: capture toggle, per-tag detach, section
     ownership (§3.10).
- Everything applies immediately (#27): each pick, toggle, and reorder writes
  through on the spot. Esc or click-outside closes with nothing further to
  apply. No Save, no cancel, no revert.

### 3.8 Dirs picker (decision #12)

- Edits the tag's **entire** dirs list via the existing
  `set_tag_dirs(tag_id, Vec<String>)` / `add_tag_dir` API, with removal
  supported.
- Available for every tag, not just `project:` tags: adding a dir to a plain
  tag is how that tag becomes directory-backed (folder icon #19, agent pane
  available, app-binding ineligible #25).
- Picking a directory reuses the project-directory list the `+` picker already
  builds from the home-directory scan.
- **Order is semantic, not cosmetic:** the first existing candidate becomes the
  agent's `cwd`, and the rest become additional session roots
  (`acp_client.rs:138-161`), and the same resolved path is the agent-session key
  (§5.1.11). Reordering can therefore change which session resumes.
- The `project:{path}` name is deliberately *not* rewritten when the encoded
  path is removed or replaced (#24) — see §5.1.7 for the inconsistency this
  accepts.

### 3.9 Sections in the panel (decisions #11, #21)

- The panel lists the selected tag's sections (`tag_sections`, ordered by
  `position`) and offers add, rename, reorder (`reorder_tag_sections`), and
  remove (`remove_tag_section`).
- **Critical:** a section created here must be written to *both*
  representations — the `tag_sections` row **and** the child tag plus its
  implication edge — because `add_tag_section` writes only the former. Without
  the child tag, the section never appears in the navbar (§3.2) and no task can
  be attached to it (task tagging works by tag name). The child tag's name must
  be built with the existing `section_tag_name` convention so task rows and
  `section_groups_for_tasks` keep resolving.
- Removing a section likewise removes both, and any tasks still carrying the
  section tag must be handled explicitly (today's `remove_tag_section` only
  deletes the `tag_sections` row).

### 3.10 App bindings in the panel (decisions #11, #25)

- The panel absorbs the surfaces `todo2-managed-apps-spec.md` §10 lists as
  missing: the binding's capture toggle, per-tag **Detach**
  (`release_app_from_tag`), and the section ownership controls, so the app card
  is no longer the only place they exist.
- Eligibility widens per decision #25: a tag is ineligible for a binding when
  it is directory-backed (`is_project()` OR non-empty dirs). The existing
  rejection in `libs/storage/src/managed.rs` (which checks `tag.is_project()`)
  must move to that wider test, and the panel must explain the rejection with
  the same "it's a project directory" reason the attach flow already uses.
- Ordering hazard: adding a dir to a tag that already holds a binding, or
  attaching an app to a tag that then gains dirs, must be refused (or must
  release the binding) — the two features cannot both be allowed to win
  silently.

### 3.11 Untouched

- Rename and delete are not in the popover (#22); the tag keeps whatever
  affordance it has today.
- Creation flows are unchanged (#29): the `+` picker and the new-tag modal still
  create bare top-level tags/projects, and the user places them afterwards.

---

## 4. Store and app changes (no schema change)

The storage layer already has every primitive; the work is mostly *where* the
rules live:

```text
libs/storage (existing, reused)
  add_tag_implication(implier_id, implied_id)      -- placement edge; cycle-checked
  remove_tag_implication(implier_id, implied_id)   -- un-place one parent
  would_create_cycle / descendant walk             -- powers the picker filter
  tag_settings / add_tag_dir / set_tag_dirs        -- dirs picker
  tag_sections / add_tag_section / rename_tag_section /
    reorder_tag_sections / remove_tag_section      -- sections panel
  attach_app_to_tag / release_app_from_tag         -- app bindings panel

libs/storage (changed)
  delete_tag(id)              -- must delete the tag's implication rows in both
                                 directions (as implier and as implied) inside one
                                 transaction, so placed children fall back to the
                                 top level (#15) and sections do not dangle.
  attach_app_to_tag(...)      -- eligibility test becomes directory-backed, not
                                 is_project() (#25); must also refuse a tag whose
                                 dirs are added later while bound.
  get_children(parent_id)     -- deterministic total order (kind group, then label,
                                 then id).
  list_tags + all implication edges
                              -- the bulk pair the navbar loads once (#31) to build
                                 the tree in memory; may subsume
                                 get_top_level_tags / get_children for the nav.

apps/todo-2
  ui_parts/navbar.rs          -- kind-driven prefix (folder for dirs-backed), a
                                 context menu on rows, ordering, and full expansion:
                                 load all tags + edges once, build the tree in memory,
                                 walk it entirely with a per-path cycle guard, and
                                 drop children_cache / _fetch_children (#14, #31).
  ui_parts/tag_settings.rs    -- NEW: the popover (placed-under chips, dirs picker,
                                 sections list, app bindings).
  ui_parts/task_list.rs       -- the gear beside the tag title; shows the popover.
  ui_parts/task_details.rs    -- the chip editor is shared, not copied: extract it
                                 (or reuse it) so both surfaces behave identically.
  ui_parts/apps.rs            -- binding rows move/extend into the panel; the app
                                 card keeps its per-app view.
  main.rs                     -- popover open/close state lives with the layout's
                                 popover slot (last child), since the entry point is
                                 the task list header, not the navbar. Follows
                                 `sync_agent_project`'s directory-backed rule for the
                                 agent pane so navbar and pane agree.
```

---

## 5. Problems this design must confront

This is the answer to "foresee problems?". Ordered roughly by how likely each is
to actually bite.

### 5.1 Model and data

1. **`delete_tag` leaves dangling implication rows — and it is the chosen
   fallback path.** Today `delete_tag` deletes only the `tags` row.
   `get_top_level_tags` excludes any tag that appears as an `implier_id`, so a
   placed project whose parent is deleted is **invisible everywhere**: not
   top-level (it still has an edge) and not nested (its parent is gone). The
   decision that projects "fall back to top level" (#15) is therefore not a
   nicety, it is the fix for an existing bug that this feature turns from
   harmless into routine (every parent deletion now strands its projects).
   Deletion must remove edges in both directions — the parent's own incoming
   edges (its placements elsewhere) and its outgoing edges (its children) — in
   one transaction, and it must do the same for section child tags.
2. **A DAG rendered as a tree duplicates subtrees.** A project under three
   tags renders (with its sections) three times. Nothing dedups this, and no
   "seen" marker is possible because the same tag at two paths is legitimately
   two rows. Consequences: unbounded visual depth (project under project under
   tag), repeated section children, and a navbar that can grow faster than the
   tag count suggests.
3. **Path matching is by name and is sloppy.** `is_selected` compares only the
   last path segment, so with multi-parent placement **every copy of a
   duplicated row highlights** — and because #14 (revised) puts all copies on
   screen at once, the user sees the ambiguity rather than it being hidden
   behind the collapsed path. Selection paths are also lists of tag *names*
   rather than ids, and `navigate_to_tag` resolves them by walking the tree
   (`selected_path.iter().any(...)`, not a prefix check), which is the wrong
   shape once the same tag is reachable at several paths. Duplication makes this
   visible, not less.
4. **Ordering does not exist yet.** Neither `get_children` nor
   `get_top_level_tags` has an `ORDER BY`; decision #21 therefore cannot be
   met by rendering alone, and a non-total sort would make duplicated rows jump
   between renders.
5. **"Is this child a section?" is not a property of the tag row.** Deciding
   the kind group for ordering (#21) — and for hiding section children from
   the parent picker (§5.2.9) — needs the parent's `tag_sections`, i.e. a
   second fetch per tag.
6. **Sections are double-represented and the panel is a new writer of that.**
   `add_tag_section` writes only `tag_sections`; `trip.rs`, `todoist.rs`, and
   `managed.rs` each create the child tag + implication themselves. If the
   panel forgets, sections appear in the panel but not in the tree, and tasks
   cannot be tagged with them. If it creates only the child tag, the reverse.
   `todo2-managed-apps-spec.md` §7.1.1 already flags divergence as a corruption
   risk, and the integrity check deliberately does **not** cover it (it cannot
   match the two without the section's display label). This panel is the first
   place a *user* creates sections, so it is where that risk becomes real.
7. **Two sources of project-ness now disagree, and `project:` prefix readers
   are everywhere.** Decision #24 keeps the name while making dirs the truth,
   so a tag can be `project:/a/b` with no dirs, or `project:/a/b` plus a
   different dir, or a plain tag with dirs. Three readers must widen to the
   dirs-based rule (`managed.rs::attach_app_to_tag`, `navbar.rs`'s row prefix,
   `apps.rs::attach_blocker`), and three more are already dirs-aware but still
   seed their candidate directories from the name
   (`main.rs::sync_agent_project`, `store.rs::project_dir`,
   `agent_pane.rs::AgentProject::candidates`). Worst case: the agent pane still
   offers a directory the user removed, because it came from the name. **§9
   enumerates every call site with a verdict.**
8. **App-binding eligibility flipping is silent and retroactive.** Adding a dir
   to a tag changes three unrelated things at once: folder icon, agent-pane
   availability, and app-binding ineligibility. A tag that already holds a
   binding (or a `full_tag` binding that created the tag) becomes a
   contradiction — an app-owned "project" — the moment a dir is added. The
   store must refuse or release; the panel must say which.
9. **No FK enforcement, no repair path.** Per `todo2-managed-apps-spec.md`
   §7.1.6, orphaned rows are possible (crash, direct DB edit) and nothing
   garbage-collects them. Dangling implication rows have exactly the invisible
   failure mode of #1, and the existing `check_managed_integrity` report does
   not cover them.
10. **Deleting a tag that is *also* a parent has two different exits.** Its own
    placements elsewhere (edges where it is the implier) and its children
    (edges where it is the implied) both need handling, and the children may
    include sections (which should perhaps go too, not fall back to top level)
    as well as projects (which should). One rule cannot cover both; the
    decision table only covers "project falls back to top level".
11. **Editing dirs silently orphans the agent session.** Agent sessions are
    keyed by the *resolved project directory*, not by the tag: `StoredSession`
    documents `project_path` as "canonical project directory; the key" and
    carries `tag_id` as "for display only"
    (`libs/acp-client/src/session_store.rs:21-28`); the pane resolves the key
    from `AgentProject::candidates` and looks it up for resume
    (`agent_pane.rs:555-572, 620-628`). Because the dirs picker (#12) can
    replace the whole list, and because the candidate *order* decides which path
    becomes `cwd` (`acp_client.rs:138-161`: "the directories a session is scoped
    to, in the order the project lists them"), changing or reordering dirs
    changes the key: the next turn starts a **fresh session** and the old entry
    is stranded in `acp-sessions.json`. The fix is cheap because `tag_id` is
    already stored — key by tag id, or migrate the entry when dirs change — but
    it has to be decided before the dirs picker ships. See §9.G.
12. **Placement durability — confirmed: nothing persists.** Decision #3 rests on
    placements being ordinary rows, but the app opens `turso::memory:` and
    re-seeds at startup (`apps/todo-2/src/main.rs`: `StorageConfig { db_uri:
    "turso::memory:" }` followed by `store.seed()`, with the app's own comment
    "The database is in-memory, so re-register the Todoist connection row when
    tokens survived"). So a placement — like every tag edit the user makes —
    lives for one session. `libs/acp-client/src/session_store.rs:4-5` was right
    and the `todo.db` under `~/.config/my-todo` is written by
    `libs/storage/src/bin/migrate.rs`, not by the app. This is not specific to
    placements and is not a regression, but it means the feature has no durable
    effect until the store moves to a file, and that is worth stating up front
    rather than discovering after the fact.

### 5.2 UI and UX

1. **The tree is now fully expanded, so the navbar's cost and length change
   shape.** Decision #14 (revised) replaces the lazy path-only walk with a bulk
   load: all tags plus all edges, then a full in-memory walk each render. Three
   consequences. (a) The walk needs a **per-path visited guard** — the old
   laziness was incidental cycle protection, `add_tag_implication` prevents
   cycles on the write path, but a legacy or hand-edited DB can still contain
   one and an unguarded recursive walk will not terminate. (b) A multi-parented
   project renders its entire subtree once per parent, so row count multiplies
   with depth, and the navbar's existing scroll area is the only mitigation.
   (c) `VisibleTag::has_children`, `children_cache`, and `_fetch_children`
   become dead — delete them rather than leave two expansion mechanisms side by
   side. The payoff is that this is the only version of the feature where a tag
   with placed projects is *visible without being selected*, which is why the
   original "no signal" option was rejected.
2. **Removal is hover-only and confirms a mis-sized action.** The chip cross is
   invisible until hover, and the confirmation modal is the same one used for
   removing a plain tag — but removing a *project* placement hides a whole
   subtree from the tree while removing a plain tag placement hides one row.
   The modal text must distinguish the two, and the chip must show the path
   (#28) so the user knows *which* parent they are removing.
3. **"Immediate, per action" plus "confirm removals" is two different save
   contracts in one panel.** Adds, toggles, and reorders write instantly;
   removals stage behind a modal. A user who confirms and then presses Esc (or
   clicks outside) must not be left unsure whether the removal happened. The
   popover must render post-write state from the store rather than optimistic
   local state, or the two paths will drift.
4. **Multi-parent placement makes "where am I?" ambiguous, and breadcrumbs were
   rejected.** With no breadcrumb (#18), selecting the second copy of a project
   produces a task list identical to the first, and the navbar highlight is the
   only clue. Now that every copy is on screen at once (#14 as revised), both
   copies highlight simultaneously — `is_selected` compares only the last path
   segment — so the highlight does not even identify which one was clicked.
5. **Aggregation (#17) can produce a very large tag view.** Selecting `work`
   now pulls in every task of every placed project *and* their sections. Worse,
   section grouping is computed for the *selected* tag
   (`section_groups_for_tasks(tag_id, ids)`); the sections of child projects
   are not the selected tag's sections, so those tasks may render ungrouped or
   under no header at all. Nesting changes what a tag view shows, and the
   section header logic was written for a flat tag.
6. **The same tag can be both a parent and a project.** With #10 (any tag may
   be a parent) and #12 (any tag may gain dirs), a single row can be a folder,
   a parent of other folders, and a section bearing at once. One prefix slot
   cannot express all three; the icon decision (#19) only covers project-ness.
7. **The navbar has no context menu today.** Rows are plain `div`s with
   `on_click`; decision #23 requires right-click plumbing (component choice,
   focus, dismissal, keyboard equivalent) that does not exist in this codebase
   yet — worth a scout before committing to it, or the second entry point
   becomes the expensive half of the feature.
8. **Popover state now lives across two owners.** The trigger is in the task
   list (or the navbar) but the card must be painted by the layout's popover
   slot (last child, outside-click dismissal like `travel.rs`). The selected tag
   can change while the popover is open, so the open state must be keyed by tag
   id and closed (or re-targeted) on selection change — otherwise the panel
   edits the previously selected tag.
9. **Sections are placeable parents unless filtered.** The picker lists tags and
   the filter (decision #13) only excludes self, descendants, and existing
   parents. Sections are tags, so `work > todo-lofi > backlog` can be made a
   parent of another project. That is almost certainly meaningless; if so, the
   picker needs a fourth exclusion rule (or those rows need a disabled
   explanation), which needs the per-tag `tag_sections` fetch of §5.1.5.
10. **Dirs picker + `project:` name means the agent pane can lie** (§5.1.7), and
    there is no visible indication in the panel that the tag's name encodes a
    path that is no longer in the dirs list.

### 5.3 Things most likely to confuse

- Why a project vanished from the top level (it was placed under a tag, so it now
  appears under that tag rather than alongside it).
- Why a project row still appears under a tag after the user removed that
  directory from the project's dirs list.
- Why the agent lost its conversation after the tag's directories were changed or
  reordered (§5.1.11).
- Why adding a directory to an ordinary tag removed its app settings.
- Why `work` now shows tasks from projects that were never tagged with `work`
  (they were *placed* under it, and aggregation follows the descendants).
- Why the same project appears three times in the navbar.
- Why the same project shows up in the navbar but the previously selected copy
  still highlights.
- Which of the two copies of a project is being un-placed when the chip cross is
  clicked.
- Why sections created in the panel do not appear in the tree (a missed child
  tag write, §5.1.6).
- Why deleting a tag made a project disappear entirely rather than returning it
  to the top level (the `delete_tag` cleanup, §5.1.1).

---

## 6. Non-goals

- **No reverse direction.** Tags are not nested under project dirs; there is no
  project-parented view.
- **No filesystem hierarchy.** Project dirs are not grouped by their real path
  (`~/src/me/todo-lofi` does not sit under a `~/src/me` node).
- **No schema change.** No new tables, columns, or migrations; placement reuses
  `tag_implications`.
- **No creation-flow changes.** The `+` picker and new-tag modal gain no parent
  step.
- **No rename/delete in the tag settings panel.**
- **No breadcrumb, no parent chips in the task list header.**
- **No collapse affordance.** No chevron, no expand/collapse state, no persisted
  expansion; the tree is always fully expanded and its length is bounded only by
  the scroll area.
- **No ordering UI.** Child order is derived (kind group, then alphabet), not
  draggable.
- No change to the recurrence engine, workflow semantics, agent pane internals,
  or the Todoist sync protocol.

---

## 7. Open items (mechanical, not product decisions)

1. The exact within-group order: projects → plain tags → sections, or
   projects → sections → plain tags. (The decision fixes "grouped by kind";
   the group order above is the draft's choice.)
2. Whether section child tags are excluded from the parent picker (§5.2.9) or
   simply allowed.
3. How the always-expanded walk guards against a cycle that reached
   `tag_implications` outside the store API: a per-path visited set, a hard depth
   cap, or both (§5.2.1).
4. Which GPUI component provides the navbar row context menu, and what the
   keyboard equivalent is (§5.2.7).
5. Whether the placement helpers get named store methods
   (`place_tag_under_tag` / `unplace_tag_from` wrapping
   `add_tag_implication`) or the panel calls the implication API directly.
6. Whether the `project:` name is eventually normalized away now that dirs are
   the source of truth, or kept as permanent legacy.
7. Section removal semantics when tasks still carry the section tag (untag,
   move to the parent tag, or block the removal).

---

## 8. Verification plan

Storage (`cargo test -p storage`):

- placement writes an implication edge and `get_children` returns the project;
- multi-parent placement returns the project under each parent, once per parent;
- cycle and self placement are rejected; duplicate placement is a no-op;
- `delete_tag` removes the tag's edges in both directions in one transaction and
  of a parent: the placed project reappears in `get_top_level_tags` (regression
  for §5.1.1);
- ordering: kind group then label then id, deterministic across calls;
- `attach_app_to_tag` rejects a tag with non-empty dirs as well as a
  `project:` tag, and refuses (or releases) when dirs are added while bound;
- a section created through the panel path exists as both a `tag_sections` row
  and a child tag whose name matches `section_tag_name`, and is a child in
  `get_children`.

The tree shape the rows render from (nesting, kind order, duplication, the
cycle guard, dirs-backed classification) is asserted in the storage tests
listed above, because the navbar is a mapper over `TagTreeRow` and holds no
logic of its own.

App (`cargo test -p todo-2`):

- the placement picker omits the tag itself, its descendants, and its existing
  parents (unit test over `eligible_parents`, which is why that filter lives in
  a free function rather than inline in the render).

**Correction:** this workspace has no `#[gpui::test]` anywhere — every app test
is a plain unit test — so "GPUI UI tests" as originally written here cannot be
delivered without first building a test harness (a window, a Tokio-backed
store, and a fixture). The render-level behaviour below is verified by hand
instead:

Manual walkthrough: place a project under a tag and confirm the project row
appears under that tag immediately (no expanding, no selecting); confirm the
tag's task list gains that project's tasks; un-place it and confirm the row
disappears from the tree but the project stays top-level; delete the parent tag
and confirm the project returns to the top level; add a dir to a plain tag and
confirm the folder icon, the agent pane, and the app-binding rejection all
follow.

Source-of-truth sweep (§9): every row marked **widen** in Appendix B is
dirs-based; a fresh `git grep 'project:'` / `is_project` / `project_path` turns
up no new reader still treating the prefix as project-ness; the docs in E are
updated; and the session keying in G is decided (tag id, or migrated on dir
change) rather than left implicit.

Expected commit prefix: `todo2:`.

---

## 10. Implementation status

Implemented, no schema change:

- **`libs/storage/src/tag.rs`**
  - `delete_tag` is transactional and removes the tag's implication rows in
    **both** directions plus its `tag_sections` and `tag_settings` rows, so a
    placed child falls back to the top level instead of becoming unreachable
    (§5.1.1).
  - `get_top_level_tags` / `get_children` now have a deterministic total order
    (label, then id).
  - `tag_tree` / `tag_tree_rows` (`TagTreeNode`, `TagTreeRow`): the whole
    hierarchy in three queries, fully expanded and already ordered
    (projects → plain tags → sections, then alphabetically), with a per-path
    visited guard and a `MAX_TREE_DEPTH` cap so a cycle that reached the table
    behind the API cannot hang a render.
  - `directory_backed_tag_ids` / `tag_is_directory_backed`: the one definition
    of "this tag is a project" (`project:` name **or** non-empty dirs).
- **`libs/storage/src/tag_settings.rs`** — `create_section` writes *both*
  representations (row, child tag, implication), `remove_section` removes both,
  `move_section` reorders. `add_tag_section` alone is now the wrong entry point
  for a caller that wants a usable section (§3.9).
- **`libs/storage/src/managed.rs`** — `attach_app_to_tag` refuses any
  directory-backed tag, not just `project:` ones (#25); `with_transaction` is
  **re-entrant**, so a nested frame joins the outer transaction and
  `delete_tag` can be transactional without `disable_app` →
  `release_app_from_tag` → `delete_tag` issuing a second `BEGIN` (§7.1.5 / #36).
- **`ui_parts/navbar.rs`** — bulk-loads `tag_tree_rows` (no `children_cache`,
  no per-tag fetch) and renders every level at once (#14 as revised, #31): no
  chevron, no count, no expand state. Folder icon for any directory-backed tag
  (#19). Selection and the row's element id are the row's **full path**, so the
  copies of a multi-parented project are distinct rows and only the clicked one
  highlights (§5.1.3). One-item row context menu: *Tag settings…* (#23).
- **`ui_parts/tag_settings.rs`** (new) — the popover: Placed under (chips with a
  hover cross and a confirm dialog), Add to a tag (filtered picker with the
  path as secondary text, #28), Directories, Sections, Apps (capture toggle +
  Detach). Writes immediately (#27); Esc and outside-click close it.
- **`ui_parts/task_list.rs`** — the gear beside the tag title (absent for All
  tasks).
- **`main.rs`** — the popover slot; `NavBarEvent::OpenTagSettings` and
  `TaskListEvent::OpenTagSettings` open/retarget the panel;
  `TagSettingsEvent::Changed` reloads the nav and the list;
  `sync_agent_project` seeds its candidate directories from `dirs`, falling back
  to the legacy name only when there are none (§9.C).
- **`store.rs`** — wrappers for the popover, and `project_dir` prefers
  `tag_settings.dirs` over the name.

Verified: `cargo check --workspace` clean (no warnings); `cargo test -p storage`
119 passed (9 new); `cargo test -p todo-2` 47 passed (3 new). Not yet run: the
app under a real window, so the manual walkthrough at the end of §8 is still a
checklist, not a completed step.

Deliberately not done:

1. **Persistence** (§5.1.12): the app's store is in-memory and re-seeded each
   launch, so a placement lasts one session. Nothing in this change can fix
   that; it needs the store to move to a file.
2. **Agent session keying** (§5.1.11): sessions are still keyed by the resolved
   directory, so editing dirs strands the previous session. The fix (key by
   `tag_id`, or migrate the entry on change) needs an `acp-sessions.json` format
   migration and was left out to keep this change reviewable.
3. **The picker is a list, not the details pane's input-with-chips editor.**
   Adding a parent is a filtered list of eligible tags; removing one is the
   chip's cross. Extracting the details-pane editor into a shared component is
   its own refactor.
4. **A tag that already holds a binding can still be given directories**
   (§5.1.8 / §3.10's ordering hazard): the new eligibility rule closes the
   attach direction only, so attaching first and adding a directory second still
   leaves an app managing a project directory.
5. **No render-level tests** (§8's correction): no `#[gpui::test]` harness exists
   in this workspace.

---

## 9. Appendix: directory-backed / `project:` call-site inventory

Inventory of every place that reads the `project:` prefix or otherwise decides
"this tag is a project", with the verdict the decisions imply. The rule today
is `Tag::is_project()` == `name.starts_with("project:")`; the rule this spec
wants is **directory-backed** == `is_project() || !dirs.is_empty()`.

Sections A/B are the readers the decisions change; C is the resolution order
they unblock; D/E/F are fixtures, docs, and a false positive; **G** is the one
cluster that is not a prefix reader at all — consumers of the *resolved*
directory, which the dirs picker reaches through the session key.

### A. The definition and its writers

| Location | What it does | Verdict |
|----------|--------------|---------|
| `libs/storage/src/tag.rs:33-37` | `Tag::is_project()` — the single definition of project-ness | Keep as the *name* test, and give the dirs-based predicate a home beside it so #19/#25 have one definition to call |
| `libs/storage/src/tag.rs:88-99` (`create_seed_project_tag`) | Builds `project:{path.display()}` (not canonicalized) for seed data | Unchanged; seeds keep the legacy shape |
| `libs/storage/src/tag.rs:120-137` (`get_or_create_project_tag`) | Builds `project:{canonicalized path}`, `display_name` = dir name; the only writer of new project tags | Unchanged (#24 keeps the name). Consider also seeding `tag_settings.dirs` with that same canonical path so name and dirs agree from birth |
| `libs/storage/src/tag_settings.rs:145,156,166` (`set_tag_dirs` / `add_tag_dir` / `remove_tag_dir`) | The dirs-list write path | The new panel's dirs picker is their **first app caller**; nothing in `apps/` calls them today |
| `apps/todo-2/src/projects.rs:56` (`Project::tag`) | Turns a scanned repo into a tag on pick | Unchanged (#29) |

### B. Readers that must widen to directory-backed (#19, #25)

| Location | What it does | Verdict |
|----------|--------------|---------|
| `libs/storage/src/managed.rs:544-553` (`attach_app_to_tag`, doc at `:537`) | Rejects `tag.is_project()` → "backs a local directory and cannot be managed by an app" | **Widen**, or a dirs-only tag becomes app-manageable while being a project everywhere else |
| `apps/todo-2/src/ui_parts/navbar.rs:220, 375` | `is_project: tag.is_project()` per row picks the folder icon | **Widen** (#19) |
| `apps/todo-2/src/ui_parts/apps.rs:313-318` (`attach_blocker`) | Grays project tags out in the attach picker with "Backs a local folder" | **Widen**; otherwise the picker offers a tag the store then rejects — §5.2's mismatch in reverse |

### C. Already dirs-aware, but still seeding candidates from the name (#24)

| Location | What it does | Verdict |
|----------|--------------|---------|
| `apps/todo-2/src/main.rs:802-809` (`sync_agent_project`) | Pushes the `project:`-decoded path into `candidates` **unconditionally**, then extends with `dirs`; `directory_backed = tag.is_project() \|\| !dirs.is_empty()`; gates `agent_available` | Availability is already dirs-based, but the name branch must stop feeding a directory the user removed — `AgentProject::resolve` only drops candidates that do not exist, so a still-present removed dir is offered. This is the site with real user-visible damage |
| `apps/todo-2/src/store.rs:1355-1385` (`project_dir`) | Walks the ancestor chain: first `strip_prefix("project:")` + `path.is_dir()`, then `tag_settings.dirs` + `path.is_dir()` | Both branches exist, but the name branch is **preferred order**, so a stale name outranks a configured dir |
| `apps/todo-2/src/store.rs:1072-1085` (`coding_directory`) | Doc: "carries a `project:` tag, or a tag with a configured directory"; delegates to `project_dir` | Comment adopts the dirs-based phrasing; behavior follows `project_dir` |
| `apps/todo-2/src/ui_parts/agent_pane.rs:78-95` (`AgentProject::candidates`, `resolve`) | Doc: "the path encoded in the tag name first, then `tag_settings.dirs`" | Decide whether name-first ordering survives once dirs are the source of truth |
| `apps/todo-2/src/ui_parts/task_details.rs:449-452, 717` | `coding_directory_backed` gates the coding workflow and auto-start | No direct prefix test — follows `coding_directory`; keep in sync |

### D. Seeds, tests, migrations (no behavior change expected)

| Location | What it does | Verdict |
|----------|--------------|---------|
| `libs/storage/src/testing.rs:508, 558-575` | Seeds `project:/Users/me/src/me/accounting` and `.../about-me` | Keep as the legacy-shape fixture |
| `libs/storage/src/tag.rs:778-900` | `test_get_or_create_project_tag` (name shape, idempotence, two repos both named `api`), `get_tag_by_name("project:/Users/me/dev/about-me")`, `assert!(... .is_project())` | Extend with dirs-backed cases |
| `libs/storage/src/tag_settings.rs:438-455` | `test_tag_dirs_add_remove` | Extend for the panel's replace-whole-list path |
| `libs/storage/src/managed.rs:1536` | Uses `get_or_create_project_tag("/tmp/somewhere/api")` to assert attach rejection | Add the non-`project:` tag with dirs as the #25 regression |
| `libs/storage/toasty/migrations/0020_seed_ownership.sql:22` | `WHERE is_seed = 1 AND name NOT LIKE 'project:%'` | Historical one-shot; no change — but it shows the rule already leaked into a migration |

### E. Docs that assert the old prefix rule (drift to fix when #19/#25 land)

- `docs/spec/todo2-managed-apps-spec.md:64` (#15), `:172` (§3.2), `:398` (§6.1), `:521`, `:542`
- `docs/spec/acp-client-and-agent-panel-spec.md:166`, `:178`, `:312`
- `docs/spec/todo2-agent-pane-ui-spec.md:266`, `:459`

### F. Not a call site

- `demos/symphony/src/tracker.rs:741` — a Linear GraphQL `project: { slugId: … }`
  filter, unrelated to tags.

### G. Derived: consumers of the *resolved* directory

Not prefix readers, but the same resolution feeds them, so a change to how a
project's directory is determined (decision #24, §9.C) reaches them too.

| Location | What it does | Verdict |
|----------|--------------|---------|
| `libs/acp-client/src/session_store.rs:1-6, 18-28, 79-126` | `StoredSession` keyed by `project_path` ("canonical project directory; the key"); header claims the path "survives tag renames"; `tag_id` present but "for display only"; `get`/`put`/`remove` all take the path | **Affected** (§5.1.11): the key is the resolved candidate, so editing dirs strands the session. Key by `tag_id`, or migrate on dir change |
| `apps/todo-2/src/ui_parts/agent_pane.rs:544-560, 620-628` | Sets `stored_path` from the resolved `cwd`; `store.get(&project_path)` to decide resume; writes `StoredSession { project_path, tag_id, … }` | Same; also the one place that already has the tag id in hand when the key is written |
| `libs/acp-client/src/acp_client.rs:138-161` | `SessionSpec`/`SessionRoots` — "the directories a session is scoped to, in the order the project lists them"; filters non-existent roots | Candidate **order** is semantic (`cwd` vs additional roots); dirs reordering is a behavior change, not a UI nicety |
| `apps/todo-2/src/ui_parts/agent_pane.rs:78-95` | `AgentProject::candidates` order: name-encoded path first, then dirs | Listed in §9.C; here because `resolve()`'s winner becomes the session key |

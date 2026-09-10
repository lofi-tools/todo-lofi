# Todoist Integration Spec

Status: Draft v1 (gathered from interview, no implementation yet)

Purpose: Define how Todoist concepts map onto the todo-lofi data model and how a
bidirectional sync between Todoist and the app behaves.

## 1. Intent

Add Todoist support to the todo app: the user connects their Todoist account
(OAuth), picks which Todoist projects to sync, and the two sides stay in sync.
This document is primarily a **concept mapping** — which Todoist concept becomes
which local concept — plus the sync semantics that make the mapping safe to run
repeatedly and in both directions.

## 2. Direction, surface, and auth (scope)

- **Direction:** bidirectional sync. Both sides can edit the same tasks and
  changes flow both ways.
- **Surface:** mixed.
  - Mapping and sync logic live in the shared storage library (`libs/storage`),
    so every app (todo-2, desktop-gpui, agent-cli, …) can use it.
  - A thin UI entry point lives in the `todo-2` app: connect/disconnect,
  - sync status, and project selection.
- **Auth:** OAuth flow (Todoist OAuth 2.0, `data:read_write` scope). Token
  storage/refresh details are left open (see §8).
- **Sync trigger:** periodic background sync while the app runs, plus an
  explicit "Sync now" action in the UI. Cadence/backoff left open (see §8).

## 3. Concept mapping

### 3.1 Todoist → local (import)

| Todoist concept | Local concept | Notes |
| --- | --- | --- |
| Project | Top-level tag | Each selected project becomes a tag at the top level of the local tag hierarchy |
| Section | Child tag | A section becomes a child tag under its project's tag |
| Label | Flat tag | Imported as-is, same name; **not** wired into implication hierarchies |
| Priority P1–P4 | `urgency_factor` | Reversed scale, see §3.3 |
| Due date (one-shot) | `deadline` (epoch UTC) | Original timezone stored as metadata, see §3.6 |
| Recurring due | Repeat template + recurrence rule | Extends the existing `repeat_task_templates` table, see §3.4 |
| Subtasks (up to 5 levels) | `parent_id` chain | Arbitrary depth is storable; UI limits, see §3.5 |
| Comments | New JSONB column on task | `text` + `author` + `created_at`; **one-way** (Todoist → app only), see §3.7 |
| Completed tasks | `done` + `completed_at` | Both ways, see §3.8 |
| Reminders / assignees / filters | — (dropped) | Never ported; never deleted from the Todoist side |

### 3.2 Local → Todoist (export)

| Local concept | Todoist concept | Notes |
| --- | --- | --- |
| Title | Task content | |
| Description | Task description | |
| `deadline` | Due date | Plus recurrence when a repeat template exists |
| Tags (leaf) | Labels | Only exported as labels; implications are local-only |
| Project/section tags | Project / section | Reconstructed from the tag hierarchy created at import |
| `parent_id` | Subtask parent | All stored levels round-trip |
| `done` / `completed_at` | Completed / reopened | |
| `urgency_factor` | Priority P1–P4 | Inverse of §3.3 |
| **Marker label** | "synced from" label | Added to Todoist tasks that came from the local app so the sync can recognize its own tasks; name TBD (see §8) |
| `blocked_by` deps, `importance_factor`, `branch_name`, `source_task_id` (follow-ups), `blocked_until` | — (local-only) | Never exported; see §3.9 |

### 3.3 Priority ↔ urgency mapping

Todoist priority is 1 (highest, red flag) → 4 (lowest). Maps onto
`urgency_factor` in **reverse**:

| Todoist | Local `urgency_factor` |
| --- | --- |
| P1 | 2.0 |
| P2 | 1.5 |
| P3 | 1.25 |
| P4 | 0.5 |

Export is the inverse table. `importance_factor` is untouched by priority.

### 3.4 Recurrence

- Goal: a full recurrence engine — Todoist recurrence syntax (e.g. `every day`,
  `every week on Mon`, `every 3rd of the month`, strict `every!` variants,
  timezone-aware) is represented and evaluated natively.
- Storage: **extend the existing `repeat_task_templates` table** with:
  - existing `interval_days` and `time_of_day`,
  - weekdays (for "every week on Mon/Tue …"),
  - month day (for "every Nth of the month", including last-day),
  - a strict-flag for `every!` (no skipping of missed dates),
  - the original timezone.
- The evaluator computes the next occurrence from a given instant **in the
  task's stored timezone** (see §3.6).
- Legacy templates (interval_days only) remain valid; new fields default to
  "no rule" so the old behavior is unchanged.

### 3.5 Nesting depth

- The data model stores **all 5 Todoist levels** (local `parent_id` chains
  already support arbitrary depth).
- UI behavior:
  - The task-list view shows **one level of nesting** (direct subtasks collapse
    under their parent, as today; deeper levels are not expanded inline).
  - The task-details view shows a task's **direct subtasks**; drilling into a
    subtask shows its own direct subtasks, and so on recursively.
- No flattening or truncation of the stored tree.

### 3.6 Timezones

- `deadline` is stored as **UTC epoch** (matches the current `u64` deadline).
- The **original Todoist timezone is kept as metadata** (new column on the
  task) so recurrence computation and display can use it.
- Without an explicit timezone on a task, fall back to the account timezone.

### 3.7 Comments

- Stored in a **new JSONB column on the task** (e.g. `comments:
  Json<Vec<Comment>>`), not appended to the description.
- Each comment: `text`, `author`, `created_at` (timestamp), plus the Todoist
  comment id for idempotent re-import.
- **One-way:** comments are imported from Todoist; the app never writes
  comments back.

### 3.8 Completed tasks

- `done` and `completed_at` sync **both ways**.
- Completing on either side marks the task done here (with completion
  timestamp); reopening on either side clears it.

### 3.9 Unmapped local concepts

`blocked_by` dependencies, `importance_factor`, `branch_name`,
`source_task_id` (follow-ups), and `blocked_until` stay **local-only**. They are
never serialized to Todoist and never stripped from local tasks during sync.
The only Todoist-side trace of a locally-synced task is the "synced from"
marker label (§3.2).

## 4. Sync semantics

### 4.1 Identity — generic external links (multiple integrations)

- The app supports **multiple integrations** (Todoist today, others later),
  so identity is not a single `external_id` column. It is a generic
  many-to-many mapping:
  - `integrations` table: one row per connected account
    (`id`, `provider` e.g. `"todoist"`, `account_label`, OAuth
    token/refresh storage location, `created_at`). The `id` is the
    **integration id**; every link below is scoped to it.
  - `external_task_links` table: `(integration_id, external_id, task_id,
    external_updated_at)` with a unique constraint on
    `(integration_id, external_id)`. One local task may link to **several**
    external tasks (one per integration), and re-syncs update in place
    instead of duplicating.
  - `external_tag_links` table: `(integration_id, external_id, tag_id,
    source_kind, namespaced)` with a unique constraint on
    `(integration_id, external_id)`. Projects/sections/labels keep their
    remote ids so tag creation is idempotent.
- Tag mapping is tracked explicitly (a tag created from a Todoist
  project/section/label remembers its `(integration_id, source id, kind)`),
  which also makes the namespaced-collision rule (§4.5) deterministic per
  integration.
- Per-field merge (§4.2) compares the link's `external_updated_at` against
  the local `updated_at`; the timestamp lives on the **link row**, not on
  the task, so two integrations never overwrite each other's merge state.

### 4.2 Conflict resolution — per-field merge

- The task is merged **field by field**.
- For each mapped field, the side whose field-level update timestamp is newer
  wins. Ties default to the Todoist value (documented assumption, see §8).
- Fields are compared per Todoist's updated timestamps (task `updated` /
  field-level `date_updated` where available) and the local `updated_at`.

### 4.3 Deletions

- **Mirror deletions both ways.**
- Local side: deleting a task only **tombstones** it (`deleted_at` column); the
  row stays in the DB but is hidden from the UI. Tombstones propagate to
  Todoist (`task:delete`).
- Deleting in Todoist tombstones the local row on the next sync.
- "Don't delete source data" applies to **unmapped fields on the Todoist side**
  (§4.4): we never remove data we don't handle.

### 4.4 Partial updates on Todoist

- The app sends **partial updates only** — `PATCH`/sync commands touching only
  the fields we map (content, description, due/recurrence, priority, labels,
  project/section, parent, completed state).
- Assignees, reminders, and other Todoist data we don't model are **never
  touched** — no sending, no clearing.

### 4.5 Tag collisions

- When an imported Todoist project/section/label name collides with an existing
  local tag, the sync creates a **namespaced copy** (e.g. `todoist/<name>`)
  instead of merging into or overwriting the local tag.
- The mapping table records the Todoist id → namespaced tag id so the sync
  keeps using the same copy across runs.

### 4.6 Scope

- The user **selects which Todoist projects sync** in the UI (multi-select).
- Inbox is a selectable project like any other.
- Unselected projects are not imported, and local data belonging to them is not
  touched.

### 4.7 Sync algorithm (high level)

1. Pull: fetch changes since the last successful sync (incremental Sync API or
   equivalent), for the selected projects only.
2. Apply per-field merge to existing tasks; create new tasks/tags for unseen
   external ids; tombstone tasks deleted on the Todoist side.
3. Push: collect locally-changed synced tasks (and tombstones), send partial
   updates to Todoist.
4. Record a sync watermark (per project) after success. Failure leaves the
   watermark untouched so the next run retries.

## 5. Data model changes (draft)

- `tasks` (generic, provider-agnostic):
  - `deleted_at` (tombstone, nullable; UI hides rows with `deleted_at` set)
  - `timezone` (original remote timezone string, nullable metadata)
  - `comments` (`Json<Vec<Comment>>`, nullable) — one-way imported comments
- `integrations` (new table, one row per connected account):
  - `id` (autoincrement PK — the **integration id**)
  - `provider` (e.g. `"todoist"`; more providers later)
  - `account_label` (human label, e.g. account name)
  - `created_at`
- `external_task_links` (new table, the multi-integration identity map):
  - `integration_id` → `integrations.id`
  - `external_id` (remote task id, opaque string)
  - `task_id` (local task id)
  - `external_updated_at` (remote-side updated timestamp, nullable)
  - unique on `(integration_id, external_id)`
- `external_tag_links` (new table):
  - `integration_id`, `external_id` (remote project/section/label id)
  - `tag_id` (local tag id)
  - `source_kind` (project / section / label)
  - `namespaced` flag for collision copies (§4.5)
  - unique on `(integration_id, external_id)`
- `tag_settings` (new table, one row per configured tag):
  - `tag_id`, optional sync target (`sync_integration_id` +
    `sync_external_id` — the remote project/section the tag syncs with)
  - `dirs` (JSON list of local directories backing a dir-project tag;
    a project may span several dirs)
  - new settings arrive as nullable columns; absent rows mean defaults
- `tag_sections` (new table):
  - `id`, `tag_id`, `name`, `position`; unique on `(tag_id, name)`
  - named subdivisions living inside one tag (Todoist sections import
    as sections of their project tag, see §3.1)
- `repeat_task_templates`:
  - weekday rule, month-day rule, strict flag, timezone (see §3.4)
- A sync-state table:
  - per `(integration_id, project_id)`: last successful sync watermark
  - OAuth token + refresh token storage location (see §8)

## 6. todo-2 UI (thin)

- Connect/disconnect Todoist (OAuth).
- "Sync now" button plus a subtle "last synced" indicator.
- Project selection dialog (multi-select of Todoist projects).
- Task details already shows direct subtasks; nested subtask navigation is
  recursive (see §3.5).

## 7. Non-goals / out of scope

- No webhooks/live push in the initial version (periodic background sync only).
- No export of `blocked_by`, `importance_factor`, `branch_name`, follow-ups, or
  `blocked_until` (§3.9).
- No comments written back to Todoist (§3.7).
- No import of reminders, assignees, or filters.
- No merging into existing local tags on collision (namespaced copies instead).
- No schema migration tooling specified here (existing DBs need migrations for
  the new columns; migration mechanics out of scope for this spec).

## 8. Open questions for implementation

- OAuth token storage (keychain vs config file) and refresh handling.
- Background sync cadence, rate-limit handling (Todoist REST ~450 req/min,
  Sync API limits), and backoff.
- Exact "synced from" marker label name and whether it is user-configurable.
- Tie-breaking default on equal per-field timestamps (currently: Todoist wins).
- Whether project selection should be editable per sync or only at connect time.
- Comment schema details: author as string vs id, timestamp precision, ordering,
  and whether Todoist comment edits (not just adds) are detected.
- Exact recurrence grammar coverage for v1 (which Todoist recurrence forms must
  be supported before "full engine" is claimed).
- Per-field merge implementation: whether Todoist's field-level `date_updated`
  is available for all fields or the task-level `updated` must be used as a proxy.
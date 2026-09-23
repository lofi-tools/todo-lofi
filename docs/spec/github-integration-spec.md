# GitHub Integration (issues/projects sync + git-backed coding workflow) — Spec

## 1. Intent

Two capabilities, deliberately specified together because the second depends on the first:

- **Sync**: a two-way link between GitHub issues and local tasks, following the
  established Todoist integration shape (`integrations` + `external_*_links`),
  with GitHub repos bound to project tags. A synced task is therefore
  *issue-backed*: the app knows its `owner/repo` and its issue number.
- **Git-backed coding workflow**: the existing `coding-task` run
  (interview → spec → implement → review → merge) stops operating inside the
  user's own checkout. Runs get **worktrees**, branches named from the issue,
  and the merge step becomes a **PR step** that pushes the branch, opens a draft
  PR for every repo involved, and completes the run when the PR is merged.

**Decision (round 1): both halves ship in one spec, and the sync half lands
first.** The second half's branch naming and PR body need an issue to name and
describe, so building it first would mean building it against a stub.

## 2. Interview decisions (normative)

| # | Round | Decision |
| --- | --- | --- |
| 1 | 1 | Both halves in one spec; **sync implemented first**, workflow/PR half second |
| 2 | 1 | Auth is **GitHub's device flow** (app ships a public client id; no client secret) |
| 3 | 1 | Sync is **two-way issues → tasks with per-field merge** |
| 4 | 1 | Mapping is **repo → project tag, issue → task, labels → tags**; milestones and assignees are imported as metadata only |
| 5 | 2 | Repo binding: **reuse `tag_settings.sync_target`, plus auto-detect from the project dir's remotes** and persist the detection for override (refined by decision 33) |
| 6 | 2 | Issue scope: **all open issues in the bound repo** (linked-but-closed issues keep syncing) |
| 7 | 2 | Conflicts: **per-field timestamps, GitHub wins ties** |
| 8 | 2, 5 | Close/delete: **closed issue → task complete**, never a tombstone; a **deleted/transferred/inaccessible issue tombstones** the task; **local delete closes the issue** |
| 9 | 3 | Worktrees live in **`<repo>/worktrees/<slug>` inside the repo** |
| 10 | 3 | Lifecycle: **one worktree per run; created at spec approval, removed at merge** |
| 11 | 3 | The agent pane **switches to the worktree** for an active run |
| 12 | 3 | **One shared build cache** across worktrees (`CARGO_TARGET_DIR`) |
| 13 | 4 | Branch name for an issue-backed run: **`feature/<issue-number>-<slug>`** |
| 14 | 4 | The **merge step is replaced by a PR step** (local merge survives only as the no-remote fallback — decision 17) |
| 15 | 4 | Opening the PR **pushes the branch first, opens it as a draft, generates title/body from the spec + `propose_summary`, and copies labels/assignee/reviewer from the issue** |
| 16 | 4 | Slug: **strip stopwords and truncate to a few words** |
| 17 | 5 | No remote, or a non-GitHub remote ⇒ the step **keeps today's local merge** as its action |
| 18 | 5 | `worktrees/` is excluded via **`.git/info/exclude`** (never the tracked `.gitignore`) |
| 19 | 5 | **Poll and auto-complete the run when the PR is merged** |
| 20 | 6 | A dirty worktree at PR time **refuses, with a "commit and continue" button** |
| 21 | 6 | **One worktree per directory involved in the run** (a multi-repo project gets a branch and PR per repo) |
| 22 | 6 | Failures: **inline reason, auto-retry transient (network/rate limit), block on permanent** |
| 23 | 6 | Polling is **demand-driven**: frequent while something is pending, idle otherwise |
| 24 | 6 | The recipe **keeps the `merge` node**; only its action changes (no `coding-task` v2) |
| 25 | 7 | Multi-repo: **one PR per repo, all shown as sub-items of the step**; the run completes when all are merged (or the user waives the rest) |
| 26 | 7 | Fields: **issue body → description** (two-way); **comments imported one-way** into `tasks.comments` |
| 27 | 7 | Labels: **create a label for a new local tag; never delete or rename labels** |
| 28 | 7 | Cleanup after merge: **remove the worktree, keep the local branch** for the existing "Branches to clean up" list |
| 29 | 7 | Verification: **storage + GPUI unit tests against fakes, no live network** |
| 30 | 8 | Push auth: **rely on the user's existing git credentials** (SSH/keychain); a failed push blocks with git's stderr |
| 31 | 8 | **One GitHub.com account** (a single `integrations` row, provider `github`) |
| 32 | 8 | First import: **import everything, quietly** (paged, rate-limit aware, background) |
| 33 | 9 | Remote resolution is **never hardcoded to `origin`**: `remote.pushDefault` → the branch's `branch.<name>.remote` → `origin` → `github` → the first remote whose URL is on github.com. Persisted per worktree |
| 34 | 9 | The OAuth app is registered **under the `lofi-tools` org** with device flow enabled (the org that hosts this repo); the client id ships in the binary and `GITHUB_CLIENT_ID` overrides it for forks/self-builds |
| 35 | 9 | **No local-merge escape hatch** once a remote resolves to github.com — the PR step is absolute there. Local merge survives only where no such remote exists (decision 17) |
| 36 | 9 | Generated PRs follow the **`AGENTS.md` PR-hygiene convention**: imperative title, no conventional-commit prefix, no trailing punctuation, and a final `Release Notes:` section |
| 37 | 10 | Sub-issues ↔ subtasks are **synced both ways**: a local subtask in a repo-bound tree opens its own issue and is attached under its parent's issue (the parent's chain is opened first when none of it is on GitHub yet), and a **pulled sub-issue becomes a local subtask**, un-nested when the relationship is removed on GitHub. The relationship is tracked in the child's link, one repo's issues only |

## 3. Current state (verified in this repo)

### 3.1 The coding workflow already exists

- `docs/spec/coding-workflow-runs-spec.md` defines and reports as implemented:
  phases `interview` / `spec` / `implement` / `review` / `merge`, materialized as
  subtasks of the run's root feature task; `RecipeNode.phase|subtask|
  retrigger_node`; `@notes`; one-run-per-project with nested runs exempt.
- Its §16 states plainly: *"Remote PR creation and any GitHub integration (local
  merge only)"* is out of scope. **This spec is that follow-up.**
- `apps/todo-2/src/coding_git.rs` (193 lines) shells out to `git` with an
  explicit `current_dir` and captures stderr: `is_repo`, `current_branch`,
  `changed_paths`, `branch_exists`, `create_branch`, `switch_branch`,
  `merge_no_ff`, `abort_merge`, `delete_branch` (+ 4 tests against temp repos).
- `apps/todo-2/src/store.rs` orchestrates: `start_coding_run`,
  `coding_run_for_task`, `save_coding_spec`, `approve_coding_spec` (cuts the
  branch), `merge_coding_branch`, `cancel_coding_run`,
  `list_branch_cleanup_runs`, `delete_coding_branch`, plus `project_dir`
  (ancestor tag → `tag_settings.dirs`), `create_feature_branch`,
  `merge_into_base`, `proposed_branch`, `default_branch_name`
  (`feature/<run-id>-<title-slug>`) and `normalize_branch_name`.
- The MCP tool surface (`coding_mcp.rs`) already has `propose_branch` and
  `propose_summary` (`task_id`, `commit_message`, `pr_summary?`) — the PR body's
  inputs exist today.
- `tasks.branch_name` exists on the task row.

**Consequence for this spec:** today the app cuts the feature branch *in the
user's own checkout* (`current_dir` = project dir) and the agent pane runs its
session with that same cwd.

### 3.2 The sync substrate already exists

- `libs/storage/src/external.rs`: `integrations` (`id`, `provider`,
  `account_label`, `created_at`, `app_id`), `external_task_links`
  (`integration_id`, `external_id`, `task_id`, `external_updated_at`, unique on
  `(integration_id, external_id)`), `external_tag_links`
  (`integration_id`, `external_id`, `tag_id`, `source_kind`, `namespaced`).
- `TagSettings { tag_id, sync_target: Option<SyncTarget>, dirs: Vec<String> }`
  (`libs/storage/src/tag_settings.rs`), persisted as
  `tag_settings.sync_integration_id` / `sync_external_id` / `dirs`.
- `Task` already carries `deleted_at` (tombstone), `timezone`,
  `comments: Option<Json<Vec<ExternalComment>>>` (one-way imported comments),
  `branch_name`, `workflow_run_id`.
- `docs/spec/todoist-sync-spec.md` §4.1 already states identity is a generic
  many-to-many map precisely so *"the app supports multiple integrations
  (Todoist today, others later)"* — GitHub is that "later".
- `apps/todo-2/src/todoist_auth.rs`: PKCE OAuth, token file
  `~/.config/my-todo/todoist.json` (0600), `MY_TODO_CONFIG_DIR` override,
  `TODOIST_CLIENT_ID` env override, `access_token()` refresh logic,
  `list_projects()`. The GitHub client should mirror this file's shape.
- `apps/todo-2/src/ui_parts/integrations.rs` renders the Todoist card and a
  **GitHub card that is a "Coming soon" stub** ("Turn issues and PRs into tasks").
- Migrations currently end at `0022_drop_managed_by_recipe_id.sql`, so this spec
  takes `0023_*`.

### 3.3 The agent pane is cwd-keyed

`apps/todo-2/src/ui_parts/agent_pane.rs` resolves a project to
`AgentProject::resolve() -> (cwd, additional roots)` and stores sessions in a
`SessionStore` keyed by `cwd.display().to_string()` (line 525), with
`SessionRoots` built from cwd + additional roots (line 565). **Pointing a run at
a worktree therefore changes the session key: the worktree-backed session is a
new ACP session, not a resumed interview session.**

## 4. Can worktrees live inside `.git`? (answered, with evidence)

Asked directly in the request. Tested against **git 2.54.0** on a scratch repo:

| Command | Result |
| --- | --- |
| `git worktree add .git/wt1 -b feature/wt1 main` | **Works.** Working tree at `.git/wt1`; git's admin dir separately at `.git/worktrees/wt1`; `git worktree list` shows both. |
| `git worktree add .git/worktrees/wt3 -b feature/wt3 main` | **Works, but is a footgun.** The checkout lands *inside* git's own admin directory — that directory then contains `HEAD`, `ORIG_HEAD`, `commondir` alongside source files. `rm -rf .git/worktrees` would destroy git metadata. Never do this. |
| `git status --porcelain` in the main tree | Empty — worktrees under `.git` are invisible, no `.gitignore` needed. |
| `git clean -fdxn` in the main tree | Reports nothing — `.git` is never swept. |
| `rm -rf` the checkout, then `git worktree list` | Entry shows as `prunable`; `git worktree prune -v` removes the stale admin dir. |
| Branch after the checkout was deleted | **Still exists** (`git branch --list` lists it) — deleting a worktree does not delete its branch, so cleanup must delete branches explicitly. |
| Disk | `.git` grew only 144K for an empty repo (admin metadata); the real cost is a full second checkout plus its build dir. |

So: **yes, `.git` works, and it is safe as long as the path is not
`.git/worktrees/<name>`.** It buys invisibility (no `.gitignore`, no `git
status` noise, no risk of committing a checkout) and costs discoverability
(Finder/editors hide dotdirs, and a user who deletes `.git` deletes every
worktree).

**Decision 9 + 18 override the `.git` option anyway:** worktrees go in
`<repo>/worktrees/<slug>`, made invisible to git by appending `worktrees/` to
`$GIT_COMMON_DIR/info/exclude` — discoverable when you want them, never in
`git status`, and never a tracked-file edit (which matters because the existing
branch-creation guard refuses a dirty tree; see §6.2).

## 5. Part A — GitHub issues ↔ tasks sync

### 5.1 Auth — device flow

- New `apps/todo-2/src/github_auth.rs`, mirroring `todoist_auth.rs`:
  - `POST https://github.com/login/device/code` with `client_id` + `scope`;
    returns `device_code`, `user_code`, `verification_uri`, `expires_in` (900s),
    `interval` (5s).
  - Poll `POST https://github.com/login/oauth/access_token` with
    `grant_type=urn:ietf:params:oauth:grant-type:device_code` until
    `access_token`, handling `authorization_pending`, `slow_down`,
    `expired_token`, `access_denied`.
  - Scopes: `repo` (private repos, writing issues/labels/PRs) and `read:user`
    (account label). Documented as the minimum that makes the feature work.
- Storage: `~/.config/my-todo/github.json`, 0600, holding `client_id`,
  `access_token`, `scope`, `login`, `created_at`; honouring
  `MY_TODO_CONFIG_DIR`; `GITHUB_CLIENT_ID` env override for development
  (mirrors `TODOIST_CLIENT_ID`).
- **The token is API-only.** Pushing uses the user's own git credentials
  (decision 30): no token is ever written into `.git/config`, a remote URL, or a
  credential helper.
- **Alternate credential — personal access token.** The card offers an optional
  PAT that authenticates every API call instead of the device-flow token, for an
  org whose owners never approved the OAuth app; it is stored in the same file
  (`personal_token`) and wins over the OAuth pair. The card says why a token is
  the easier route (it works on organization repos immediately, and is revocable
  on GitHub at any time) and lists the steps to create one, each with a button
  opening the page it is about; the classic-token form is prefilled with the
  `repo` scope and no expiration, so a saved token does not start failing
  silently after GitHub's 30-day default.
- The shipped client id is public by design (device flow takes no secret). The
  app is registered **under the `lofi-tools` org** — the org that already hosts
  this repo — with device flow enabled and the scopes above (decision 34);
  `GITHUB_CLIENT_ID` lets a fork or self-build point at its own app. The one
  remaining maintainer action is creating that app and justifying the scopes to
  it (§10.1).

### 5.2 Object mapping

| GitHub | Local | Direction |
| --- | --- | --- |
| Repository (`owner/repo`) | Project tag, namespaced `github/<owner>/<repo>`, with `tag_settings.sync_target` set | binding |
| Issue | Task (title, body → `description`) | two-way |
| Issue state (`open`/`closed`) | `done` / `completed_at` | two-way |
| Labels | Tags, nested under the issue's project tag | two-way, additive (decision 27) |
| Comments | `tasks.comments` (one-way import) | GitHub → local |
| Assignees, milestone, author, `html_url` | Metadata chips (stored in the link's `field_state`, rendered read-only) | GitHub → local |
| Sub-issue relationship | `tasks.parent_id` (the child's link also records the parent issue) | two-way, GitHub owns the relationship (§5.9) |
| PRs, Projects v2 boards, milestones-as-sections | — | **out of scope** (§9) |

Namespacing reuses the Todoist collision rule: an imported repo tag never merges
into an existing local tag of the same name; it is created (and thereafter
found) under its namespaced name.

### 5.3 Repo binding

1. A project tag **may** carry an explicit sync target
   (`tag_settings.sync_integration_id` + `sync_external_id = "owner/repo"`),
   chosen in the tag settings popover next to the dirs picker.
2. When it does not, the app **auto-detects** on first sync — and it must look at
   *all* remotes, not `origin` (decision 33). This repo is the proof: its remotes
   are `github` and `gitlab`, and **there is no `origin` at all**, with
   `remote.pushDefault = github`. `resolve_remote(dir)` therefore picks, in
   order:
   1. `remote.pushDefault`, when its URL is on github.com;
   2. the current branch's `branch.<name>.remote`, when its URL is on github.com;
   3. a remote named `origin`;
   4. a remote named `github`;
   5. the first remote whose URL parses as
      `github.com[:/]<owner>/<repo>(\.git)?`.

   The result is **persisted** to the sync target (and to `run_worktrees.remote`,
   §6.5) so it is stable and overridable. Other hosts are never used.
3. The detected/selected repo must be visible to the connected account; a 404
   leaves the tag unbound with an inline reason rather than an error toast.
4. A tag with neither a target nor a detectable remote is simply not synced
   (no error). Today's `project_dir` resolution (ancestor tag → `dirs[0]`) is
   unchanged for the coding workflow.

### 5.4 Identity and change detection

Because one integration spans many repos, the external id must be
repo-qualified:

```
external_id = "<owner>/<repo>#<issue-number>"
external_task_links.external_updated_at = issue.updated_at
```

The unique constraint on `(integration_id, external_id)` still holds.

GitHub has **no per-field timestamps**, so "per-field timestamps with GitHub as
tie-breaker" (decision 7) is implemented against a stored snapshot:

- New nullable JSON column `external_task_links.field_state`:
  ```json
  {
    "remote": { "title": "...", "body": "...", "state": "open",
                "labels": ["bug"], "assignees": ["me"], "milestone": "v1" },
    "local_changed_at": { "title": 1757600000, "body": 1757600100 }
  }
  ```
- Local writes to a synced field stamp `local_changed_at[field]` (in the store's
  update paths, not via DB triggers).
- On pull, a field whose remote value differs from `remote[field]` counts as
  remotely changed, timestamped `issue.updated_at`.
- Resolution: both changed → newer wins, tie → GitHub (decision 7); one side
  changed → that side; neither → no-op.
- After a successful push the snapshot and `external_updated_at` are refreshed,
  so the app's own writes never look like remote edits.
- **Known limitation, stated rather than hidden:** GitHub's `updated_at` moves
  on comments and label changes too, so an unrelated comment can make a remote
  edit look newer than a local one. The snapshot comparison keeps this rare
  (only fields that actually differ are considered).
- Fields that are never pushed (assignees, milestone) cannot conflict.

### 5.5 Push rules

- **A new local task in a repo-bound project opens an issue.** Capture is the
  GitHub counterpart of Todoist's new-item push: the task's title becomes the
  issue `title`, its description the `body`, and the issue is opened. The
  returned issue is stored as the link's snapshot, so the first pull merges it
  rather than importing a second task. A task outside a repo-bound project, an
  already-issue-backed task, a workflow step, and a builtin/demo task all stay
  local; no GitHub connection means no work at all.
- Pushed: `title`, `body`, `state` (open/closed), labels.
- Not pushed: assignees, milestone, comment threads.
- **Additive labels** (decision 27): adding a local tag creates the label if
  missing (`POST /repos/{owner}/{repo}/labels`) and adds it to the issue —
  including when the issue is opened, so a task captured with a project subtag
  shows its label at once; the app never deletes or renames label objects, and
  never removes a label it did not add. (Whether the app may *remove* a label it
  previously added when the tag is removed locally is an open item — §10.)
- **Labels are the project's subtags** (both directions). A remote label is
  imported as a tag under the issue's project tag (the user's own tag is reused
  when one is named after the label, otherwise it is created as the per-repo
  `github/<owner>/<repo>/<label>`, displayed as the label, so two projects never
  share one label tag), and a local tag under a repo-bound project is pushed as
  a label. The project view's descendant aggregation then holds every issue
  carrying one of the project's labels.
- **Subtasks** are mirrored too: a subtask opens its own issue and is attached
  under its parent's issue, opening the parent's chain first (§5.9).
- Workflow-step tasks (`workflow_run_id` set) never sync, matching today's rule
  — and neither does anything under one, since run content is the app's.
- All calls are `PATCH`/`POST` on the specific fields only — no full-object
  round trips that could clobber fields we do not model.

### 5.6 Close, delete, reopen

| Event | Effect |
| --- | --- |
| Local task completed | `PATCH state=closed` and a comment saying the linked task was marked as completed — GitHub gives a plain close no reason of its own, so the thread is what distinguishes "done" from "abandoned" |
| Issue closed on GitHub | Local task completed (never tombstoned) |
| Issued reopened | Local task reopened (unless tombstoned) |
| Local task deleted | Tombstone locally **and** close the issue (GitHub has no delete); the link is marked tombstoned so later pulls cannot resurrect it |
| Issue deleted / transferred / access revoked | Tombstone the local task, keep the row, record the reason (`404` vs transfer redirect) so the UI can explain it |

### 5.7 Polling, limits, first import

- **Demand-driven** (decision 23): a short interval while a sync is in flight or
  the queue is non-empty, back-off to idle otherwise; a manual **Sync now** in
  the integration card.
- Conditional requests: `If-None-Match`/ETag per repo plus `since=<last_synced_at>`
  for issue lists; both stored in a new `integration_sync_state` row.
- Rate limits: honour `x-ratelimit-remaining`/`x-ratelimit-reset` and
  `retry-after`; 403 secondary-limit responses back off exponentially.
- **Transient** failures (network, 5xx, rate limit) auto-retry with backoff and
  only surface if they persist; **permanent** ones (401, 404 repo, 422
  validation) block with an inline reason and a fix action (decision 22).
- First connect: **import everything, quietly** (decision 32) — paged at 100/page
  in the background, rate-limit aware, filling the task list as it goes.
- A pass that found nothing **reports nothing**. The poller runs every few
  seconds while a run is pending, and a reported pass reloads everything synced
  content appears on — the tag tree, the task list, the selected task. Only a
  pass whose summary is non-empty (an import, an update, a push, a removal, a
  new sub-issue link) drives that reload; the card's status line and last-sync
  time update either way. A manual **Sync now** and the fetch an opened project
  triggers are full passes, which re-read what is there and so always report.

### 5.8 UI

- `integrations.rs`: replace the GitHub stub with a real card — connect (device
  flow: show `user_code` + `verification_uri`, a copy button, a waiting state,
  expiry countdown), account label, disconnect, last-sync time, **Sync now**,
  and the optional personal-token block with its step-by-step instructions and
  an open-the-page button per step.
- Per-project binding lives in the tag settings popover, beside the dirs picker,
  showing the bound `owner/repo` (detected or chosen) with a change affordance.
- Imported tasks carry a source badge with the issue number; the details panel
  shows read-only assignee/milestone chips, the one-way comment list, and an
  **Open on GitHub** link.
- Sync problems render as inline reasons/blocks (the pattern already used by
  blocked phases), not modal errors.

### 5.9 Sub-issues ↔ subtasks

The local subtask tree and GitHub's sub-issues are the same shape, so they are
mirrored (decision 37). The relationship is *GitHub's*: the child's
`external_task_links.field_state` records the parent issue it hangs under
(`parent_issue = "owner/repo#number"`), which is what makes the push idempotent
and a remote un-parenting visible.

**Push (local → GitHub)**

1. A subtask's issue is opened in its **parent's repo**, not its own binding —
   that is where a sub-issue lives. A top-level task still goes where its own
   project tag is bound.
2. When the parent has no issue yet, the parent's own chain is opened first
   (each ancestor in turn), so the mirrored tree matches the local one. A
   subtask whose ancestry reaches no repo-bound project stays local, and so do
   its children.
3. The child is attached with `POST /repos/{owner}/{repo}/issues/{n}/sub_issues`
   using its **database id** (not its number), which the link records at every
   write; a link made before ids were stored pays one `GET /issues/{n}` and
   keeps the answer. `replace_parent` is set, so a tree that moved is repaired
   rather than rejected.
4. Pushing a task also walks its subtasks, so a parent that gains an issue later
   picks up the children it already has. A subtask that fails is reported
   without stopping its siblings.
5. A task inside a run's tree — a step (`workflow_run_id` set) or anything under
   one — never syncs, extending §5.5's step rule to the step's content.
6. An attachment is only made when the child's issue is in the parent's repo:
   the link's external id names the child inside its own repo, so a cross-repo
   sub-issue (allowed on GitHub within one owner) is left alone rather than
   recorded as a different issue of this repo.

**Pull (GitHub → local)**

1. Children are read **from the parent side**: one `list_sub_issues` call per
   issue whose listing summary reports children, plus the parents a link already
   records children under — so a relationship removed on GitHub is noticed even
   once the parent reports no children at all. This is a few calls per pass
   instead of one per issue.
2. A listed child is synced like any other issue and then nested under the
   parent's task. A child whose payload names another repo of the same owner is
   skipped — its number is not this repo's, and pairing the two would name a
   different issue. A child of *this* repo that the page never carried is only
   re-nested when its own link already exists.
3. A child the parent no longer lists is un-nested locally (`parent_id` cleared,
   the recorded relationship dropped); the issue itself is untouched.
4. A cycle is refused rather than allowed to corrupt the local tree.

**Known limits, stated rather than hidden:** ordering within a parent is not
mirrored (no `sub_issues/priority` calls); GitHub's 100-sub-issue and 8-level
nesting limits surface as a failed attach, which is logged and retried by the
next push of that tree; and a relationship created on GitHub for an issue that
has not moved since the cursor is picked up when that issue next appears in a
listing (a full **Sync now** always sees it).

## 6. Part B — the coding workflow becomes git/PR-backed

### 6.1 Branch naming

- **Issue-backed run:** `feature/<issue-number>-<slug>` (decision 13).
  - `slug` comes from the issue title (falling back to the task title).
  - **Stopword-stripped and truncated** (decision 16): drop filler words
    (`the`, `a`, `an`, `fix`, `feat`, `add`, …), take up to ~5 words / ~60
    characters, then run it through the existing `normalize_branch_name`
    (`[A-Za-z0-9._/-]+`), lowercased. Non-ASCII and emoji are transliterated or
    dropped rather than passed through.
  - The app derives this name; the agent's `propose_branch` proposal no longer
    wins for issue-backed runs (decision 13 implies precedence, not the
    "prefer the agent's proposal" option).
- **No issue linked:** today's behaviour is preserved —
  `feature/<run-id>-<title-slug>`, with `propose_branch` still honoured.
- Collisions keep today's rule: if the derived name exists, fall back to
  `feature/<run-id>-<slug>`; if both exist, block with an inline reason.

### 6.2 Worktrees

- **Path:** `<repo>/worktrees/<run-slug>`, where `<run-slug>` is the branch name
  with `/` replaced by `-` (e.g. `feature/123-add-login` →
  `worktrees/123-add-login`), and the run id is appended when that collides
  (nested runs are exempt from the one-run-per-project guard and can coexist).
- **Creation:** `git -C <repo> worktree add worktrees/<run-slug> -b <branch> <base>`,
  at the moment the spec is approved — i.e. replacing today's `create_feature_branch`
  call site in `approve_coding_spec`.
- **One worktree per directory involved in the run** (decision 21): every
  directory of the project tag that resolves to a git repo gets its own worktree,
  branch and (later) PR. Each worktree records **its own base branch**, because
  the repos of a multi-repo project can be on different branches.
- **The user's checkout is never switched.** This retires the dirty-tree refusal
  in `create_feature_branch`: the app only *reads* the base branch from the main
  checkout (`git rev-parse --abbrev-ref HEAD` is read-only). `merge_into_base`
  (the no-remote fallback) keeps today's guard and behaviour, since it does
  switch.
- **Exclusion (decision 18):** append `worktrees/` to
  `$(git -C <repo> rev-parse --git-common-dir)/info/exclude` if absent —
  idempotent, shared by every worktree of that repo, invisible to `git status`,
  never a tracked-file edit, and it cannot dirty the tree.
- **Removal:** `git worktree remove <path>` at merge; a dirty worktree refuses
  and lists the changed paths. Stale entries are cleaned with
  `git worktree prune` (e.g. after a manual `rm -rf`, which is what the verified
  `prunable` state above describes). **The branch is never deleted by worktree
  removal** (verified: it survives), so it still appears in the existing
  "Branches to clean up" list (decision 28).
- Never create a worktree at `.git/worktrees/<name>` — see §4.
- New `coding_git.rs` surface: `worktree_add`, `worktree_remove`,
  `worktree_list`, `worktree_prune`, `git_common_dir`, `ensure_excluded`, plus
  `resolve_remote(dir)` (§5.3) — the one helper shared by sync, the PR step and
  decision 17's fallback test.

### 6.3 Agent pane cwd and sessions

- `AgentProject::resolve()` gains worktree awareness: when the selected task's
  coding run is active and has worktrees, the **primary root is the worktree**
  for the run's main directory (decision 11); the project directory can remain a
  secondary root so the agent can still read non-repo material.
- **Consequence to be explicit about in the UI:** the session key is the cwd, so
  switching to the worktree starts a **new agent session**. The interview
  session stays on the project directory as history; the pane returns to the
  project directory when the run completes or is cancelled.
- The pane should show which checkout it is currently pointed at (worktree path
  vs project directory), so the user is never confused about where edits land.

### 6.4 Shared build cache

- The pane spawns the agent with `CARGO_TARGET_DIR=<main repo>/target` when the
  session is worktree-backed (decision 12), and the app passes the same variable
  for cargo commands it runs inside a worktree. No per-worktree `target/`.
- Trade-offs to document in the UI/docs: cargo's build lock serialises
  concurrent builds across worktrees, and sharing the cache means branch
  switching can invalidate incremental state. A per-project opt-out
  ("isolated build cache") is worth having.

### 6.5 The merge step becomes the PR step

- The recipe is unchanged: node id and phase stay `merge` (decision 24). The app
  decides the action at runtime:
  - **A GitHub remote exists** → the step renders as **"Open pull request"**.
  - **No remote, or a non-GitHub remote** → today's **"Merge the branch"**
    (`git switch <base>` + `git merge --no-ff`, conflict → `git merge --abort`
    + "Resolve in agent"), per decision 17.
- Requirements for the PR action (all selected in decision 15):
  1. **Push first**: `git push -u <remote> <branch>` from each worktree, where
     `<remote>` is the name `resolve_remote` chose — hardcoding `origin` would
     fail in this very repo, which pushes to `github` — using the user's
     configured credentials. A push failure blocks the step with git's own
     stderr.
  2. **Open as a draft**, one PR per repo (decision 25), each shown as a sub-item
     of the step.
  3. **Title/body generated** from the spec plus `propose_summary`
     (`commit_message`, `pr_summary`), shaped by the `AGENTS.md` PR-hygiene
     convention (decision 36):
     - **Title**: imperative, correctly capitalized, no conventional-commit
       prefix, no trailing punctuation; optionally prefixed with a crate name
       when `propose_summary` names one clear scope. Source is the summary's
       title, else the task title — a leading `fix:`/`feat:` is stripped rather
       than passed through.
     - **Body**: the summary, then `Closes #<n>` for the linked issue, then a
       final `Release Notes:` section: the heading, a blank line, and exactly one
       bullet (`- Added …`, `- Fixed …`, `- Improved …`, or `- N/A` for
       non-user-facing changes).
  4. **Labels/assignee/reviewer copied from the issue.**
- Step actions afterwards: **Mark ready for review** (`draft: false`),
  **Open on GitHub** per sub-item, and (for multi-repo) **waive the remaining
  repos** to let the run complete early.
- **No local-merge escape hatch once a GitHub remote exists** (decision 35).
  Rationale: two merge paths for the same project is how a workflow stops being
  legible, and the worktree → branch → PR model assumes the branch's life ends on
  the remote. A project that wants local-only merging expresses that by having no
  github.com remote, which selects decision 17's action automatically. Accepted
  cost: for GitHub-remote projects, the app can no longer complete a run without
  a PR.
- Remotes on other hosts (this repo also has a GitLab remote) are **never
  touched** — no GitLab MRs, no mirroring.

### 6.6 Completion

- **Poll and auto-complete** (decision 19): when the PR (or, for multi-repo,
  every PR) is merged, the app:
  1. marks the run's branch `merged` (`branch_status`),
  2. **removes the worktrees** (keeping the local branches — decision 28),
  3. completes the merge step and the root feature task, completing the run,
  4. closes the issue locally-if-needed (two-way close, §5.6),
  5. leaves the remote branch alone.

### 6.7 Dirty worktree at PR time

- The step **refuses** and lists the changed paths, with a **Commit and
  continue** button that stages and commits everything using the agent's
  `propose_summary.commit_message` (or a generated message from the task title),
  then proceeds (decision 20). Cancel leaves the tree untouched.

## 7. Data model changes — migration `0023_github_integration.sql`

Reused as-is (no change): `integrations` (new row, `provider = "github"`),
`tag_settings.sync_integration_id`/`sync_external_id`/`dirs`,
`external_task_links`/`external_tag_links`, `tasks.deleted_at`,
`tasks.comments`, `tasks.branch_name`, `workflow_runs.branch`/`base_branch`/
`branch_status`.

New:

```sql
-- per-field sync snapshot + local field-change stamps for GitHub links
ALTER TABLE external_task_links ADD COLUMN field_state TEXT;

-- one row per (integration, repo): ETag / since cursor for incremental pulls
CREATE TABLE IF NOT EXISTS integration_sync_state (
    integration_id INTEGER NOT NULL,
    external_id    TEXT NOT NULL,          -- "owner/repo"
    etag           TEXT,
    since          TEXT,
    last_synced_at TEXT,
    UNIQUE (integration_id, external_id)
);

-- the worktree of one run in one repo (decision 21: several rows per run)
CREATE TABLE IF NOT EXISTS run_worktrees (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        INTEGER NOT NULL,
    repo_dir      TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch        TEXT NOT NULL,
    base_branch   TEXT NOT NULL,
    -- the git remote `resolve_remote` picked (decision 33); never assumed to be "origin"
    remote        TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    removed_at    TEXT
);
CREATE INDEX IF NOT EXISTS idx_run_worktrees_run ON run_worktrees(run_id);

-- one row per opened PR (decision 25: several per run)
CREATE TABLE IF NOT EXISTS run_pull_requests (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        INTEGER NOT NULL,
    repo_dir      TEXT NOT NULL,
    integration_id INTEGER NOT NULL,
    owner         TEXT NOT NULL,
    repo          TEXT NOT NULL,
    number        INTEGER NOT NULL,
    url           TEXT NOT NULL,
    head_branch   TEXT NOT NULL,
    base_branch   TEXT NOT NULL,
    draft         INTEGER NOT NULL DEFAULT 1,
    state         TEXT NOT NULL,           -- open | merged | closed
    merged_at     TEXT,
    last_polled_at TEXT,
    UNIQUE (integration_id, owner, repo, number)
);
CREATE INDEX IF NOT EXISTS idx_run_pull_requests_state ON run_pull_requests(state);
```

`base_branch` on `workflow_runs` becomes the primary repo's base for backwards
compatibility; the per-repo truth lives in `run_worktrees`.

## 8. Failure and edge-case matrix

| Situation | Behaviour |
| --- | --- |
| Device code expired / denied | Card shows the error with Reconnect; nothing else changes |
| Token revoked (401) | Block with "Reconnect GitHub"; sync and PR actions stop |
| Rate limited | Auto-retry with backoff from `x-ratelimit-reset`/`retry-after`; only surfaces if it persists |
| Bound repo not visible to the account | Tag left unbound, inline reason, other projects keep syncing |
| Issue deleted / transferred / access revoked | Tombstone local task with a recorded reason |
| Both sides edited a field | Per-field resolution, GitHub wins ties (§5.4) |
| Local delete | Tombstone + close the issue; never resurrected by pulls |
| Issue closed | Task completed (never tombstoned) |
| Sub-issue attach refused (cross-repo child, nesting limit, no recorded id) | Nothing is written; the task and its issue stay as they are, and the next push of that tree retries |
| Sub-issue relationship removed on GitHub | The local task is un-nested (kept, never deleted or tombstoned) |
| No remote resolves to github.com (none, or other hosts only) | Merge step keeps today's local merge (decision 17) |
| Push rejected (non-fast-forward, no credentials) | Block with git's stderr; no retry loop |
| Several remotes, only one of them GitHub (this repo: `github` + `gitlab`) | The GitHub remote is the only one used for push/PR; other hosts are never pushed to or mirrored |
| Reviewer request rejected (not a collaborator, no access) | PR creation still succeeds; the failure is recorded as a note on the step, never a blocked step |
| PR already exists for the branch | Adopt it: store the number/URL and continue instead of erroring |
| Worktree path already exists | Suffix the run id; never reuse another run's checkout |
| Dirty worktree before PR | Refuse + "Commit and continue" (§6.7) |
| One repo of a multi-repo run has no GitHub remote | That sub-item is pushed only; the step reports it and the run can still complete |
| Build cache contention | Cargo's lock serialises; note it and offer the isolated-cache opt-out |

## 9. Out of scope

- Multiple GitHub accounts and GitHub Enterprise/base-URL support (decision 31).
- PRs as tasks, Projects v2 boards, milestones → sections. Cross-repo
  sub-issues (a sub-issue whose repo differs from its parent's) are never
  mirrored, and sub-issue ordering is never written back (§5.9).
- Two-way comments; milestone/assignee write-back.
- Webhooks (polling only), and deleting remote branches after merge.
- Unattended/scheduled coding runs (still user-present, per the coding workflow spec).
- Any change to the Todoist integration beyond sharing the new link columns.
- GitLab (or any non-GitHub host) remotes: never pushed to, mirrored, or turned into merge requests, even when a project has one alongside a GitHub remote.

## 10. Open questions

### 10.1 Resolved after the interview (round 9)

All three product questions are now decisions in §2; nothing below reopens them.

1. **OAuth app ownership → decision 34.** Registered under **`lofi-tools`** — the
   org that already hosts this repo — with device flow enabled. The client id is
   public by design (device flow takes no secret), and `GITHUB_CLIENT_ID` lets a
   fork or a self-build use its own app. Remaining action: create the app under
   that org and justify the `repo` + `read:user` scopes to it.
2. **Local-merge escape hatch → decision 35.** None when a github.com remote
   exists; the PR step is absolute there, and the fallback is selected by the
   absence of such a remote rather than by a per-run toggle.
3. **Release Notes convention → decision 36.** Generated PRs follow the
   `AGENTS.md` PR-hygiene rule even though that block is currently commented out.
   Rationale: it is the repo's documented intent, it costs one template string,
   and if the block is re-enabled the app already complies. Reversible by editing
   that template.

A fourth decision (**33**, remote resolution) was forced by this repo's own
remotes: `github` + `gitlab`, no `origin`. Any prose that said `origin` has been
corrected in §5.3 and §6.5.

### 10.2 Still open

**Product**

1. **Label removal nuance:** may the app remove a label it previously added when
   the tag is removed locally (§5.5), or is the label purely additive?
2. **Comments import window:** all comments, last N, or excluding bots? And
   should the PR link be posted as an issue comment when the PR opens?
3. **Do local edits push while the issue is closed?**

**Mechanical**

4. Exact **stopword list** and the **slug caps** (default: 5 words / 60 chars).
5. How are **"directories involved in the run"** determined for a multi-dir
   project tag — all git-repo dirs of the tag, or only those the run has
   touched? (decided in principle in decision 21, precise rule TBD)
6. Should the **implement/review phase prompts** name the worktree path
   explicitly, in addition to the pane's cwd?
7. Default for the **isolated build cache** opt-out, and whether
   `CARGO_TARGET_DIR` should also be set for the main checkout.
8. **Migration number** could collide with in-flight work from
    `todo2-nested-projects-spec.md` / `todo2-managed-apps-spec.md`; re-check the
    tail of `libs/storage/toasty/migrations/` before writing `0023`.
12. Whether the review phase should read the **PR diff** rather than the
    transcript once a draft PR exists (today's review is transcript + notes).

## 11. Testing plan

Everything runs against fakes; **no live network in CI** (decision 29).

**Storage (`cargo test -p storage`)**

- Table-driven cases for §5.4 resolution: remote-only, local-only, both-newer,
  tie-to-GitHub, snapshot refresh after push (so a push is not re-read as a
  remote edit).
- §5.6 transitions: complete → close, close → complete, reopen, local delete →
  tombstone + close, 404/transfer → tombstone, tombstoned rows never
  resurrected.
- Binding: explicit sync target, auto-detected remote persisted, undetectable
  remote left alone, repo-invisible (404) reason.
- `integration_sync_state` cursor behaviour and idempotent re-sync.
- Sub-issues (§5.9): a subtask opening its parent's chain and attaching once, a
  repeat push making no call, a pulled sub-issue nesting locally, an un-parented
  child being un-nested, a step's subtree staying local, and a child link with
  no recorded id being looked up before it is attached.
- Worktree/PR rows: several per run, per-repo base branches, UNIQUE on PR
  identity, `state` transitions.

**todo-2**

- `coding_git` temp-repo tests (extending the existing four): `worktree add` in
  the decided path, `info/exclude` written once and idempotently, `workdir
  remove` refuses dirty, `prune` clears a manually deleted checkout, and the
  branch survives removal.
- Branch naming: issue number + stopword-stripped truncated slug, non-ASCII and
  emoji titles, collisions → run-id fallback, no-issue → today's default.
- `resolve_remote` resolution order on a fixture repo that mirrors this one
  (`github` + `gitlab`, no `origin`, `remote.pushDefault = github`), plus a repo
  with `origin` only and one with no github.com remote at all.
- PR title/body convention (decision 36): a leading `fix:`/`feat:` is stripped,
  trailing punctuation is dropped, `Closes #<n>` is present, and the
  `Release Notes:` section is exactly heading + blank line + one bullet.
- Stepper states for the PR step: draft opened, "mark ready", multi-repo
  sub-items, waiver, dirty-worktree "commit and continue", no-remote fallback
  showing "Merge the branch".
- Pane: worktree-aware `resolve()` returns the worktree as primary root and a
  *different* session key than the project directory.
- Device-flow card: waiting state, expiry, error, connected.
- A `GithubClient` trait with a fake implementation, plus recorded JSON fixtures
  for issues/labels/PR payloads.

**Manual (documented, not CI)**

- Real connect, first quiet backfill, a real worktree run end to end, a draft PR
  on a scratch repo, merge detection, worktree removal and branch retention.

## 12. Implementation order (sync first, per decision 1)

1. **Auth**: `github_auth.rs` (device flow) + `github.json` storage + the
   integration card (connect/disconnect/account), tests with a fake HTTP layer.
2. **Storage**: migration `0023`, `field_state`, `integration_sync_state`,
   link/identity helpers, and the sync engine (pull → resolve → push →
   watermark) with the full test table from §11.
3. **Task-side mapping**: repo → namespaced project tag, binding via tag
   settings + remote auto-detection, source badge, `Open on GitHub`, comment and
   metadata rendering.
4. **Sync loop**: demand-driven poller, ETag/`since`, rate-limit handling,
   retry/block policy.
5. **Worktrees**: `coding_git` additions, `run_worktrees`, worktree creation in
   `approve_coding_spec` (and the retirement of the dirty-tree guard there),
   `info/exclude` management, cleanup at merge, temp-repo tests.
6. **Agent pane**: worktree-aware resolution, primary/secondary roots, the
   current-checkout indicator, `CARGO_TARGET_DIR` on the spawned agent.
7. **Branch naming**: issue-derived `feature/<n>-<slug>` with stopword stripping
   and truncation.
8. **PR step**: `resolve_remote` per worktree, push to that remote, draft PR per
   repo, generated title/body per the PR convention, label/assignee/reviewer
   copy, sub-items, "mark ready", waiver, dirty-tree commit-and-continue.
9. **Completion + polish**: merge polling, auto-complete, worktree removal,
   branch retention in the cleanup list, failure matrix coverage in the UI.

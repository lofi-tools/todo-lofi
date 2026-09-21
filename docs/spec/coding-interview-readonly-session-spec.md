# Read-only interview sessions for coding runs — Spec

Status: Proposed (no code written; this spec is the output of the interview)
Relates to: `docs/spec/coding-workflow-runs-spec.md` (the run, its phases, the
MCP tool surface), `docs/spec/coding-workflow-subtasks-spec.md` (coverage,
`role`, `spec_covered_at`), `docs/spec/todo2-agent-pane-ui-spec.md` (the pane),
`docs/spec/acp-client-and-agent-panel-spec.md` (process launch, `session/new`),
`docs/spec/agent-cli-interview-feature-spec.md` (`/interview`, `ask_user`).

## 1. Intent

The request, verbatim:

> opencode: for interview step, the model should not be able to change files.
> the agent should be launched in a new session without write tools. How to
> remove default tools on opencode via cli (the agent is spawned in the
> background via cli, right?)
> todo-lofi should inject get_spec & set_spec tools via mcp (into the new agent
> session)
>
> Where to store the spec ? in a extra_data column on task ? namespaced by
> app/extension ? is that a common pattern ?

So this spec answers three things:

1. **No file changes during the interview.** The interview phase today runs on
   the project's general agent session with full tools, so the model writes
   `docs/spec/<slug>-spec.md` to disk and can touch anything else. The interview
   must run against a **read-only opencode profile** (writes and destructive
   shell denied) in a **separate process**.
2. **The spec arrives through MCP, not the filesystem.** A read-only model
   cannot write a file, so the spec is delivered to todo-lofi by a tool call:
   `set_spec` (renamed from `save_spec`) with `get_spec` to read it back.
3. **A reusable place to store per-task extension data.** Specs move out of the
   `tasks` table's dedicated columns into a **namespaced key/value store**
   (`task_extra`), so any app/extension can keep task-scoped data without a new
   column each time. This is the "extra_data / namespaced by app" idea from the
   request, implemented as a table rather than a single JSON column.

The workflow then continues with a **second, full-tool process** for the coding
phases, spawned when the user starts the next step from the run UI.

## 2. Interview decisions (normative)

| # | Topic | Decision |
| --- | --- | --- |
| 1 | Agent | **opencode first.** ACP sessions are already implemented (`OpenCodeAgent`, `opencode acp`). More ACP agents later, behind the existing `AgentServer` registry. |
| 2 | Session model | **Two processes, one per profile.** A read-only process for the interview, a full-tools process for coding. Not two sessions inside one process (opencode's tool profile is process/config-level, not per-ACP-session). |
| 3 | First session | The interview session is the **first** session a coding run spawns. Later, the coding session is a second process in the same project with a different profile. |
| 4 | Write denial | **Deny writes, keep read tools.** `edit` denied; `bash` restricted to an **allow-list of read commands** (unknown commands denied); read/glob/grep/list remain; `task`, `external_directory`, `webfetch`, `websearch` remain allowed. |
| 5 | Config delivery | **`OPENCODE_CONFIG_CONTENT` inline JSON**, generated per launch. The app never edits the user's global opencode config. |
| 6 | Agent profile | An **app-defined named opencode agent** (proposed name `todo-interview`) declared in the injected config, carrying the permission profile explicitly. |
| 7 | Storage | A new **`task_extra` table**: `(task_id, namespace, key, value)` with JSON values, unique on `(task_id, namespace, key)`. Namespace for this feature: `coding`, key: `spec`. |
| 8 | Migration | **Migrate and drop**: backfill `tasks.spec` into `task_extra`, then remove `tasks.spec` and `tasks.spec_path`. `spec_covered_at` and `role` stay typed columns (they are coverage/identity facts, not extension data). |
| 9 | Tools | `save_spec` is **renamed to `set_spec`**; `get_spec` is added (spec text only); `get_coding_context` **stays** as the structured "all task details, including the current spec" object. |
| 10 | Tool sets | **Both profiles expose the same MCP tools** (all of them). The difference between profiles is opencode's *filesystem/shell* tools, not the MCP surface. |
| 11 | MCP scoping | **Per-profile token.** One app-wide loopback endpoint; each process gets a token bound to its profile/run so the server can scope and audit tool calls. The current tool split is identical, so the token is the seam for narrowing later. |
| 12 | MCP transport | The server is attached through the **ACP `session/new` `mcpServers`** field (the runs spec's original plan), not the injected opencode config. |
| 13 | Visibility | The run's sessions are **visible in the agent pane**; only one session is shown at a time (interview while interviewing, coding while coding). The **user** starts the coding step from the run UI — nothing auto-advances into a coding process. |
| 14 | Transcripts | Transcripts are **kept**. Switching from the interview session to the coding session does not discard the interview transcript. |
| 15 | Interview lifecycle | The read-only process is spawned **per phase** (on "Start interview") and **killed once the spec is saved** (or the run is cancelled). |
| 16 | Coding session | Spawned **when the user clicks the launcher on the run's next step** (implement), with the **full tool set** (decision #13). |
| 17 | Spec file on disk | **Nobody writes it.** The spec lives only in `task_extra`; `spec_path` is retired and the "open file" affordance goes away. |
| 18 | No `set_spec` | The app **auto-injects a reminder prompt** telling the model to call `set_spec`. The phase does not advance on a reminder alone — the step stays open until a spec lands (or the user takes a manual path). |
| 19 | Model | The interview process uses the **same model/mode as the project's pane session**; no separate model setting. |
| 20 | Scope | **Uniform**: root interview, sub-interviews, and nested/promoted runs all use the read-only process and the same namespaced storage. |
| 21 | Fallback | If the read-only profile cannot be applied (old opencode, rejected config, launch failure), the phase is **blocked with an inline reason and a fix affordance** — never silently run with full tools. |

Superseded during the interview (recorded so the reasoning is not lost):

- "the coding session spawns automatically at spec approval" → **superseded by
  #13/#16**: the user clicks the step's launcher.
- "extra_data as a single JSON column on `tasks`" → **superseded by #7**: a
  namespaced table, so rows are addressable and indexable.
- "interview session gets a narrower MCP tool subset" → **superseded by #10**:
  same tools both profiles; the profile gates opencode's own tools.

## 3. Current state (verified in this repo)

### 3.1 The interview runs with full tools

- The details pane emits `TaskDetailsEvent::CodingLaunch { phase, prompt }`
  (`apps/todo-2/src/ui_parts/task_details.rs:421`, emitted at `:4274`); the
  Layout inserts the composed prompt into the agent pane and switches the right
  pane to `Agent` (`apps/todo-2/src/main.rs:654`).
- The pane **hardcodes** `Arc::new(OpenCodeAgent)` (`agent_pane.rs:515`), whose
  `args()` is `["acp"]` (`libs/acp-client/src/agent.rs:66`). There is no
  read-only variant and no per-profile process.
- `phase_prompt("interview")` is `INTERVIEW_BASE_PROMPT` + title + description.
  The base prompt explicitly tells the model to **write a spec file**
  (`./docs/spec/<slug>-spec.md`, `apps/agent-cli/src/interview.rs`), and the
  model has the tools to do it. That is the behaviour this spec removes.

### 3.2 The spec is stored in dedicated columns

- `tasks.spec` / `tasks.spec_path` (migration `0017_coding_workflow.sql`,
  model at `libs/storage/src/task.rs:66`/`:68`), rendered by the details pane's
  spec artifact (`task_details.rs:5159`) with an "open file" hint
  (`spec_path_hint`, `:5472`).
- Writes go through `TodoStore::save_task_spec` (`task.rs:383`) and
  `save_subtask_specs` (`libs/storage/src/workflow.rs:2139`).
- `role` and `spec_covered_at` are the other two coding columns
  (`0025_coding_step_role.sql`; coverage helpers at `workflow.rs:179`-`233`).
- There is **no generic extension storage**: the pattern in this repo is either
  a dedicated typed column or a JSON column on the row (`labels`, `comments`,
  `workflow_runs.step_results`, `workflow_runs.params`).

### 3.3 The MCP endpoint is not attached to any agent yet

- `coding_mcp.rs` hosts one app-wide loopback HTTP endpoint (`coding_mcp::start`,
  `main.rs:241`) with 8 tools and a bearer token, started at app launch and
  never handed to a session.
- `libs/acp-client`'s `new_session` builds
  `NewSessionRequest::new(cwd).additional_directories(additional)` only
  (`connection.rs:86`) — **no `mcp_servers`**. `mcp` does not appear anywhere in
  `libs/acp-client`.
- The runs spec already recorded this as open: agent-cli's MCP client
  (`cersei::mcp`) only speaks stdio, so the loopback HTTP endpoint needs an HTTP
  transport or a stdio shim before it can be attached
  (`coding-workflow-runs-spec.md` §19 "Still open", item 2).

### 3.4 The pane owns one session per project

- One `ProjectEntry` per directory-backed project, one ACP connection, one
  session id, a transcript, a busy flag, a prompt queue, permission cards and
  terminals (`agent_pane.rs`). `new_session` (`:1165`) resets that one session;
  `create_session` (`:1990`) is the only `session/new` call path.
- Sessions are persisted per agent id (`SessionStore`); a different agent id is
  therefore a different persisted session key.

### 3.5 The task list is positional on the `tasks` table

`libs/storage/src/task.rs` parses task rows positionally (`:264`-`:295`) and
`workflow.rs` has raw `SELECT`s; the subtasks spec already warns that new
columns must be appended in one place with every index updated. Dropping two
columns touches the same three parsers.

## 4. The model on one page

```
project directory
│
├── interview process   (opencode acp, OPENCODE_CONFIG_CONTENT: read-only agent)
│     session/new  + mcpServers → todo-lofi MCP (per-profile token)
│     visible in the pane while the interview phase runs
│     killed when set_spec lands
│
└── coding process      (opencode acp, default profile, full tools)
      session/new  + mcpServers → the same MCP, coding-profile token
      spawned when the user clicks the implement step's launcher
      visible in the pane from then on

spec flow:  get_spec / set_spec  →  task_extra (namespace "coding", key "spec")
            set_spec completes the interview step → run advances to the spec gate
```

## 5. opencode integration — the read-only profile

### 5.1 Findings (opencode CLI, researched)

- `opencode acp` starts the ACP server (stdin/stdout, nd-JSON). The ACP client
  already launches this (`OpenCodeAgent`).
- Tools are gated by an agent's **`permission` block**, not the deprecated
  `tools` map. Keys include `read`, `edit` (gates `write`/`edit`/`apply_patch`),
  `glob`, `grep`, `list`, `bash`, `task`, `external_directory`, `todowrite`,
  `webfetch`, `websearch`, `lsp`, `skill`. Each takes `"allow" | "ask" |
  "deny"`; `bash` (and `read`, `edit`, `glob`, `grep`, `list`, `task`, `lsp`,
  `skill`) additionally take **glob → action maps**, and the **last matching
  rule wins**.
- Agents are declared in `opencode.json` under `agent.<name>` (or markdown in
  `~/.config/opencode/agents/` and `.opencode/agents/`). The built-in `plan`
  agent is read-only-ish (`edit`/`bash` default to `ask`), but "ask" is not
  "denied".
- Config can be injected without touching the user's files via
  `OPENCODE_CONFIG` (path), `OPENCODE_CONFIG_CONTENT` (inline JSON),
  `OPENCODE_CONFIG_DIR`, and `OPENCODE_PERMISSION` (inline permission JSON).
- `opencode run` is the non-interactive one-shot mode; `opencode serve` is a
  long-lived headless backend; `--agent <name>` selects the profile, but only on
  `run` — `opencode acp` takes no such flag and exits with its help when given
  one. A config's `default_agent` (a primary agent's name) is what selects the
  profile for an ACP session. Binary resolution already exists
  (`resolve_program`, `agent.rs`).

Conclusions used below: (a) a **named agent with an explicit permission block**
is the reliable way to get a hard read-only profile; (b) the profile belongs to
a **process**, which is exactly why decision #2 is two processes; (c) it can be
delivered with **no writes to the user's config** via env.

### 5.2 The injected config

Per launch, todo-lofi builds an inline config (serialized to
`OPENCODE_CONFIG_CONTENT`) along the lines of:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "agent": {
    "todo-interview": {
      "mode": "primary",
      "description": "todo-lofi interview: gathers context and saves a spec; never changes files.",
      "prompt": "<the interview instructions, see §9.1>",
      "permission": {
        "edit": "deny",
        "bash": {
          "*": "deny",
          "cat *": "allow",
          "ls *": "allow",
          "rg *": "allow",
          "find *": "allow",
          "git status *": "allow",
          "git log *": "allow",
          "git diff *": "allow",
          "git show *": "allow"
        },
        "task": "allow",
        "external_directory": "allow",
        "webfetch": "allow",
        "websearch": "allow"
      }
    }
  }
}
```

Rules:

- `"*": "deny"` goes **first** wherever a glob map is used, because the last
  matching rule wins; every allowed command is listed after it.
- `edit: "deny"` covers `write`, `edit`, and `apply_patch` in one key.
- The allow-list is a **starting set** and lives as one constant in the app so
  it is reviewable and testable; adding a command is a one-line change.
- `task` stays allowed (per decision #4), which means a subagent could in
  principle be invoked. Subagents inherit the denying agent's permissions in
  opencode, but this is called out as a risk in §14.
- The config is generated with a small pure builder (`agent` name, prompt,
  allow-list) so it is unit-testable without launching opencode. It also sets
  `default_agent: "todo-interview"`.

### 5.3 Launch shape

- A second `AgentServer` implementation (working name `OpenCodeInterviewAgent`)
  reuses `program: "opencode"`, `args: ["acp"]`, and overrides `spawn_spec` to
  add the `OPENCODE_CONFIG_CONTENT` env var. `SpawnSpec` already carries
  `env: BTreeMap<String, String>`, so no transport change is needed to inject
  it. The process is not told the agent through `args` — `opencode acp` rejects
  `--agent` — so the injected `default_agent` selects it, and the launch
  additionally switches the new session to `todo-interview` with
  `session/set_mode` (an `AgentServer::session_mode`) and fails the launch if
  the agent refuses: the interview must never run under a writable default.
- The coding profile gets the **default** launch (today's `OpenCodeAgent`), so
  existing behaviour is untouched for non-coding use (decision #3 in §2 table,
  config scope).
- The `--pure` global flag (skip external plugins) is worth considering for the
  read-only process, since a plugin can run code in the session's behalf; see
  §14.

### 5.4 Version and capability

There is no reliable "does this opencode support the injected agent config"
probe before launch. The app therefore treats "process did not complete the ACP
handshake" as the failure signal (the client already surfaces
`AcpError::Launch` with stderr, `connection.rs`), and blocks the phase per
decision #21.

## 6. Sessions and the pane

- **Interview.** "Start interview" spawns the read-only process, opens a
  session rooted at the project directory, attaches the MCP servers, and shows
  that session in the pane. The user can watch the transcript and answer
  `ask_user` questions through the existing question-card path (the elicitation
  bridge is a prerequisite — see §14).
- **Coding.** The user clicks the launcher on the run's next step (implement).
  A **second process** starts with the default profile; its session replaces the
  interview session in the pane. The interview transcript is kept as history.
- **Chat.** With no active coding run the pane behaves exactly as today. The
  injected config applies to **coding-run processes only**; a project's general
  chat never gets it.
- **Session identity.** Two agent ids ⇒ two persisted session keys (the
  `AgentServer::id()` is the persistence key). Choose ids such as
  `opencode-interview` and `opencode` so a resumed session cannot cross
  profiles.

## 7. MCP surface

### 7.1 Tools

The endpoint keeps all its tools and gains one; `save_spec` is renamed:

| Tool | Change |
| --- | --- |
| `get_coding_context` | unchanged — the structured object containing all task details including the current spec. |
| `get_spec` | **new** — returns the spec markdown for a task (namespace `coding`, key `spec`), so a re-interview round can read what exists. |
| `set_spec` | **renamed from `save_spec`** — writes the spec and advances the interview step exactly as today. |
| `create_sub_task`, `request_sub_task_interview`, `append_note`, `propose_branch`, `complete_phase`, `propose_summary` | unchanged. |

Surface goes **8 → 9**; the tests that assert the tool count
(`coding_mcp.rs`, `tools_are_declared_with_schemas` and the socket test
`serves_json_rpc_over_loopback_http`) must be updated with the rename and the
new tool.

### 7.2 Attachment

- `acp-client`'s `Requester::new_session` grows an MCP parameter and
  `NewSessionRequest` is built with it (the ACP v1 schema models MCP servers;
  agent-cli already parses `mcpServers` with `name`/`command`/`args`/`env` plus
  `url`/`type`, `apps/agent-cli/src/acp.rs:564`). The pane passes the loopback
  URL and token.
- **Risk:** the endpoint is HTTP; opencode's ACP implementation may only accept
  stdio servers, and `cersei::mcp` (agent-cli's client) is stdio-only too. The
  spec requires the HTTP form and records the fallbacks in §14 (stdio shim
  process, or an `mcp` block in the injected config).
- **Per-profile token.** `coding_mcp::start` currently mints one token. Change
  it to mint a token per profile/run and expose `open_profile_token(profile,
  run_id)`; `tools/call` resolves the profile from the token and refuses calls
  the profile may not make. With the current identical tool sets the only
  observable effect is audit/provenance, which is the point of the seam.
- The `MCP` tool results and error shapes stay as they are (`tool_result`,
  `isError`), so the model's error handling is unchanged.

## 8. Data model — `task_extra`

### 8.1 Migration `0026_task_extra.sql`

```sql
-- A namespaced key/value store for per-task data owned by an app/extension, so
-- extensions stop needing a dedicated column each. `value` is JSON text.
-- #[toasty::breakpoint]
CREATE TABLE IF NOT EXISTS task_extra (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "task_id" INTEGER NOT NULL,
    "namespace" TEXT NOT NULL,
    "key" TEXT NOT NULL,
    "value" TEXT NOT NULL,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX IF NOT EXISTS idx_task_extra_scope
    ON task_extra("task_id", "namespace", "key");
-- #[toasty::breakpoint]
INSERT INTO task_extra ("task_id", "namespace", "key", "value", "created_at", "updated_at")
    SELECT id, 'coding', 'spec', json_quote(spec), <now>, <now>
    FROM tasks WHERE spec IS NOT NULL AND trim(spec) <> '';
-- #[toasty::breakpoint]
ALTER TABLE tasks DROP COLUMN "spec";
-- #[toasty::breakpoint]
ALTER TABLE tasks DROP COLUMN "spec_path";
```

Notes:

- `spec_path` is **not** migrated (decision #17): there is no file to open any
  more. Existing paths are dropped with the column; the round log and the spec
  itself carry the useful history.
- `cascade`: deleting a task should delete its `task_extra` rows. The repo has no
  FK cascades on tasks today (subtasks are handled in application code), so this
  is done in the task-delete path, not in SQL — and covered by a test.
- Index: the unique index doubles as the lookup index for `(task_id,
  namespace)`; add a plain `(namespace, key)` index only if a query needs it
  (none do today).

### 8.2 API

```rust
/// One namespaced slice of a task's extension data.
pub struct TaskExtra { task_id: u64, namespace: String, key: String, value: serde_json::Value }

impl TodoStore {
    pub async fn get_task_extra(&mut self, task_id: u64, namespace: &str, key: &str)
        -> QueryResult<Option<serde_json::Value>>;
    pub async fn set_task_extra(&mut self, task_id: u64, namespace: &str, key: &str,
        value: serde_json::Value) -> QueryResult<()>;   // upsert on the unique index
    pub async fn task_extra_for_namespace(&mut self, task_id: u64, namespace: &str)
        -> QueryResult<BTreeMap<String, serde_json::Value>>;
}
```

`save_task_spec(task_id, spec, path)` becomes a thin wrapper that writes
`("coding", "spec", Value::String(markdown))`; `save_subtask_specs` keeps its
transaction shape and still refuses step rows (subtasks spec decision #31). The
coverage helpers read `spec` through the new accessor; `spec_covered_at` and
`role` stay on the task.

### 8.3 Is this a common pattern? (the request's question)

Short answer: **yes, for data a third party owns; no, when the app itself has a
first-class concept.**

- The "one JSON/extra column" shape is common in task/issue trackers
  (Jira's entity properties, Linear's metadata, GitHub's "custom properties",
  Notion properties). It solves exactly the problem the request names: an
  extension can add data without a schema migration each time.
- A **namespaced table** (what this spec picks) is the slightly more rigorous
  version of the same idea: rows are addressable, indexable, and one
  extension's blob cannot collide with another's key space. It also avoids the
  "one giant JSON column that everything reads and rewrites" write-amplification
  problem.
- The counter-pattern is also real and worth stating: a **first-class concept
  deserves a typed column**, which is why `role` and `spec_covered_at` stay on
  `tasks` (they are queried by core surfaces and drive filtering). The spec text
  moves because it is opaque markdown that only the coding surfaces read, and
  because the request explicitly wants an extension-shaped home for it.
- Naming the namespace after the **owning app/extension** (`coding`) rather than
  the consumer is what keeps two extensions from racing for the same key.

## 9. Phase flow

### 9.1 The interview prompt

The prompt becomes the app-defined agent's `prompt` (injected config) plus the
existing composed target. It keeps the current interview instructions minus the
file-writing step:

- gather context with the read tools;
- ask clarifying questions in rounds via `ask_user`;
- **call `set_spec` with the finished markdown** — do not write a file;
- `get_spec` first on a later round to read the spec being revised;
- the subtask section from `coding-workflow-subtasks-spec.md` §8.1
  (enumerate open subtasks; spec each or mark covered) is unchanged.

### 9.2 Completion and the reminder

- `set_spec` writes `task_extra` and completes the open `interview` step, which
  advances the run to the spec gate (today's behaviour, unchanged).
- If the process ends or goes idle without a spec, the app **injects a reminder
  prompt** ("you have not saved a spec yet; call `set_spec`") into the same
  session. The step stays open. Repeated non-compliance leaves the phase open
  with the manual paths still available ("Write spec manually").
- Once the spec lands, the read-only process is **killed** (decision #15) and
  the pane's transcript for that session is retained as history.

### 9.3 Starting the coding phase

The run's implement step carries a launcher. Clicking it spawns the full-tools
process, opens the session, attaches MCP, inserts the composed implement prompt
(the umbrella plus each open subtask spec, per the subtasks spec §8.2) and
shows it in the pane. Nothing here auto-sends.

## 10. Failure and edge-case matrix

| Situation | Behaviour |
| --- | --- |
| opencode not on PATH | Phase action disabled with the existing "Configure agent" reason. |
| Injected config rejected / handshake fails | Phase blocked with an inline reason carrying the captured stderr (never run with full tools). |
| Model writes to disk anyway | Impossible via `edit`; a `bash` write is denied by the allow-list. Any file change that still happens is a bug to report, not a fallback. |
| Model never calls `set_spec` | Reminder prompt injected; the step stays open; manual spec paths remain. |
| `set_spec` called with an empty body | Refused by the tool (existing guard), nothing written. |
| Re-interview round (reject at spec/review) | A **new** read-only process is spawned for the round; it calls `get_spec`/`get_coding_context` to read the existing spec (`role`, coverage, round log unchanged). |
| Sub-interview on a subtask | Same read-only process; its spec lands in that subtask's `task_extra` row. |
| Nested/promoted run | Same model, recursively (decision #20). |
| Two runs on one project | Each run spawns its own processes; only one session is shown in the pane at a time. Interaction between concurrent processes and the pane's single-session model is an open question (§14). |
| Run cancelled mid-interview | The read-only process is killed; the step is tombstoned as today. |
| Spec ever read by the old column path | Gone with the column: every read goes through `get_task_extra` / `run_subtasks`. |
| Step row spec | Still refused (subtasks spec decision #31): `set_spec` and `save_task_spec` reject a step id. |
| Existing run with `tasks.spec` data | Backfilled by the migration; the run's spec artifact keeps rendering. |

## 11. Code change list (by file)

**`libs/acp-client/`**
1. `connection.rs`: `Requester::new_session` takes MCP servers and builds
   `NewSessionRequest` with them; thread the parameter through `create_session`
   callers.
2. `agent.rs`: `OpenCodeInterviewAgent` (or a parameterized `OpenCodeAgent`) that
   injects `OPENCODE_CONFIG_CONTENT` through `SpawnSpec.env` and names its mode
   through `AgentServer::session_mode`; keep the default `OpenCodeAgent`
   untouched for chat.
3. `acp_client.rs` exports for the new type(s).

**`libs/storage/`**
4. `toasty/migrations/0026_task_extra.sql`: table, unique index, backfill,
   drop `spec`/`spec_path`.
5. `task.rs`: remove the two model fields, update the three positional parsers,
   turn `save_task_spec` into the `task_extra` write, add the
   `get/set/list_task_extra` accessors, delete `spec_path` handling.
6. `workflow.rs`: `save_subtask_specs` writes through the new store; coverage
   helpers read the spec via `get_task_extra`; task deletion clears
   `task_extra`.
7. `lib.rs` exports for the new type/API.

**`apps/todo-2/`**
8. `coding_mcp.rs`: rename `save_spec` → `set_spec`; add `get_spec`; per-profile
   tokens (`start` mints a token per profile/run, `tools/call` resolves the
   profile); update the two tool-count tests.
9. `coding_agent.rs` (new, small): the injected-config builder (pure, tested),
   the read-only allow-list constant, the two `AgentServer` selections, and the
   profile→spawn wiring.
10. `store.rs`: `save_coding_spec` / `save_task_spec` call sites move to the new
    accessor; `start_coding_run` unchanged; new wrappers for spawning the
    interview and coding profiles and for the reminder prompt.
11. `ui_parts/agent_pane.rs`: accept a profile when opening a session; allow the
    pane to hold the interview session and then the coding session; keep the
    retiring transcript; implement the session switch.
12. `ui_parts/task_details.rs`: the launcher that spawns the coding profile; drop
    `spec_path_hint` and the "open file" affordance; spec reads via the new
    accessor; blocked reason for a failed read-only launch.
13. `main.rs`: `CodingLaunch` routes `interview` to the read-only profile and the
    later phases to the coding profile; MCP URL/token handed to the pane
    per profile; the reminder-prompt wiring.

**Docs**
14. Update `coding-workflow-runs-spec.md` §5.2/§7.2 (`save_spec` → `set_spec`,
    the added `get_spec`, the spec no longer on `tasks`) and the subtasks spec's
    references to `tasks.spec` / `save_spec`, so cross-references stay true.

## 12. Testing plan

**Storage (`cargo test -p storage`)**
- Migration: a task with `spec` gets one `task_extra` row `("coding","spec",…)`;
  a task with whitespace-only spec gets none; `spec_path` is gone from the
  schema; the unique index rejects a duplicate upsert (or the upsert updates).
- `get/set/list_task_extra`: round-trip a JSON value, upsert overwrites, unknown
  key returns `None`, two namespaces do not collide.
- `save_task_spec` / `save_subtask_specs`: still refuse a step id; still write
  the subtask specs and coverage marks in one transaction; the spec is read back
  through `get_task_extra`.
- Coverage helpers: unchanged results after the storage move (regression).
- Task deletion removes its `task_extra` rows.

**todo-2 (`cargo test -p todo-2`)**
- `coding_mcp`: 9 tools declared; `get_spec` returns the markdown and `null`
  when absent; `set_spec` writes and advances the interview step; the old
  `save_spec` name is gone; a per-profile token resolves the right profile and a
  mismatched call is refused.
- Injected-config builder (pure): the generated JSON has `edit: "deny"`, a
  `bash` map whose first rule is `"*": "deny"`, the named agent, and no writes
  to any user config path; `OPENCODE_CONFIG_CONTENT` is the only config channel.
- Spawn spec: the interview profile carries the env var, spawns `opencode acp`
  with no `--agent`, and reports `todo-interview` as its session mode; the
  coding profile is byte-identical to today's.
- The reminder prompt fires once when the interview process ends without a spec.

**UI (GPUI, `TestAppContext`)**
- The pane can show the interview session and then switch to the coding session
  while retaining the interview transcript.
- The implement step's launcher spawns the coding profile; the interview step's
  action spawns the read-only profile.
- A failed read-only launch renders the blocked reason and disables the action.
- The spec artifact renders from `task_extra`; no "open file" hint remains.

**Manual verification**
- Start a run against a scratch repo with opencode installed; confirm the
  interview transcript shows denied `edit` attempts and that no file changed
  (`git status --porcelain` clean); confirm `set_spec` lands and the process
  exits; confirm the coding session then starts on user click.

## 13. Out of scope

- More ACP agents beyond opencode (decided #1: later).
- Non-coding workflows and general chat: they never get the injected profile.
- Writing a spec file to disk on the agent's behalf, and any `docs/spec/` file
  convention for agent-produced specs.
- Editing opencode's global config, or shipping an opencode plugin.
- A UI for the injected profile/allow-list (it is a code constant for now).
- Coverage-model changes: `role` / `spec_covered_at` and the `spec covers x/y`
  semantics are untouched.
- Syncing `task_extra` to Todoist/GitHub (it never leaves the local store).
- Sandboxing opencode beyond its own permission system (no seatbelt/container).

## 14. Open questions and risks

1. **HTTP MCP over ACP.** The chosen transport is ACP `session/new.mcpServers`,
   but the endpoint is loopback HTTP and the repo's own notes say agent-cli's
   MCP client is stdio-only. Confirmed fallbacks, in order: (a) an `mcp` block
   with `type: "http"`/`url` in `OPENCODE_CONFIG_CONTENT` (opencode supports
   remote servers), (b) a tiny stdio shim that proxies to the HTTP endpoint.
   The attach mechanism is the one thing most likely to change during
   implementation.
2. **`task` + subagents.** `task` stays allowed (decision #4). If opencode
   subagents can write regardless of the parent's `edit: "deny"`, a reported
   write during an interview means `task` must also be denied. The spec leaves
   `task` allowed and records this as the first thing to verify.
3. **Plugins.** A user-installed opencode plugin runs with the session's
   privileges. Whether the read-only process should pass `--pure` (skip external
   plugins) is undecided; recommend yes, since a plugin is a write path outside
   the permission system.
4. **The pane's single-session model.** Two processes per run and one visible
   session is fine for one run, but the interaction between concurrent runs on
   one project (and the pane's per-project persisted session) needs a concrete
   rule; the spec assumes "most recently launched run wins the pane".
5. **Coding process teardown.** This spec pins the interview process's death
   (at `set_spec`) but not the coding process's: kill it at merge/cancel, or keep
   it alive for the run's later phases (review, merge re-entry)? Recommend keep
   alive until the run ends, so a review round does not pay a cold start.
6. **Reminder cadence.** How many reminders before the app gives up and points
   the user at the manual spec path, and whether the reminder counts as a round
   in the round log (recommend: no, rounds are rejections).
7. **Exact `OPENCODE_CONFIG_CONTENT` precedence.** Whether the inline config
   *replaces* or *merges with* the project's `opencode.json` — the profile must
   not be silently overridden by a project-level `agent` block. Needs a manual
   check against the installed opencode version.
8. **Migration `DROP COLUMN` support.** libsql/Turso must support
   `ALTER TABLE … DROP COLUMN`; if not, the fallback is a table rebuild
   (create-copy-drop-rename) in the same migration.
9. **Model/mode for the read-only process.** Decision #19 says "same as the
   pane", but the pane's model state is per-project UI state; the exact plumbing
   (read it at spawn time, or persist it) is unspecified.
10. **`get_spec` vs `get_coding_context`.** `get_coding_context` already returns
    the spec, so `get_spec` is a convenience the prompt must actually use;
    otherwise it is dead surface. Recommend the prompt's re-interview path calls
    `get_spec`, and `get_coding_context` stays for the structured view.

## 15. Implementation order

1. **Storage**: migration `0026`, the `task_extra` model + accessors,
   `save_task_spec`/`save_subtask_specs` rewiring, coverage regression,
   delete-cascade. Tests.
2. **MCP**: `set_spec` rename + `get_spec`; per-profile tokens; tool-count test
   updates. Tests.
3. **MCP attachment**: `acp-client` `new_session` MCP parameter and the pane
   handing over URL + token; verify against real opencode (open question 1).
4. **Read-only profile**: the injected-config builder + allow-list constant +
   `OpenCodeInterviewAgent`; spawn/teardown per phase; reminder prompt. Tests
   for the pure builder.
5. **Pane**: profile-aware session open/switch with retained transcripts.
6. **UI wiring**: the interview launcher uses the read-only profile; the
   implement launcher spawns the coding profile; blocked reason on failure; drop
   `spec_path_hint`.
7. **Docs sweep**: update the runs and subtasks specs' `save_spec` / `tasks.spec`
   references.
8. **Polish**: `--pure` decision, coding-process teardown, reminder cadence,
   empty states.

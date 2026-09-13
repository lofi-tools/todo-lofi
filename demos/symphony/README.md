# Symphony

A long-running automation service that reads work from an issue tracker, creates an isolated
workspace per issue, and runs a Codex coding-agent session inside that workspace.

The behaviour contract is `docs/spec/symphony.md`. This README documents the
implementation-defined choices the specification requires ports to state explicitly.

## Running

```sh
# Uses ./WORKFLOW.md from the current working directory
cargo run -p symphony

# Explicit workflow path
cargo run -p symphony -- path/to/WORKFLOW.md

# Enable the optional HTTP observability server on an ephemeral port
cargo run -p symphony -- --port 0
```

The positional argument selects the workflow file (Section 5.1). The CLI exits non-zero when
startup validation fails (missing/unparseable `WORKFLOW.md`, unsupported tracker kind, missing
tracker credentials, invalid config) and zero on a normal shutdown.

Logs are structured `key=value` lines on **stderr**, filtered by `RUST_LOG` (default `info`).
Issue-related logs carry `issue_id` and `issue_identifier`; agent session lifecycle logs carry
`session_id`.

## Trust and safety posture

This implementation targets **trusted environments** and runs with a deliberately high-trust
configuration. It documents these selected behaviours:

| Decision | Selected behaviour |
| --- | --- |
| Command-execution approvals (`item/commandExecution/requestApproval`) | Auto-approved (`acceptForSession`) |
| File-change approvals (`item/fileChange/requestApproval`) | Auto-approved (`acceptForSession`) |
| Permission escalation (`item/permissions/requestApproval`) | Declined, keeping the agent inside its configured sandbox |
| User-input-required turns (`tool/requestUserInput`) | Hard failure of the run attempt |
| Unsupported dynamic tool calls | Structured tool failure; the session continues |
| Default `codex.approval_policy` | `never` |
| Default `codex.thread_sandbox` / `codex.turn_sandbox_policy` | `workspace-write` (network access disabled) |

Baseline containment that is always enforced:

- The coding agent runs with `cwd` equal to the per-issue workspace, inside `workspace.root`.
- Workspace directory names are sanitized to `[A-Za-z0-9._-]`.
- Platform permissions for the workspace root itself (dedicated OS user, restricted
  permissions, dedicated volume) are **not** managed by this service; see Section 15.2.

`linear_graphql` (Section 10.5) is advertised to the session and executes raw GraphQL using
Symphony's configured tracker credentials, so the agent never needs the API key on disk.

## Workspace lifecycle

- Workspaces live at `<workspace.root>/<sanitized issue identifier>` and are reused across runs.
- Successful runs **do not** delete workspaces.
- `after_create` runs only for newly created directories; `before_run` runs before every attempt;
  `after_run` runs after every attempt once the workspace exists; `before_remove` runs before
  deletion. `after_create`/`before_run` failures abort the attempt; `after_run`/`before_remove`
  failures are logged and ignored.
- Hook timeout is `hooks.timeout_ms` (default `60000`).
- Repository bootstrap (checkout, dependency install, code generation) is **not** built in; use
  the hooks (Section 9.3).
- A non-directory at the workspace path is a hard failure (workspace creation errors); it is never
  deleted or replaced.

## Configuration

`WORKFLOW.md` front matter supplies runtime settings; see Sections 5.3 and 6.4 for the full
schema and defaults. Notable implementation details:

- `tracker.api_key` accepts a literal token or `$VAR` / `${VAR}`. A `$VAR` that resolves to an
  empty string is treated as missing. The literal token is sent in the `Authorization` header
  (the form Linear uses for personal API keys).
- Path-valued settings (`workspace.root`) support `~` and `$VAR` expansion; `codex.command` is
  preserved verbatim as a shell command and never rewritten.
- Relative `workspace.root` values resolve against the directory containing `WORKFLOW.md`.
- `codex.stall_timeout_ms <= 0` disables stall detection.
- `server.port` enables the optional HTTP observability server; CLI `--port` overrides it.
  A value of `0` requests an ephemeral port. The listener binds loopback by default.

### Dynamic reload (Section 6.2)

`WORKFLOW.md` is checked for changes continuously (and again before each dispatch tick). A valid
change is re-applied to future dispatches, retries, reconciliation, hooks, and agent launches
without restart, including polling cadence, concurrency limits, active/terminal states, codex
settings, workspace root, hooks, and prompt content. In-flight sessions are not restarted.

An invalid reload is reported as an operator-visible error and the **last known good**
configuration stays in effect (this is the reload-time behaviour that takes precedence over the
"block dispatch" wording in Section 5.5; startup failures still fail startup).

`server.port` changes require a restart.

## Observability

- Structured logs on stderr (Section 13.1).
- Optional HTTP extension (Section 13.7) when `server.port` or `--port` is configured:
  - `GET /` — server-rendered dashboard (running sessions, retry queue, token totals, runtime)
  - `GET /api/v1/state` — runtime summary
  - `GET /api/v1/<issue_identifier>` — per-issue runtime detail, `404` when unknown
  - `POST /api/v1/refresh` — queues an immediate poll + reconciliation, `202 Accepted`
  - Unsupported methods return `405`; errors use `{"error":{"code","message"}}`
- No coding-agent session transcript files are written, so `logs.codex_session_logs` is empty.

## Restart recovery (Section 14.3)

Scheduler state is in memory only. After a restart the service re-polls the tracker, reuses
preserved workspaces, and removes workspaces for issues already in terminal states. Retry timers
and running sessions do not survive a restart.

## Extensions not implemented

- Appendix A (SSH worker execution) is not implemented; workers always run locally.
- Tracker writes stay with the agent (Section 11.5); `linear_graphql` is the escape hatch.
- Only the `linear` tracker kind is supported.
- No durable retry queue or session metadata persistence (Section 18.2 TODOs).

## Protocol note

The Codex app-server protocol is owned by the targeted Codex version. This client sends
`initialize` → `initialized`, `thread/start`, `thread/name/set`, `turn/start`, and `turn/interrupt`
over stdio JSONL, and consumes `thread/*`, `turn/*`, and `item/*` notifications. The
`item/tool/call` response envelope (text content plus an error flag) follows the targeted
version's contract and is isolated in `agent::tool_result` so it can be adjusted for a different
Codex version.

## Tests

```sh
cargo test -p symphony
```

The suite covers the Section 17 conformance matrix that is deterministic offline: workflow and
config parsing/defaults, `$VAR`/`~` resolution, prompt strictness, workspace safety and hooks,
tracker query construction and error mapping, dispatch sorting/eligibility, retry backoff,
stall detection, token accounting, snapshot shape, and the HTTP endpoints.

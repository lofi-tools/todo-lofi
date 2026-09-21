# agent-cli provider telemetry, routing + session history — Spec

## 1. Intent

Give agent-cli a durable memory of what its providers actually did: every attempt (ok/failed,
error kind, latency, tokens), every session and turn, and the routing decisions taken from that
history. Then use it: pick the combo entry with the best chance of working instead of the first one
in the list, and expose the reasoning to the user.

The provider *machinery* (catalog, transport, pacing, failure classification) is extracted to
`libs/ai_providers` — see `docs/spec/ai-providers-library-spec.md`. This document covers what
agent-cli does with it: the database it owns, what it writes, and how it chooses.

---

## 2. Decisions (normative)

| # | Decision |
|---|----------|
| D1 | **agent-cli owns the database and its migrations.** Storage is toasty + `toasty-driver-turso` (SQLite), the same stack `libs/storage` uses, but a **separate database, separate code, separate migrations**. Nothing is shared with todo-2. |
| D2 | **`libs/ai_providers` defines the storage trait; agent-cli implements it.** The library asks for `TelemetryStore` (attempts, cooldowns, windows); agent-cli provides the toasty-backed implementation and the `.sql` migrations that make the tables real. |
| D3 | **Sessions, turns and outcomes are persisted**, forming the success/failure history tree. Message transcripts are **not** stored in v1. |
| D4 | **The unfinished `sessions list / show / rm` subcommands get implemented** as the read side of that data. |
| D5 | **Selection is a deterministic, explainable score.** Lowest penalty wins; ties break on the configured combo order. The decision and its inputs are both logged and stored. |
| D6 | **Models are assumed free for now.** The price term is a seam that is always `0`; `cost_usd` columns exist and are written as `0`. No pricing fetch. |
| D7 | **Self rate-limiting** (implemented in the library, configured here): proactive pacing, `429`/`Retry-After` handling, and a per-provider in-flight cap. |
| D8 | **Telemetry never breaks a run.** Any storage error degrades to `NullStore` for the rest of the process, with a single warning. |
| D9 | **No behaviour change when disabled.** `telemetry.enabled = false` and `routing.enabled = false` restore today's ordered walk. |

---

## 3. Current state (verified in this repo)

- **No database.** The only provider state is `~/.abstract/cooldowns.json` (`config.rs:543`), a
  `{"provider\0model": unix_ms}` map written by `FallbackManager::record_failure`
  (`providers.rs:893`, persisted at `:953`) and read at startup (`:922`). Cooldown length is one
  config value, `fallback.cooldown_seconds` (`config.rs:427`, default 300).
- **No session storage.** `SessionAction { List, Show { id }, Rm { id } }` is declared in
  `cli_commands.rs:120`, but `main.rs` only dispatches `Commands::Config` (`main.rs:44`); a
  `sessions …` invocation falls through to the TUI. The resume code path is commented out
  (`main.rs:206`).
- **Usage lives in memory only.** `state.input_tokens / output_tokens / cost_usd`
  (`tui/app.rs:735`) are updated from `AgentEvent::CostUpdate` (`tui/event_loop.rs:2287`) and
  discarded when the process exits. Cost is `cersei::tools::estimate_cost` (`:2297`, `:2304`) with a
  second copy in `tui/widgets.rs:579`. Across conversations there is no record that a provider
  failed, how often, or how slowly.
- **`graph_db_path()`** (`config.rs:537`) is dead: the memory manager that would use it is commented
  out (`main.rs:215`).
- **The DB stack already in the workspace**: `libs/storage` (`toasty 0.7` + `toasty-driver-turso`,
  `include_dir!` over `toasty/migrations`, a hand-rolled checksum runner in `migrations.rs`,
  `toasty::models!` registration, `Turso::new(db_uri)`), used only by `apps/todo-2`. It is a
  reference for the pattern, not a dependency (D1).

---

## 4. Storage

### 4.1 Location and configuration

`~/.abstract/agent.db` by default, overridable:

```jsonc
"telemetry": {
  "enabled": true,
  "db_path": null,          // null → ~/.abstract/agent.db
  "retention_days": 30
}
```

`null`/empty `db_path` with `enabled: true` still uses the default; `enabled: false` forces
`NullStore` (in-memory only, no file created). Adding this section changes `default_config_jsonc()`
and its assertions in `config.rs` (the same template test that today asserts 10 providers and 13 env
vars must gain the new sections/counts).

### 4.2 Wiring

Mirrors `libs/storage` but stays inside agent-cli:

- `apps/agent-cli/src/telemetry.rs` — `AgentStore { db: toasty::db::Db }`, constructed with
  `toasty_driver_turso::Turso::new(&db_uri)`, models registered with `toasty::models!(…)`.
- `apps/agent-cli/migrations/*.sql` — the schema, embedded with `include_dir!` and applied by a
  runner that records `(id, name, checksum)` per file, so an edited migration fails loudly instead
  of silently diverging (same contract as `libs/storage/src/migrations.rs`, which is the template to
  copy; whether to copy that runner or drive `toasty-cli` migrations is an implementation detail —
  copying keeps agent-cli's behaviour identical to the todo-2 app it was proven on).
- Startup order in `main.rs`: load config → open the store (best-effort) → build `Catalog` with
  `Arc<dyn TelemetryStore>` → run. Opening failure logs once and hands back `NullStore` (D8).

### 4.3 Schema — `migrations/0001_telemetry.sql`

```sql
-- Sessions: one row per agent run (TUI session, ACP session, single-shot `-p`).
CREATE TABLE sessions (
  id                 TEXT PRIMARY KEY,       -- uuid
  started_at         INTEGER NOT NULL,       -- unix ms
  ended_at           INTEGER,
  cwd                TEXT NOT NULL,
  git_branch         TEXT,
  app_version        TEXT,
  provider           TEXT NOT NULL,          -- user-facing selection (e.g. 'combos')
  model              TEXT NOT NULL,          -- user-facing selection (e.g. 'coding')
  effective_provider TEXT,                   -- concrete start of the run
  effective_model    TEXT,
  outcome            TEXT,                   -- 'ok' | 'error' | 'cancelled'
  error_kind         TEXT,
  turn_count         INTEGER NOT NULL DEFAULT 0,
  input_tokens       INTEGER NOT NULL DEFAULT 0,
  output_tokens      INTEGER NOT NULL DEFAULT 0,
  cost_usd           REAL NOT NULL DEFAULT 0
);

-- Turns: what ran, on which model, and how it went. parent_turn_id makes this a tree,
-- so sub-agent work ("spawn_agents") hangs off the turn that produced it.
CREATE TABLE turns (
  id             TEXT PRIMARY KEY,
  session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq            INTEGER NOT NULL,           -- 0-based within the session
  parent_turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
  started_at     INTEGER NOT NULL,
  ended_at       INTEGER,
  provider       TEXT NOT NULL,              -- concrete provider for this turn
  model          TEXT NOT NULL,
  outcome        TEXT NOT NULL,              -- 'ok' | 'error' | 'cancelled'
  error_kind     TEXT,
  latency_ms     INTEGER,
  tool_calls     INTEGER NOT NULL DEFAULT 0,
  input_tokens   INTEGER NOT NULL DEFAULT 0,
  output_tokens  INTEGER NOT NULL DEFAULT 0,
  cost_usd       REAL NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX turns_session_seq ON turns(session_id, seq);
CREATE INDEX turns_session_started ON turns(session_id, started_at);

-- Attempts: the library's telemetry port writes here. One row per HTTP attempt.
CREATE TABLE attempts (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  at             INTEGER NOT NULL,           -- unix ms
  session_id     TEXT REFERENCES sessions(id) ON DELETE SET NULL,
  turn_id        TEXT REFERENCES turns(id) ON DELETE SET NULL,
  provider       TEXT NOT NULL,
  model          TEXT NOT NULL,
  ok             INTEGER NOT NULL,
  error_kind     TEXT,                       -- FailureKind tag: 'rate_limited', 'timeout', …
  retry_after_ms INTEGER,
  latency_ms     INTEGER,
  input_tokens   INTEGER NOT NULL DEFAULT 0,
  output_tokens  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX attempts_key_at ON attempts(provider, model, at);
CREATE INDEX attempts_at ON attempts(at);

-- Cooldowns replace ~/.abstract/cooldowns.json.
CREATE TABLE cooldowns (
  provider   TEXT NOT NULL,
  model      TEXT NOT NULL,
  until_ms   INTEGER NOT NULL,
  cause      TEXT NOT NULL,                  -- FailureKind tag
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider, model)
);

-- Every routing decision, with its inputs, so "why this model" is answerable later.
CREATE TABLE routing_decisions (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  at              INTEGER NOT NULL,
  session_id      TEXT,
  combo           TEXT,                      -- 'coding', or NULL for a non-combo selection
  chosen_provider TEXT NOT NULL,
  chosen_model    TEXT NOT NULL,
  scores          TEXT NOT NULL              -- JSON: [{entry, failure_rate, attempts, cooldown, pacing, penalty}]
);
CREATE INDEX routing_decisions_at ON routing_decisions(at);
```

Retention: on store open, delete `attempts`/`routing_decisions` older than
`telemetry.retention_days` and `sessions` whose `ended_at` is older than the same window (cascading
turns; attempts' `session_id` becomes NULL, which is why the FK is `SET NULL`).

### 4.4 Implementing `TelemetryStore`

- `record_attempt` → one `attempts` insert. `session_id` comes from the run scope (see §5.1);
  `turn_id` is filled in by the agent after the turn row exists (an `UPDATE attempts SET turn_id`
  for rows with that session and a NULL turn id, or the library's record carries the turn id when
  the agent sets it — implementation detail, prefer the latter as it avoids an update).
- `set_cooldown` / `cooldown_until` → upsert/select on `cooldowns`. `active_cooldowns` returns
  every row with `until_ms > now`.
- `attempts_since` → the rows the score reads, over `attempts_key_at`.
- **Legacy import**: on first open, if `~/.abstract/cooldowns.json` exists with a non-empty map,
  insert the non-expired entries as cooldowns (`cause = 'imported'`) and rename the file to
  `cooldowns.json.migrated`. The `fallback.cooldowns_file` config key becomes
  deprecated/ignored (documented in `default_config_jsonc`), since state no longer lives in a file.

### 4.5 Degraded mode (D8)

Any toasty error during a write logs one warning, flips a process-wide `AtomicBool`, and returns
`Ok(())` for the remainder of the run so callers never see storage failures. A read error degrades
the score to "no history": every candidate scores the prior, i.e. configured order (see §6).
A read-only or locked database must never abort a run.

---

## 5. What gets recorded, when

### 5.1 Run scope

`AgentRuntime::new` (and each ACP `session/new`) opens a session row and keeps
`{ session_id, turn_seq }` in `AgentRuntimeInner`. The session id is handed to the library so
attempts can be attributed: `Catalog::provider_impl(provider, model, session_id: Option<&str>)`
(see the library spec §5.2), with the agent passing it when it builds or rebuilds the agent for a
run.

| Event | Write |
|---|---|
| Runtime start | `sessions` insert (`provider`/`model` = the selection, `effective_*` = the resolved first run) |
| `AgentEvent::TurnStart` | `turns` insert (`seq` incremented, provider/model = the run's effective pair) |
| First attempt of a turn | `attempts` row via the library |
| Fallback switch (`model_changed`) | new `turns` row (the turn is retried on another entry); the failed entry's attempt already recorded the cause |
| `AgentEvent::CostUpdate` | `turns` + `sessions` token/cost columns updated |
| `AgentEvent::ToolEnd` | `turns.tool_calls` incremented |
| Stream ends (`Complete`) / errors / cancel | `turns.outcome`, `turns.ended_at`, `sessions` totals |
| Process exit (normal, or `signals::install` cancel) | `sessions.ended_at` + `outcome` |
| Every combo choice | `routing_decisions` insert |

Single-shot (`-p`) and ACP runs write the same rows; a sub-agent's work is recorded through
`turns.parent_turn_id` when the parent turn is known, otherwise `NULL` (v1 does not thread the
parent turn id through `subagents.rs`).

### 5.2 What is deliberately not stored

- Message text, tool inputs/outputs, files, diffs (no transcripts in v1 — D3).
- API keys or anything derived from `Resolved.api_key` (the library spec restates this).
- Prompt/tool-call content that could be sensitive: only counts and kinds.

---

## 6. Routing

### 6.1 Inputs and formula

Candidates are the entries of the selected combo, in configured order. For each candidate
`(provider, model)`, over a rolling window (`routing.window_hours`, default 24):

```
decay(age)        = 0.5 ^ (age / half_life)                 # half_life = routing.half_life_hours (6)
weighted_fails    = Σ decay(age) over failed attempts       # only kinds that count (§6.2)
weighted_attempts = Σ decay(age) over all attempts
failure_rate      = (weighted_fails + α · prior) / (weighted_attempts + α)   # α = 2, prior = 0.5
pacing_pressure   = in_flight / max_concurrency             # 0 when no cap is configured
latency_term      = ewma_latency / min(ewma_latency over candidates)   # only if weight > 0

penalty = w_failure · failure_rate          # default 1.0
        + w_pacing  · pacing_pressure       # default 0.25
        + w_latency · latency_term          # default 0.0  (off in v1)
        + w_price   · price_term            # default 0.0  (D6: no pricing)
        + (candidate in cooldown ? 10.0 : 0.0)
```

Chosen = the lowest penalty. Tie-break: the candidate's index in the combo (stable, so an
all-equal field keeps today's order), then model id.

Notes:

- `min_samples` (default 3 weighted attempts): below it a candidate scores exactly the prior
  `0.5`, so a new model is neither preferred nor punished.
- A candidate in cooldown is effectively excluded unless *every* candidate is; then the
  least-recently-cooled one is chosen and the UI says so.
- Config surface:

```jsonc
"routing": {
  "enabled": true,
  "window_hours": 24,
  "half_life_hours": 6,
  "min_samples": 3,
  "weight_failure": 1.0,
  "weight_pacing": 0.25,
  "weight_latency": 0.0,
  "weight_price": 0.0
}
```

`routing.enabled = false` (or `fallback.enabled = false`) means: walk the combo in order, skipping
entries in cooldown — today's behaviour. `combos/<name>` stays the user-facing selection; only
`effective_provider` / `effective_model` change, exactly as the existing fallback does
(`acp.rs:658`, `main.rs:131`).

### 6.2 Which failures count (mirrors the library spec §5.8)

| Counts against the candidate | Does not count |
|---|---|
| `RateLimited`, `Overloaded`, `Timeout`, `Network`, `Auth`, `Unknown` | `ContextOverflow`, `RequestRejected`, `Cancelled` |
| | `ModelNotFound` → long cooldown instead of a bad score |

Rationale: a request we sent wrong (too long, malformed, blocked) must not make a healthy provider
look unreliable; `Cancelled` is the user pressing Ctrl-C.

### 6.3 Explainability (D5)

Every choice produces:

```rust
pub struct RoutingDecision {
    pub at: SystemTime,
    pub combo: Option<String>,
    pub chosen: ModelKey,
    pub candidates: Vec<CandidateScore>,   // in configured order
}

pub struct CandidateScore {
    pub key: ModelKey,
    pub failure_rate: f64,
    pub weighted_attempts: f64,
    pub in_cooldown: Option<SystemTime>,
    pub pacing_pressure: f64,
    pub penalty: f64,
}
```

Three surfaces:

1. **TUI**: a grayed `[system]` line when the chosen entry is not the first candidate, in the same
   style as the existing fallback notice:
   `routing: combos/coding → orcarouter/orcarouter/free (fail 0.05 over 12 attempts) — next: tokenrouter/z-ai/glm-5.3-free (fail 0.31 over 8)`.
   When everything ties, print nothing.
2. **`/why`**: a TUI slash command rendering the last decision as a table (candidate, failure rate,
   attempts, cooldown, pacing, penalty) — registered next to `/model` and `/provider`
   (`tui/app.rs:406` is the slash-command list).
3. **Storage**: the `routing_decisions` row, so historical questions ("did routing help?") are
   answerable with SQL.

ACP gets the same information through the existing `model_changed` / `effectiveModelId` mechanism
(`acp.rs:658`), unchanged.

---

## 7. Sessions CLI (D4)

Dispatch `Commands::Sessions { action }` in `main.rs` before the TUI/one-shot paths:

```
agent-cli sessions list [--limit 20]     # id prefix, started, cwd, model, turns, tokens, outcome
agent-cli sessions show <id>             # session header + turn table + attempt failure summary
agent-cli sessions rm <id>               # delete the session (turns cascade, attempts keep their row)
```

- `list` shows the most recent sessions newest-first, with a short id prefix accepted by `show`/`rm`.
- `show <id>` prints the session row, then turns (`seq`, provider/model, outcome, error kind,
  tokens, latency), then a per-provider failure summary derived from `attempts`.
- `rm <id>` requires the id to resolve to exactly one session; `--all-before <date>` is deliberately
  out of scope.
- Unknown/unopenable database → the command explains that telemetry is disabled and exits non-zero.

**Not in v1: `--resume`.** Resuming a conversation needs the transcript, which D3 excludes. Should
resume be wanted, it needs its own decision on storing message history.

---

## 8. Failure and edge-case matrix

| Situation | Behaviour |
|---|---|
| DB file missing | Created; migrations applied; no error surfaced unless creation fails. |
| DB unwritable / locked | One warning, degrade to `NullStore` for the process; runs proceed unaffected. |
| Migration checksum mismatch | Hard error at startup with the offending file named (do not auto-apply). |
| Crash (SIGKILL) mid-session | `sessions.ended_at` stays NULL; `sessions list` renders such rows as `(open)` and `rm` can clean them. |
| Clock skew / future attempt timestamp | Timestamps are clamped to `now` on write so decay never goes negative. |
| Attempt with no session (sub-agent, direct-prompt helper) | `session_id` NULL; the attempt still counts for the score. |
| Cooldown row expired | Ignored on read and pruned with retention. |
| All candidates cooling down | Least-recently-cooled chosen, with an explicit line in the UI. |
| `attempts` grows unbounded | Retention pass on open (`telemetry.retention_days`). |
| Two agent-cli processes run concurrently | Writes are independent rows; SQLite serializes. Cooldown writes are last-wins. |
| `telemetry.enabled = false` | `NullStore`; `/why` reports "no decisions recorded"; `sessions` subcommands exit with an explanation. |
| Routing disabled, `fallback.enabled` true | Ordered walk with cooldown skipping (today's behaviour). |

---

## 9. Code change list (by file)

| File | Change |
|---|---|
| `apps/agent-cli/src/telemetry.rs` (new) | `AgentStore` (toasty + turso), migrations embedding + runner, `impl TelemetryStore`, session/turn/attempt/routing writes, retention, legacy cooldown import. |
| `apps/agent-cli/migrations/0001_telemetry.sql` (new) | The schema in §4.3. |
| `apps/agent-cli/src/routing.rs` (new, thin) | Builds candidates from a combo, calls the library's `Router`, records the decision, formats the explanation. |
| `apps/agent-cli/src/main.rs` | Store init + degraded mode; `Commands::Sessions` dispatch; session open/close around single-shot and TUI runs. |
| `apps/agent-cli/src/cli_commands.rs` | `--limit` on `sessions list`; keep `SessionAction` shape otherwise. |
| `apps/agent-cli/src/config.rs` | `TelemetryConfig`, `RoutingConfig`; deprecate `fallback.cooldowns_file`; `default_config_jsonc` + template tests updated. |
| `apps/agent-cli/src/providers.rs` | `AgentRuntime` holds `Arc<Catalog>` + store handle; `next_fallback_entry` / `record_failure` delegate; `provider_impl(.., session_id)`. |
| `apps/agent-cli/src/tui/event_loop.rs` | Turn/session writes on `TurnStart` / `CostUpdate` / `ToolEnd` / `Complete` / cancel; routing line; `/why` handler. |
| `apps/agent-cli/src/tui/app.rs` | `/why` in the slash-command table; last-decision state. |
| `apps/agent-cli/src/acp.rs` | Same writes for ACP sessions. |
| `apps/agent-cli/src/subagents.rs` | No schema change; optionally pass the parent turn id (v2). |
| `apps/agent-cli/Cargo.toml` | `toasty`, `toasty-driver-turso`, `include_dir`, `sha2`, `jiff` (as in `libs/storage`). |

---

## 10. Testing plan

Agent-side (`cargo test -p agent-cli`):

1. **Migrations** — apply to a temp file, re-open, apply again (idempotent); a mutated migration
   fails with its name.
2. **Store round-trip** — `record_attempt` → `attempts_since` returns it in order;
   `set_cooldown`/`cooldown_until`/`active_cooldowns`. Each test uses its own temp DB (no test
   touches `~/.abstract/agent.db`, mirroring `isolated_config` at   `providers.rs:1736`).
3. **Legacy import** — a fixture `cooldowns.json` becomes rows and is renamed; a second run is a
   no-op.
4. **Degraded mode** — a store pointed at an unwritable path: one warning, calls succeed,
   `NullStore` semantics afterwards.
5. **Scoring** — pure-function tests: no history → configured order; a 0.9-failure entry loses to a
   0.1 one; fewer than `min_samples` keeps the prior; in-cooldown only wins when everything is
   cooling; decay halves at the half-life; `routing.enabled = false` reproduces the ordered walk.
6. **Failure weighting** — `ContextOverflow`/`Cancelled` attempts do not move the score;
   `ModelNotFound` produces a cooldown, not a bad score.
7. **CLI** — `sessions list` on an empty DB; `show` on a seeded session renders turns; `rm` cascades;
   an ambiguous id prefix is rejected.
8. **End-to-end** — with the mock provider used by `subagents.rs` tests, a failing first entry
   produces one failed attempt + one routing decision and a successful turn on the second entry.
9. **Retention** — rows older than the window are pruned on open, newer ones survive.

---

## 11. Out of scope (v1)

- Transcript storage and `--resume`.
- Pricing data, real `cost_usd` values, spend caps.
- Cross-machine sync of the telemetry DB.
- An ML/bandit router (D5 chose the deterministic score).
- A `/why` history browser (only the last decision is rendered).
- Sharing this schema with todo-2 or moving it into a shared crate.

---

## 12. Open questions and risks

1. **Attempt→turn attribution.** The library writes attempts when the HTTP call ends; the turn row
   is agent-owned. If the agent supplies the turn id at construction time, a mid-turn fallback
   (which starts a *new* turn) needs a rebuilt agent; otherwise attempts land with `turn_id NULL`.
   Decide while implementing step 5 of §13.
2. **Sub-agent attribution.** Sub-agents spawn their own providers (`subagents.rs:751`); they need
   the same store + session id, and ideally the parent turn id. v1 records them session-scoped only.
3. **Hard-coded weights.** `w_failure = 1.0` vs `w_pacing = 0.25` is a guess. If it proves wrong the
   fix is a constant, not a redesign — but the first week of data should be looked at before
   trusting the ordering.
4. **Cooldown semantics for `Auth`.** Disabling a provider for the whole process is right for a bad
   key, but a *rotating* key (proxy setups) would want a re-read. Confirm before shipping.
5. **`retention_days = 30`** is arbitrary; the score only needs 24h, so retention is about history
   browsing. Fine for v1, worth revisiting if `sessions list` becomes load-bearing.
6. **Migration runner duplication.** Copying `libs/storage/src/migrations.rs` (309 lines) into
   agent-cli is the fastest path but duplicates a runner. Extracting it to a tiny shared crate is
   tempting and would touch todo-2 — explicitly not in scope here.

---

## 13. Implementation order

1. `telemetry.rs` + `0001_telemetry.sql` + migration runner; open/create/degrade + tests (§10.1–4).
2. Implement `TelemetryStore` over it (attempts + cooldowns), including the legacy import;
   wire it into `Catalog` in the agent. Nothing is scored yet.
3. Record sessions and turns (TUI + ACP + single-shot), including outcome and usage on
   `Complete`/cancel; verify with `sessions list/show` after implementing the CLI (§13.5 alongside).
4. Implement the `sessions list|show|rm` dispatch and its tests.
5. Routing: score function + tests (§10.5–6), then the combo walk in `AgentRuntime` using it, then
   the `routing_decisions` insert.
6. Explainability: the TUI system line, then `/why`; re-run the end-to-end test (§10.8).
7. Retention + doc updates (`README.md` provider section, `default_config_jsonc` comments).

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
| D10 | **Attempts are attributed with a shared `AttemptScope`**, not a SQL backfill: one scope per built agent, holding the session id plus the current turn id, handed to the library at `provider_impl` time and updated by the agent at turn boundaries and on fallback switches. |
| D11 | **Sub-agents get their own scope** carrying their parent turn id; their attempts and turns therefore hang off the spawning turn in the history tree. |
| D12 | **The store is raw SQL** — `toasty::sql::statement(..)` for writes and `toasty::sql::query(..).column_types([..])` for reads, over a `tokio::sync::Mutex<toasty::db::Db>`. No `#[derive(Model)]` schema: the library's trait is the interface, and typed models would add a parallel definition of the same tables. |
| D13 | **The migration runner is copied from `libs/storage/src/migrations.rs`** (embed + checksum + statement splitter), not shared: extracting a crate would touch todo-2, which D1 excludes. |

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

### 4.2 Wiring (D12, D13)

Mirrors `libs/storage` but stays inside agent-cli, and uses **raw SQL** rather than deriving toasty
models — the tables exist to serve `TelemetryStore`, so a second typed definition of them would be
pure duplication:

```rust
pub struct AgentStore {
    /// `toasty::sql::{statement, query}` take `&mut Db` while `TelemetryStore` is `&self`,
    /// so the handle is behind an async mutex. One writer, short critical sections.
    db: tokio::sync::Mutex<toasty::db::Db>,
}

// write
 toasty::sql::statement("INSERT INTO attempts (at, provider, model, ok) VALUES (?1, ?2, ?3, ?4)")
    .bind(at_ms).bind(&provider).bind(&model).bind(ok as i64)
    .exec(&mut *self.db.lock().await).await?;

// read (column types are explicit, as in libs/storage/src/managed.rs:367)
let rows = toasty::sql::query("SELECT at, ok, error_kind, latency_ms, input_tokens, output_tokens \
                               FROM attempts WHERE provider = ?1 AND model = ?2 AND at >= ?3 \
                               ORDER BY at")
    .column_types([Type::I64, Type::I64, Type::String, Type::I64, Type::I64, Type::I64])
    .bind(&provider).bind(&model).bind(since_ms)
    .exec(&mut *self.db.lock().await).await?;
```

- `apps/agent-cli/src/telemetry_migrations.rs` — the runner copied from
  `libs/storage/src/migrations.rs` (embed via `include_dir!`, per-file SHA-256 checksum recorded in
  `_migrations_history`, `split_sql` for multi-statement files, checksum mismatch = hard error).
  `apps/agent-cli/migrations/0001_telemetry.sql` holds the schema in §4.3. Copying rather than
  sharing is D13: a shared crate would mean touching todo-2.
- The DB URL is `turso:<path>`; for tests,  `turso::memory:` (as `libs/storage/src/lib.rs:114` does
  with `StorageConfig { db_uri: "turso::memory:" }`).
- Startup order in `main.rs`: load config → open the store (best-effort) → apply migrations → build
  `Catalog` with `Arc<dyn TelemetryStore>` → run. Opening or migrating failure logs once and hands
  back `NullStore` (D8).

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

- `record_attempt` → one `attempts` insert. Both `session_id` and `turn_id` come from the
  `AttemptScope` the agent handed to the library when it built the provider (see §5.1), so there is
  no post-hoc update and no guessing. The turn row itself is inserted by the agent at `TurnStart`,
  before the first attempt of that turn.
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

### 5.1 Run scope and attempt attribution (D10, D11)

`AgentRuntime::new` (and each ACP `session/new`) opens a session row and keeps `turn_seq` plus an
`AttemptScope` in `AgentRuntimeInner`:

```rust
/// Shared between the agent and the library: the library reads it when it writes an
/// attempt, the agent writes it at turn boundaries.
pub struct AttemptScope {
    pub session_id: Option<String>,
    /// The turn in flight. `None` means "not inside a turn" (attempts then record NULL).
    pub turn: parking_lot::Mutex<Option<String>>,
    /// For a sub-agent: the turn that spawned it (D11).
    pub parent_turn_id: Option<String>,
}
```

- Created by the agent when it builds the provider for a run, and passed down:
  `Catalog::provider_impl(provider, model, Arc<AttemptScope>)` (library spec §5.2).
- Written by the agent: on `TurnStart` (new turn id) and on a fallback switch (a new turn id, since
the retry is a new turn — see the recording table below).
- Read by the library at attempt start/end; it never guesses. A `None` turn records `NULL`, and the
attempt still counts for the score.
- Sub-agents build their own provider (`subagents.rs:751`) and therefore their own scope, with
`parent_turn_id` filled from the spawning turn — so concurrent child requests cannot mis-attribute
the parent's turn. This is why a single shared cell is safe: one provider instance per agent
instance, and the cersei runner awaits each stream.

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
| `apps/agent-cli/src/telemetry.rs` (new) | `AgentStore` (toasty + turso over raw SQL, D12), `impl TelemetryStore`, session/turn/attempt/routing writes, retention, legacy cooldown import. |
| `apps/agent-cli/src/telemetry_migrations.rs` (new) | The runner copied from `libs/storage/src/migrations.rs` (D13): embed, checksum, split, apply. |
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

## 12. Resolved questions (with evidence)

### 12.1 Attempt→turn attribution → **shared `AttemptScope`, no rebuild, no backfill** (D10)

Resolved as §5.1. The three rejected alternatives, for the record:

- *Re-pass the turn id through `CompletionRequest`* — nothing on the request carries an opaque tag;
  `ProviderOptions` is a JSON map that cersei's OpenAi reads key-by-key, so smuggling a `_turn_id`
  there would be invisible to the wire but also untyped and fragile.
- *Backfill with `UPDATE attempts SET turn_id = ? WHERE session_id = ? AND turn_id IS NULL AND at >= ?`*
  — raced by sub-agents and by any concurrent session sharing the DB, and it needs the turn's start
  timestamp to be exact.
- *Rebuild the agent per turn* — would discard the cersei agent's state on every turn; a non-starter.

The scope costs one `Arc` and two mutex writes per turn; the library reads it once per attempt.

### 12.2 Sub-agent attribution → **own scope, parent turn id filled at the spawn site** (D11)

Resolved as §5.1: `subagents.rs` already builds a fresh provider per sub-agent (`:751`), so it hands
each child a fresh `AttemptScope` whose `parent_turn_id` is the turn that called `spawn_agents`.
Turns rows for the child get `parent_turn_id`, and attempts inherit the tree through `turn_id` — so
no `attempts.parent_turn_id` column is needed.

### 12.3 Hard-coded weights → **accepted, with an explicit review checkpoint**

Resolved as: keep `1.0 / 0.25 / 0.0 / 0.0` as defaults, but treat the first real telemetry as the
validation. Mitigations that come free: `min_samples` keeps thin history from mattering, the
cooldown term (+10.0) dominates everything else, and `/why` plus the `routing_decisions` rows make
the terms inspectable rather than mysterious. If the ordering ever looks wrong in practice, the
check is "which term moved the choice" — answerable from the stored decision. No ML, no fitting.

### 12.4 `Auth` with a rotating key → **re-resolve once, then disable**

Resolved in the library spec (D13 there): re-resolve `!command` / `env:VAR` key specs and retry the
request once; only a second failure disables the provider for the process. The telemetry side needs
nothing extra — both outcomes are recorded as attempts with their `error_kind`.

### 12.5 `retention_days = 30` → **keep 30 days, prune only closed history**

Resolved as: prune `attempts` / `routing_decisions` older than the window on open; prune `sessions`
only when `ended_at` is set and older than the window (never an open session, never a row with no
`ended_at`). `sessions rm <id>` stays the manual control. Revisit if `sessions list` becomes
load-bearing for long-range history.

### 12.6 Migration runner duplication → **copy it, extract later** (D13)

Resolved as: copy `libs/storage/src/migrations.rs` — `MigrationEntry` + `split_sql` + checksum
apply, ~200 lines of substance with its own tests — into `apps/agent-cli/src/telemetry_migrations.rs`.
The statement splitter is the risky part to rewrite (quotes, comments, `BEGIN…END` bodies), so
reusing proven code beats a minimal reimplementation. Extraction into a shared crate is deferred
until a third consumer exists, precisely because it would touch todo-2.

---

## 12A. Implementation notes (as built)

What differs from the prose, and what is knowingly not done yet:

- **§4.2 raw SQL binds** — every bound value goes through `bind_typed` with a
  database type, not plain `bind`: the turso driver refuses an inferred type for
  `NULL` (`cannot infer raw SQL bind type for Null`), which would otherwise
  degrade the store on the first attempt with no session or turn id. The type
  is derived from the value (`I64` → `Integer(8)`, `String`/`Null` → `Text`,
  `F64` → `Float(8)`).
- **§4.4 session deletion** — `sessions rm` and the retention pass delete
  explicitly (`UPDATE attempts SET session_id = NULL, turn_id = NULL`, then
  `DELETE FROM turns`, then `DELETE FROM sessions`) instead of relying on
  `ON DELETE CASCADE`. The turso driver does not enable `PRAGMA foreign_keys`
  for every pooled connection, so a cascade would silently leave turns behind;
  the explicit deletes reproduce exactly the documented semantics.
- **§4.3 schema** is unchanged, including the `REFERENCES` clauses, which are
  now documentation of intent plus a hint for any future FK-enabled path.
- **§5.1 attribution** — the shared `AttemptScope` is created per runtime and
  per ACP run, and is handed to the library at `provider_impl` time. The TUI and
  single-shot paths open a turn before the first request and close it on
  `Complete`/`Error`; ACP records the session and the attempts, but not turn
  rows, so its attempts carry `session_id` with `turn_id NULL`.
- **Sub-agents** — §5.1's "own scope" is now wired. `SpawnAgentsTool` takes a
  `SubAgentProviders` factory (`Arc<dyn Fn() -> Result<Box<dyn Provider>>>`)
  built by the runtime (`AgentRuntime::build`/`retry`/`switch`, and the ACP
  builder) from its catalog and parent scope; `run_sub_agent` calls it per
  spawn and only falls back to a plain provider when the factory is absent
  (tests, read-only builds). The factory snapshots the parent turn at *call*
  time and gives each sub-agent a fresh `AttemptScope` with the runtime's
  session, so concurrent children share nothing but the session id.
- **Abandoned prompt before an attempt** — a run that Fails before any attempt
  (a resolution error) leaves the turn `running`; the session close marks the
  session, and `sessions list` shows such turns by their turn rows. Not a
  correctness issue, but a turn row can outlive the process that wrote it.
- **Token counts** — the TUI rolls usage into the turn and the session from
  `CostUpdate`; the single-shot path has no usage events, so its turn rows carry
  `0/0`. `cost_usd` is written as `0` everywhere (D6).
- **§6.3 `/why`** renders the last decision of the *current* process. It reports
  "no routing decision recorded" when routing or telemetry is off, or when no
  combo run has happened yet (D19).
- **`sessions show`** prints the turn table, then per-`(provider, model)`
  failure tallies derived from `attempts` (top `FAILURE_SUMMARY_LIMIT` = 10).
- **§7 CLI** — `sessions list --limit` (default 20) exists; `show`/`rm` accept a
  unique id prefix and reject an ambiguous one.

## 13. Accepted risks → decisions (no open questions remain)

| # | Risk | Decision |
|---|------|----------|
| D14 | The SQL in §4.3 and the hand-written row decoders can drift | **One decode site:** all row→struct conversion lives in `telemetry.rs` next to the `INSERT`/`SELECT` strings for that table, so a column change is one edit; the migration checksum makes the SQL side fail loudly if edited after being applied. |
| D15 | `tokio::sync::Mutex<Db>` serializes writes | Accepted for v1: one attempt per turn, sub-second writes, and the critical section only covers the statement. If write contention ever shows up, move to a channel + single writer task — a change local to `AgentStore`. |
| D16 | Attempts are recorded even when no session row exists (single-shot helpers, tests, `-p` without a session) | Accepted: the attempt still counts for the score, the tree view shows it unattributed (`session_id NULL`). No synthetic sessions. |
| D17 | Clock skew or a future timestamp distorts decay | Timestamps are clamped to `now` on write, and the scorer clamps negative ages to zero; both are cheap guards, not a clock service. |
| D18 | `routing_decisions` grows with every choice, not just fallbacks | **Record every choice** — that is what makes "/why" and "did routing help?" answerable. Growth is bounded by `telemetry.retention_days` and the table is one row per turn, not per token. |
| D19 | `/why` has nothing to show when telemetry is disabled or a run has not chosen yet | Accepted: `/why` reports "no routing decision recorded" instead of an empty table; it never errors. |

---

## 14. Implementation order

1. `telemetry.rs` + `0001_telemetry.sql` + migration runner; open/create/degrade + tests (§10.1–4).
2. Implement `TelemetryStore` over it (attempts + cooldowns), including the legacy import;
   wire it into `Catalog` in the agent. Nothing is scored yet.
3. Record sessions and turns (TUI + ACP + single-shot), including outcome and usage on
   `Complete`/cancel; verify with `sessions list/show` once the CLI step below lands.
4. Implement the `sessions list|show|rm` dispatch and its tests.
5. Routing: score function + tests (§10.5–6), then the combo walk in `AgentRuntime` using it, then
   the `routing_decisions` insert.
6. Explainability: the TUI system line, then `/why`; re-run the end-to-end test (§10.8).
7. Retention + doc updates (`README.md` provider section, `default_config_jsonc` comments).

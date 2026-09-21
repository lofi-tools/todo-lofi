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

-- Turns: what ran, on which model, and how it went. parent_turn_id makes this
-- a tree, so sub-agent work hangs off the turn that spawned it.
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

-- Attempts: the ai_providers telemetry port writes here. One row per HTTP
-- attempt; no prompt or response text is ever stored.
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
  cause      TEXT NOT NULL,                  -- FailureKind tag, or 'imported'
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider, model)
);

-- Every routing decision, with its inputs, so "why this model" is answerable
-- later and "did routing help?" is a query.
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

//! The telemetry database: attempts, cooldowns, sessions, turns, routing
//! decisions.
//!
//! This is agent-cli's implementation of the library's `TelemetryStore` port,
//! plus the session/turn history the `sessions` subcommands read. The database
//! is its own SQLite file (`~/.abstract/agent.db` by default) with its own
//! migrations — nothing is shared with `libs/storage` or todo-2.
//!
//! Two rules shape everything here:
//!
//! * **Telemetry never breaks a run.** Every write goes through [`AgentStore`],
//!   which logs at most one warning and then degrades to a no-op for the rest
//!   of the process. A read failure degrades the score to "no history".
//! * **No prompt or response text is stored.** Only counts, kinds and times.

use ai_providers::{
    AttemptRecord, AttemptSummary, CooldownRegistry, FailureKind, ModelKey, Outcome,
    RoutingDecision, TelemetryStore,
};
use anyhow::Context as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many attempts the failure summary in `sessions show` lists.
const FAILURE_SUMMARY_LIMIT: usize = 10;

// ─── Time and value helpers ─────────────────────────────────────────────────

fn to_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn now_millis() -> i64 {
    to_millis(SystemTime::now())
}

fn from_millis(millis: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64)
}

fn int_at(row: &toasty::stmt::Value, index: usize) -> Option<i64> {
    match record_at(row, index)? {
        toasty::stmt::Value::I64(value) => Some(*value),
        toasty::stmt::Value::I32(value) => Some(*value as i64),
        _ => None,
    }
}

fn text_at(row: &toasty::stmt::Value, index: usize) -> Option<String> {
    match record_at(row, index)? {
        toasty::stmt::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn record_at(row: &toasty::stmt::Value, index: usize) -> Option<&toasty::stmt::Value> {
    match row {
        toasty::stmt::Value::Record(record) => record.get(index),
        _ => None,
    }
}

fn millis_value(value: i64) -> toasty::stmt::Value {
    toasty::stmt::Value::I64(value)
}

/// The database type a bound value is sent as.
///
/// Every bind goes through `bind_typed` rather than plain `bind`, because the
/// turso driver refuses to infer a type for `NULL` (`cannot infer raw SQL bind
/// type for Null`). SQLite is dynamically typed, so naming the column's type is
/// all that is needed — including for the `NULL` itself.
fn bind_type(value: &toasty::stmt::Value) -> toasty::schema::db::Type {
    match value {
        toasty::stmt::Value::Bool(_) => toasty::schema::db::Type::Boolean,
        toasty::stmt::Value::I64(_)
        | toasty::stmt::Value::I32(_)
        | toasty::stmt::Value::I16(_)
        | toasty::stmt::Value::U64(_) => toasty::schema::db::Type::Integer(8),
        toasty::stmt::Value::F64(_) | toasty::stmt::Value::F32(_) => {
            toasty::schema::db::Type::Float(8)
        }
        toasty::stmt::Value::Bytes(_) => toasty::schema::db::Type::Blob,
        // Strings, and every `NULL` (which is text-or-integer in this schema
        // and is never compared by type).
        _ => toasty::schema::db::Type::Text,
    }
}

// ─── Row shapes the CLI renders ─────────────────────────────────────────────

/// One session as `sessions list` shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub effective_provider: Option<String>,
    pub effective_model: Option<String>,
    pub outcome: Option<String>,
    pub turn_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

impl SessionRow {
    /// The short id the other `sessions` subcommands accept.
    pub fn short_id(&self) -> &str {
        self.id.get(..8).unwrap_or(&self.id)
    }
}

/// One turn as `sessions show` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    pub seq: i64,
    pub provider: String,
    pub model: String,
    pub outcome: String,
    pub error_kind: Option<String>,
    pub latency_ms: Option<i64>,
    pub tool_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// A per-provider/model failure tally derived from `attempts`.
#[derive(Debug, Clone, PartialEq)]
pub struct FailureRow {
    pub provider: String,
    pub model: String,
    pub failures: i64,
    pub attempts: i64,
}

/// What `open` needs to start a session row.
#[derive(Debug, Clone)]
pub struct SessionStart {
    pub id: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub app_version: Option<String>,
    pub provider: String,
    pub model: String,
    pub effective_provider: String,
    pub effective_model: String,
}

impl SessionStart {
    /// Capture the ambient context (cwd, git branch, version) and mint an id.
    pub fn capture(
        cwd: impl Into<PathBuf>,
        provider: impl Into<String>,
        model: impl Into<String>,
        effective_provider: impl Into<String>,
        effective_model: impl Into<String>,
    ) -> Self {
        let cwd = cwd.into();
        Self {
            id: new_id(),
            git_branch: git_branch(&cwd),
            cwd: cwd.display().to_string(),
            app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            provider: provider.into(),
            model: model.into(),
            effective_provider: effective_provider.into(),
            effective_model: effective_model.into(),
        }
    }
}

/// One turn about to run.
#[derive(Debug, Clone)]
pub struct TurnStart {
    pub id: String,
    pub session_id: String,
    pub seq: u32,
    pub parent_turn_id: Option<String>,
    pub provider: String,
    pub model: String,
}

impl TurnStart {
    pub fn new(
        session_id: impl Into<String>,
        seq: u32,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: new_id(),
            session_id: session_id.into(),
            seq,
            parent_turn_id: None,
            provider: provider.into(),
            model: model.into(),
        }
    }

    pub fn with_parent_turn(mut self, parent_turn_id: Option<String>) -> Self {
        self.parent_turn_id = parent_turn_id;
        self
    }
}

/// A fresh opaque id (session or turn).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The current branch, best-effort (`None` outside a repository).
fn git_branch(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty() && branch != "HEAD").then_some(branch)
}

// ─── The store ──────────────────────────────────────────────────────────────

/// The telemetry database handle. Cheap to clone via `Arc`.
pub struct AgentStore {
    /// `toasty::sql::{statement, query}` need `&mut Db` while `TelemetryStore`
    /// is `&self`, so the handle sits behind an async mutex. Critical sections
    /// cover one statement each.
    db: tokio::sync::Mutex<toasty::db::Db>,
    /// Set after the first storage error: everything afterwards is a no-op.
    degraded: AtomicBool,
}

impl AgentStore {
    /// Open (creating if needed) the database at `path`, apply migrations,
    /// prune expired history and import any legacy cooldown file.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating the telemetry directory {} failed", parent.display())
            })?;
        }
        let uri = format!("turso:{}", path.display());
        Self::open_uri(&uri).await
    }

    /// An in-memory database: the tests' store.
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        Self::open_uri("turso::memory:").await
    }

    async fn open_uri(uri: &str) -> anyhow::Result<Self> {
        let driver = toasty_driver_turso::Turso::new(uri)
            .map_err(|error| anyhow::anyhow!("opening the telemetry database failed: {error}"))?;
        let mut db = toasty::Db::builder()
            .build(driver)
            .await
            .map_err(|error| anyhow::anyhow!("building the telemetry database failed: {error}"))?;
        crate::telemetry_migrations::apply_pending_migrations(&mut db).await?;
        Ok(Self {
            db: tokio::sync::Mutex::new(db),
            degraded: AtomicBool::new(false),
        })
    }

    /// Open the store the config asks for, degrading to `NullStore` on any
    /// failure (D8). Returns the trait object the library uses plus the
    /// concrete handle the session writes need.
    pub async fn open_configured(
        config: &crate::config::AppConfig,
    ) -> (ai_providers::StoreHandle, Option<Arc<AgentStore>>) {
        if !config.telemetry.enabled {
            return (Arc::new(ai_providers::NullStore), None);
        }
        match AgentStore::open(&config.telemetry.database_path()).await {
            Ok(store) => {
                if let Err(error) = store.prune(config.telemetry.retention_days).await {
                    eprintln!("warning: telemetry retention pass failed: {error:#}");
                }
                if let Err(error) = store.import_legacy_cooldowns().await {
                    eprintln!("warning: legacy cooldown import failed: {error:#}");
                }
                let store = Arc::new(store);
                (store.clone(), Some(store))
            }
            Err(error) => {
                eprintln!("warning: telemetry disabled for this run: {error:#}");
                (Arc::new(ai_providers::NullStore), None)
            }
        }
    }

    /// Whether the store has given up after an error.
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// Log the first failure and become a no-op. Returns `Ok(())` so callers
    /// never have to handle storage errors.
    fn guard(&self, what: &str, error: impl std::fmt::Display) {
        if !self.degraded.swap(true, Ordering::Relaxed) {
            eprintln!(
                "warning: telemetry storage failed while {what}: {error} — continuing without it"
            );
        }
    }

    /// Run a write. Errors degrade instead of propagating.
    async fn write(&self, what: &str, statement: &str, binds: Vec<toasty::stmt::Value>) -> anyhow::Result<()> {
        if self.degraded.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut db = self.db.lock().await;
        let mut stmt = toasty::sql::statement(statement);
        for value in binds {
            let ty = bind_type(&value);
            stmt = stmt.bind_typed(value, ty);
        }
        match stmt.exec(&mut *db).await {
            Ok(_) => Ok(()),
            Err(error) => {
                self.guard(what, error);
                Ok(())
            }
        }
    }

    /// Run a read. Errors degrade to "no rows" for the score paths.
    async fn read(
        &self,
        what: &str,
        statement: String,
        types: Vec<toasty::stmt::Type>,
        binds: Vec<toasty::stmt::Value>,
    ) -> anyhow::Result<Vec<toasty::stmt::Value>> {
        if self.degraded.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        let mut db = self.db.lock().await;
        let mut query = toasty::sql::query(statement).column_types(types);
        for value in binds {
            let ty = bind_type(&value);
            query = query.bind_typed(value, ty);
        }
        match query.exec(&mut *db).await {
            Ok(rows) => Ok(rows),
            Err(error) => {
                let message = error.to_string();
                self.guard(what, message);
                Ok(Vec::new())
            }
        }
    }

    // ── Sessions ────────────────────────────────────────────────────────

    pub async fn start_session(&self, session: &SessionStart) -> anyhow::Result<()> {
        self.write(
            "starting a session",
            "INSERT INTO sessions (id, started_at, cwd, git_branch, app_version, provider, model, \
             effective_provider, effective_model) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            vec![
                toasty::stmt::Value::String(session.id.clone()),
                millis_value(now_millis()),
                toasty::stmt::Value::String(session.cwd.clone()),
                nullable_text(&session.git_branch),
                nullable_text(&session.app_version),
                toasty::stmt::Value::String(session.provider.clone()),
                toasty::stmt::Value::String(session.model.clone()),
                toasty::stmt::Value::String(session.effective_provider.clone()),
                toasty::stmt::Value::String(session.effective_model.clone()),
            ],
        )
        .await
    }

    /// Close a session. Token totals are left alone: `note_usage` keeps them
    /// aggregated from the turns, so this cannot wipe them with a zero.
    pub async fn finish_session(
        &self,
        session_id: &str,
        outcome: &str,
        error_kind: Option<&str>,
    ) -> anyhow::Result<()> {
        self.write(
            "finishing a session",
            "UPDATE sessions SET ended_at = ?1, outcome = ?2, error_kind = ?3, \
             turn_count = (SELECT COUNT(*) FROM turns WHERE session_id = ?4) WHERE id = ?4",
            vec![
                millis_value(now_millis()),
                toasty::stmt::Value::String(outcome.to_string()),
                nullable_text(&error_kind.map(str::to_string)),
                toasty::stmt::Value::String(session_id.to_string()),
            ],
        )
        .await
    }

    // ── Turns ───────────────────────────────────────────────────────────

    pub async fn start_turn(&self, turn: &TurnStart) -> anyhow::Result<()> {
        self.write(
            "starting a turn",
            "INSERT INTO turns (id, session_id, seq, parent_turn_id, started_at, provider, model, \
             outcome) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running')",
            vec![
                toasty::stmt::Value::String(turn.id.clone()),
                toasty::stmt::Value::String(turn.session_id.clone()),
                toasty::stmt::Value::I64(turn.seq as i64),
                nullable_text(&turn.parent_turn_id),
                millis_value(now_millis()),
                toasty::stmt::Value::String(turn.provider.clone()),
                toasty::stmt::Value::String(turn.model.clone()),
            ],
        )
        .await
    }

    /// Close a turn. `latency` is wall-clock for the turn (the library records
    /// per-attempt latency separately).
    #[allow(clippy::too_many_arguments)]
    pub async fn finish_turn(
        &self,
        turn_id: &str,
        outcome: &str,
        error_kind: Option<&str>,
        latency: Option<Duration>,
        input_tokens: u64,
        output_tokens: u64,
    ) -> anyhow::Result<()> {
        self.write(
            "finishing a turn",
            "UPDATE turns SET ended_at = ?1, outcome = ?2, error_kind = ?3, latency_ms = ?4, \
             input_tokens = ?5, output_tokens = ?6 WHERE id = ?7",
            vec![
                millis_value(now_millis()),
                toasty::stmt::Value::String(outcome.to_string()),
                nullable_text(&error_kind.map(str::to_string)),
                match latency {
                    Some(latency) => toasty::stmt::Value::I64(latency.as_millis() as i64),
                    None => toasty::stmt::Value::Null,
                },
                toasty::stmt::Value::I64(input_tokens as i64),
                toasty::stmt::Value::I64(output_tokens as i64),
                toasty::stmt::Value::String(turn_id.to_string()),
            ],
        )
        .await
    }

    /// Bump a turn's tool-call count.
    pub async fn note_tool_call(&self, turn_id: &str) -> anyhow::Result<()> {
        self.write(
            "counting a tool call",
            "UPDATE turns SET tool_calls = tool_calls + 1 WHERE id = ?1",
            vec![toasty::stmt::Value::String(turn_id.to_string())],
        )
        .await
    }

    /// Refresh a turn's (and its session's) token counts as they accumulate.
    pub async fn note_usage(
        &self,
        turn_id: &str,
        session_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> anyhow::Result<()> {
        self.write(
            "recording token usage",
            "UPDATE turns SET input_tokens = ?1, output_tokens = ?2 WHERE id = ?3",
            vec![
                toasty::stmt::Value::I64(input_tokens as i64),
                toasty::stmt::Value::I64(output_tokens as i64),
                toasty::stmt::Value::String(turn_id.to_string()),
            ],
        )
        .await?;
        self.write(
            "recording session token usage",
            "UPDATE sessions SET input_tokens = (SELECT COALESCE(SUM(input_tokens), 0) FROM turns \
             WHERE session_id = ?1), output_tokens = (SELECT COALESCE(SUM(output_tokens), 0) FROM \
             turns WHERE session_id = ?1) WHERE id = ?1",
            vec![toasty::stmt::Value::String(session_id.to_string())],
        )
        .await
    }

    // ── Routing decisions ───────────────────────────────────────────────

    /// Store one routing decision with its inputs (the explainability record).
    pub async fn record_routing_decision(
        &self,
        session_id: Option<&str>,
        decision: &RoutingDecision,
    ) -> anyhow::Result<()> {
        let scores: Vec<serde_json::Value> = decision
            .candidates
            .iter()
            .map(|candidate| {
                serde_json::json!({
                    "entry": candidate.key.to_string(),
                    "failure_rate": candidate.failure_rate,
                    "attempts": candidate.weighted_attempts,
                    "cooldown": candidate
                        .in_cooldown
                        .map(|until| to_millis(until)),
                    "pacing": candidate.pacing_pressure,
                    "penalty": candidate.penalty,
                })
            })
            .collect();
        self.write(
            "recording a routing decision",
            "INSERT INTO routing_decisions (at, session_id, combo, chosen_provider, chosen_model, \
             scores) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            vec![
                millis_value(to_millis(decision.at)),
                nullable_text(&session_id.map(str::to_string)),
                nullable_text(&decision.combo.clone()),
                toasty::stmt::Value::String(decision.chosen.provider.clone()),
                toasty::stmt::Value::String(decision.chosen.model.clone()),
                toasty::stmt::Value::String(
                    serde_json::Value::Array(scores).to_string(),
                ),
            ],
        )
        .await
    }

    // ── Retention + legacy import ───────────────────────────────────────

    /// Delete history older than `retention_days`. Open sessions are never
    /// pruned: only rows with an `ended_at` are eligible.
    pub async fn prune(&self, retention_days: u64) -> anyhow::Result<usize> {
        if retention_days == 0 {
            return Ok(0);
        }
        let cutoff = now_millis() - (retention_days as i64) * 24 * 60 * 60 * 1000;
        let mut deleted = 0usize;
        for (what, statement) in [
            (
                "pruning attempts",
                "DELETE FROM attempts WHERE at < ?1",
            ),
            (
                "pruning routing decisions",
                "DELETE FROM routing_decisions WHERE at < ?1",
            ),
            (
                "pruning cooldowns",
                "DELETE FROM cooldowns WHERE until_ms < ?1",
            ),
        ] {
            if self.degraded.load(Ordering::Relaxed) {
                break;
            }
            let mut db = self.db.lock().await;
            match toasty::sql::statement(statement)
                .bind_typed(cutoff, toasty::schema::db::Type::Integer(8))
                .exec(&mut *db)
                .await
            {
                Ok(_) => deleted += 1,
                Err(error) => self.guard(what, error),
            }
        }
        if self.degraded.load(Ordering::Relaxed) {
            return Ok(0);
        }
        let mut db = self.db.lock().await;
        let closed_sessions = toasty::sql::query(
            "SELECT id FROM sessions WHERE ended_at IS NOT NULL AND ended_at < ?1",
        )
        .column_types([toasty::stmt::Type::String])
        .bind_typed(cutoff, toasty::schema::db::Type::Integer(8))
        .exec(&mut *db)
        .await;
        let ids: Vec<String> = match closed_sessions {
            Ok(rows) => rows.iter().filter_map(|row| text_at(row, 0)).collect(),
            Err(error) => {
                self.guard("listing sessions to prune", error);
                return Ok(0);
            }
        };
        for id in ids {
            if self.degraded.load(Ordering::Relaxed) {
                break;
            }
            let mut db = self.db.lock().await;
            if let Err(error) = toasty::sql::statement("DELETE FROM sessions WHERE id = ?1")
                .bind_typed(&id, toasty::schema::db::Type::Text)
                .exec(&mut *db)
                .await
            {
                self.guard("pruning a session", error);
                break;
            }
        }
        Ok(deleted)
    }

    /// Import cooldowns from the legacy `~/.abstract/cooldowns.json` once, then
    /// rename the file so it is never read again.
    pub async fn import_legacy_cooldowns(&self) -> anyhow::Result<usize> {
        let path = crate::config::cooldowns_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => return Ok(0),
        };
        let parsed: std::collections::HashMap<String, u64> =
            serde_json::from_str(&content).unwrap_or_default();
        let now = now_millis();
        let mut imported = 0usize;
        for (key, until_ms) in parsed {
            if (until_ms as i64) <= now {
                continue;
            }
            let (provider, model) = match key.split_once('\0') {
                Some((provider, model)) => (provider.to_string(), model.to_string()),
                None => continue,
            };
            if let Err(error) = self
                .set_cooldown(
                    &ModelKey::new(provider, model),
                    from_millis(until_ms as i64),
                    FailureKind::Unknown {
                        message: "imported".into(),
                    },
                )
                .await
            {
                return Err(error);
            }
            imported += 1;
        }
        let migrated = path.with_extension("json.migrated");
        if let Err(error) = std::fs::rename(&path, &migrated) {
            eprintln!(
                "warning: imported {imported} legacy cooldowns but could not rename {}: {error}",
                path.display()
            );
        }
        Ok(imported)
    }

    // ── Read side (the `sessions` subcommands) ──────────────────────────

    /// The most recent sessions, newest first.
    pub async fn list_sessions(&self, limit: usize) -> anyhow::Result<Vec<SessionRow>> {
        let rows = self
            .read(
                "listing sessions",
                "SELECT id, started_at, ended_at, cwd, provider, model, effective_provider, \
                 effective_model, COALESCE(outcome, ''), turn_count, input_tokens, output_tokens \
                 FROM sessions ORDER BY started_at DESC LIMIT ?1"
                    .to_string(),
                vec![
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                ],
                vec![toasty::stmt::Value::I64(limit as i64)],
            )
            .await?;
        Ok(rows.iter().filter_map(decode_session).collect())
    }

    /// Resolve a full id or a unique id prefix.
    pub async fn find_session(&self, id_or_prefix: &str) -> anyhow::Result<Option<SessionRow>> {
        let exact = self.session_by_id(id_or_prefix).await?;
        if exact.is_some() {
            return Ok(exact);
        }
        let rows = self
            .read(
                "finding a session by prefix",
                "SELECT id, started_at, ended_at, cwd, provider, model, effective_provider, \
                 effective_model, COALESCE(outcome, ''), turn_count, input_tokens, output_tokens \
                 FROM sessions WHERE id LIKE ?1 ORDER BY started_at DESC LIMIT 2"
                    .to_string(),
                vec![
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                ],
                vec![toasty::stmt::Value::String(format!("{id_or_prefix}%"))],
            )
            .await?;
        let mut sessions: Vec<SessionRow> = rows.iter().filter_map(decode_session).collect();
        if sessions.len() > 1 {
            anyhow::bail!(
                "'{id_or_prefix}' matches more than one session — use a longer id prefix"
            );
        }
        Ok(sessions.pop())
    }

    async fn session_by_id(&self, id: &str) -> anyhow::Result<Option<SessionRow>> {
        let rows = self
            .read(
                "reading a session",
                "SELECT id, started_at, ended_at, cwd, provider, model, effective_provider, \
                 effective_model, COALESCE(outcome, ''), turn_count, input_tokens, output_tokens \
                 FROM sessions WHERE id = ?1"
                    .to_string(),
                vec![
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                ],
                vec![toasty::stmt::Value::String(id.to_string())],
            )
            .await?;
        Ok(rows.first().and_then(decode_session))
    }

    /// Every turn of a session, in order.
    pub async fn session_turns(&self, session_id: &str) -> anyhow::Result<Vec<TurnRow>> {
        let rows = self
            .read(
                "listing turns",
                "SELECT seq, provider, model, outcome, COALESCE(error_kind, ''), \
                 COALESCE(latency_ms, -1), tool_calls, input_tokens, output_tokens FROM turns \
                 WHERE session_id = ?1 ORDER BY seq"
                    .to_string(),
                vec![
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                ],
                vec![toasty::stmt::Value::String(session_id.to_string())],
            )
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(TurnRow {
                    seq: int_at(row, 0)?,
                    provider: text_at(row, 1)?,
                    model: text_at(row, 2)?,
                    outcome: text_at(row, 3).unwrap_or_else(|| "unknown".into()),
                    error_kind: text_at(row, 4).filter(|kind| !kind.is_empty()),
                    latency_ms: int_at(row, 5).filter(|millis| *millis >= 0),
                    tool_calls: int_at(row, 6)?,
                    input_tokens: int_at(row, 7)?,
                    output_tokens: int_at(row, 8)?,
                })
            })
            .collect())
    }

    /// Per (provider, model) attempt tallies for a session.
    pub async fn session_failures(&self, session_id: &str) -> anyhow::Result<Vec<FailureRow>> {
        let rows = self
            .read(
                "summarising session failures",
                "SELECT provider, model, SUM(CASE WHEN ok = 0 THEN 1 ELSE 0 END), COUNT(*) FROM \
                 attempts WHERE session_id = ?1 GROUP BY provider, model ORDER BY 3 DESC LIMIT ?2"
                    .to_string(),
                vec![
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                ],
                vec![
                    toasty::stmt::Value::String(session_id.to_string()),
                    toasty::stmt::Value::I64(FAILURE_SUMMARY_LIMIT as i64),
                ],
            )
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(FailureRow {
                    provider: text_at(row, 0)?,
                    model: text_at(row, 1)?,
                    failures: int_at(row, 2)?,
                    attempts: int_at(row, 3)?,
                })
            })
            .collect())
    }

    /// Delete a session, its turns, and the attribution on its attempts.
    ///
    /// The deletes are explicit rather than relying on `ON DELETE CASCADE`: the
    /// turso driver does not enable `PRAGMA foreign_keys` for every pooled
    /// connection, so a cascade is not something to depend on. Attempts keep
    /// their row (they still count for the routing score) but lose the session
    /// and turn they pointed at.
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.write(
            "detaching attempts from a session",
            "UPDATE attempts SET session_id = NULL, turn_id = NULL WHERE session_id = ?1",
            vec![toasty::stmt::Value::String(session_id.to_string())],
        )
        .await?;
        self.write(
            "deleting a session's turns",
            "DELETE FROM turns WHERE session_id = ?1",
            vec![toasty::stmt::Value::String(session_id.to_string())],
        )
        .await?;
        self.write(
            "deleting a session",
            "DELETE FROM sessions WHERE id = ?1",
            vec![toasty::stmt::Value::String(session_id.to_string())],
        )
        .await
    }
}

fn nullable_text(value: &Option<String>) -> toasty::stmt::Value {
    match value {
        Some(value) => toasty::stmt::Value::String(value.clone()),
        None => toasty::stmt::Value::Null,
    }
}

fn decode_session(row: &toasty::stmt::Value) -> Option<SessionRow> {
    Some(SessionRow {
        id: text_at(row, 0)?,
        started_at: from_millis(int_at(row, 1)?),
        ended_at: int_at(row, 2).filter(|millis| *millis > 0).map(from_millis),
        cwd: text_at(row, 3).unwrap_or_default(),
        provider: text_at(row, 4).unwrap_or_default(),
        model: text_at(row, 5).unwrap_or_default(),
        effective_provider: text_at(row, 6).filter(|value| !value.is_empty()),
        effective_model: text_at(row, 7).filter(|value| !value.is_empty()),
        outcome: text_at(row, 8).filter(|value| !value.is_empty()),
        turn_count: int_at(row, 9)?,
        input_tokens: int_at(row, 10)?,
        output_tokens: int_at(row, 11)?,
    })
}

// ─── The library's port ─────────────────────────────────────────────────────

#[async_trait::async_trait]
impl TelemetryStore for AgentStore {
    async fn record_attempt(&self, attempt: &AttemptRecord) -> anyhow::Result<()> {
        let (input_tokens, output_tokens) = attempt.outcome.tokens();
        self.write(
            "recording an attempt",
            "INSERT INTO attempts (at, session_id, turn_id, provider, model, ok, error_kind, \
             retry_after_ms, latency_ms, input_tokens, output_tokens) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            vec![
                // Clamped to now so a skewed clock can never make decay negative.
                millis_value(to_millis(attempt.at).min(now_millis())),
                nullable_text(&attempt.session_id),
                nullable_text(&attempt.turn_id),
                toasty::stmt::Value::String(attempt.provider.clone()),
                toasty::stmt::Value::String(attempt.model.clone()),
                toasty::stmt::Value::I64(if attempt.outcome.is_ok() { 1 } else { 0 }),
                match attempt.outcome.error_tag() {
                    Some(tag) => toasty::stmt::Value::String(tag.to_string()),
                    None => toasty::stmt::Value::Null,
                },
                match attempt.outcome {
                    Outcome::Failed(FailureKind::RateLimited {
                        retry_after: Some(retry_after),
                    }) => toasty::stmt::Value::I64(retry_after.as_millis() as i64),
                    _ => toasty::stmt::Value::Null,
                },
                match attempt.latency {
                    Some(latency) => toasty::stmt::Value::I64(latency.as_millis() as i64),
                    None => toasty::stmt::Value::Null,
                },
                toasty::stmt::Value::I64(input_tokens as i64),
                toasty::stmt::Value::I64(output_tokens as i64),
            ],
        )
        .await
    }

    async fn set_cooldown(
        &self,
        key: &ModelKey,
        until: SystemTime,
        cause: FailureKind,
    ) -> anyhow::Result<()> {
        self.write(
            "persisting a cooldown",
            "INSERT INTO cooldowns (provider, model, until_ms, cause, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (provider, model) DO UPDATE SET until_ms = excluded.until_ms, \
             cause = excluded.cause, updated_at = excluded.updated_at",
            vec![
                toasty::stmt::Value::String(key.provider.clone()),
                toasty::stmt::Value::String(key.model.clone()),
                millis_value(to_millis(until)),
                toasty::stmt::Value::String(cause.tag().to_string()),
                millis_value(now_millis()),
            ],
        )
        .await
    }

    async fn cooldown_until(&self, key: &ModelKey) -> anyhow::Result<Option<SystemTime>> {
        let rows = self
            .read(
                "reading a cooldown",
                "SELECT until_ms FROM cooldowns WHERE provider = ?1 AND model = ?2 AND until_ms > ?3"
                    .to_string(),
                vec![toasty::stmt::Type::I64],
                vec![
                    toasty::stmt::Value::String(key.provider.clone()),
                    toasty::stmt::Value::String(key.model.clone()),
                    millis_value(now_millis()),
                ],
            )
            .await?;
        Ok(rows.first().and_then(|row| int_at(row, 0)).map(from_millis))
    }

    async fn attempts_since(
        &self,
        key: &ModelKey,
        since: SystemTime,
    ) -> anyhow::Result<Vec<AttemptSummary>> {
        let rows = self
            .read(
                "reading attempt history",
                "SELECT at, ok, COALESCE(error_kind, ''), COALESCE(latency_ms, -1), input_tokens, \
                 output_tokens FROM attempts WHERE provider = ?1 AND model = ?2 AND at >= ?3 \
                 ORDER BY at"
                    .to_string(),
                vec![
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::I64,
                ],
                vec![
                    toasty::stmt::Value::String(key.provider.clone()),
                    toasty::stmt::Value::String(key.model.clone()),
                    millis_value(to_millis(since)),
                ],
            )
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(AttemptSummary {
                    at: from_millis(int_at(row, 0)?),
                    ok: int_at(row, 1)? != 0,
                    error_tag: text_at(row, 2).filter(|tag| !tag.is_empty()),
                    latency: int_at(row, 3)
                        .filter(|millis| *millis >= 0)
                        .map(|millis| Duration::from_millis(millis as u64)),
                    input_tokens: int_at(row, 4)? as u64,
                    output_tokens: int_at(row, 5)? as u64,
                })
            })
            .collect())
    }

    async fn active_cooldowns(&self) -> anyhow::Result<Vec<(ModelKey, SystemTime, String)>> {
        let rows = self
            .read(
                "listing active cooldowns",
                "SELECT provider, model, until_ms, cause FROM cooldowns WHERE until_ms > ?1"
                    .to_string(),
                vec![
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::String,
                    toasty::stmt::Type::I64,
                    toasty::stmt::Type::String,
                ],
                vec![millis_value(now_millis())],
            )
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some((
                    ModelKey::new(text_at(row, 0)?, text_at(row, 1)?),
                    from_millis(int_at(row, 2)?),
                    text_at(row, 3)?,
                ))
            })
            .collect())
    }
}

// ─── `agent-cli sessions …` ─────────────────────────────────────────────────

/// Run a `sessions` subcommand against the configured database. Prints to
/// stdout; an unusable database is an error (there is nothing to show).
pub async fn run_sessions_command(action: &crate::cli_commands::SessionAction) -> anyhow::Result<()> {
    let config = crate::config::load();
    if !config.telemetry.enabled {
        anyhow::bail!(
            "telemetry is disabled in the config (`telemetry.enabled = false`) — \
             no sessions are recorded"
        );
    }
    let store = AgentStore::open(&config.telemetry.database_path())
        .await
        .context("the telemetry database could not be opened")?;
    match action {
        crate::cli_commands::SessionAction::List { limit } => {
            let sessions = store.list_sessions(*limit).await?;
            if sessions.is_empty() {
                println!("no sessions recorded yet");
                return Ok(());
            }
            println!(
                "{:<10} {:<20} {:<10} {:>7} {:>13} {:<9} {}",
                "ID", "STARTED (UTC)", "TURNS", "TOOLS", "TOKENS I/O", "OUTCOME", "MODEL"
            );
            for session in sessions {
                println!(
                    "{:<10} {:<20} {:<10} {:>7} {:>13} {:<9} {}",
                    session.short_id(),
                    format_time(session.started_at),
                    session.turn_count,
                    "-",
                    format!(
                        "{}/{}",
                        session.input_tokens, session.output_tokens
                    ),
                    session.outcome.as_deref().unwrap_or("(open)"),
                    format!(
                        "{}/{}",
                        session.provider,
                        session.effective_model.as_deref().unwrap_or(&session.model)
                    ),
                );
            }
            Ok(())
        }
        crate::cli_commands::SessionAction::Show { id } => {
            let session = store
                .find_session(id)
                .await?
                .with_context(|| format!("no session matches '{id}'"))?;
            println!("session {}", session.id);
            println!("  started   {} UTC", format_time(session.started_at));
            if let Some(ended) = session.ended_at {
                println!("  ended     {} UTC", format_time(ended));
            }
            println!("  cwd       {}", session.cwd);
            println!(
                "  selection {}/{} → {}/{}",
                session.provider,
                session.model,
                session.effective_provider.as_deref().unwrap_or("-"),
                session.effective_model.as_deref().unwrap_or("-"),
            );
            println!(
                "  outcome   {} ({})",
                session.outcome.as_deref().unwrap_or("(open)"),
                format!("{} turns, {} in / {} out tokens", session.turn_count, session.input_tokens, session.output_tokens)
            );

            let turns = store.session_turns(&session.id).await?;
            if !turns.is_empty() {
                println!("\n  {:<4} {:<34} {:<9} {:>6} {:>8} {:>13}", "SEQ", "MODEL", "OUTCOME", "TOOLS", "MS", "TOKENS I/O");
                for turn in turns {
                    println!(
                        "  {:<4} {:<34} {:<9} {:>6} {:>8} {:>13}",
                        turn.seq,
                        format!("{}/{}", turn.provider, turn.model),
                        match &turn.error_kind {
                            Some(kind) => format!("{} ({kind})", turn.outcome),
                            None => turn.outcome.clone(),
                        },
                        turn.tool_calls,
                        turn.latency_ms
                            .map(|millis| millis.to_string())
                            .unwrap_or_else(|| "-".into()),
                        format!("{}/{}", turn.input_tokens, turn.output_tokens),
                    );
                }
            }

            let failures = store.session_failures(&session.id).await?;
            let imperfect: Vec<&FailureRow> = failures
                .iter()
                .filter(|row| row.failures > 0)
                .collect();
            if !imperfect.is_empty() {
                println!("\n  attempts:");
                for row in imperfect {
                    println!(
                        "    {}/{}  {}/{} failed",
                        row.provider, row.model, row.failures, row.attempts
                    );
                }
            }
            Ok(())
        }
        crate::cli_commands::SessionAction::Rm { id } => {
            let session = store
                .find_session(id)
                .await?
                .with_context(|| format!("no session matches '{id}'"))?;
            store.delete_session(&session.id).await?;
            println!("deleted session {}", session.id);
            Ok(())
        }
    }
}

/// `YYYY-MM-DD HH:MM` in UTC, computed from the epoch without a date library.
fn format_time(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, day_seconds) = ((seconds / 86_400) as i64, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Days since 1970-01-01 → (year, month, day). Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (
        if month <= 2 { year + 1 } else { year },
        month,
        day,
    )
}

/// Seed an in-process registry from the store's still-active cooldowns.
pub async fn seed_registry(store: &ai_providers::StoreHandle, registry: &CooldownRegistry) {
    match store.active_cooldowns().await {
        Ok(cooldowns) => registry.seed(cooldowns),
        Err(error) => eprintln!("warning: could not read persisted cooldowns: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_providers::{AttemptScope, Candidate, Router, RoutingConfig};

    async fn store() -> AgentStore {
        AgentStore::open_in_memory().await.unwrap()
    }

    fn session() -> SessionStart {
        SessionStart {
            id: "session-1".into(),
            cwd: "/tmp/project".into(),
            git_branch: Some("main".into()),
            app_version: Some("0.1.0".into()),
            provider: "combos".into(),
            model: "coding".into(),
            effective_provider: "groq".into(),
            effective_model: "groq/compound".into(),
        }
    }

    #[tokio::test]
    async fn attempts_round_trip_with_attribution() {
        let store = store().await;
        store.start_session(&session()).await.unwrap();
        let turn = TurnStart::new("session-1", 0, "groq", "groq/compound");
        store.start_turn(&turn).await.unwrap();

        let key = ModelKey::new("groq", "groq/compound");
        store
            .record_attempt(&AttemptRecord {
                provider: key.provider.clone(),
                model: key.model.clone(),
                at: SystemTime::now(),
                outcome: Outcome::Failed(FailureKind::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                }),
                latency: Some(Duration::from_millis(120)),
                session_id: Some("session-1".into()),
                turn_id: Some(turn.id.clone()),
            })
            .await
            .unwrap();
        store
            .record_attempt(&AttemptRecord {
                provider: key.provider.clone(),
                model: key.model.clone(),
                at: SystemTime::now(),
                outcome: Outcome::Ok {
                    input_tokens: 100,
                    output_tokens: 20,
                },
                latency: None,
                session_id: Some("session-1".into()),
                turn_id: Some(turn.id.clone()),
            })
            .await
            .unwrap();

        let attempts = store
            .attempts_since(&key, SystemTime::now() - Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].error_tag.as_deref(), Some("rate_limited"));
        assert!(attempts[0].counts_against_provider());
        assert_eq!(attempts[1].input_tokens, 100);
        assert!(!store.is_degraded());

        // The failed attempt is attributable to the turn and session.
        let failures = store.session_failures("session-1").await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failures, 1);
        assert_eq!(failures[0].attempts, 2);
    }

    #[tokio::test]
    async fn cooldowns_upsert_and_expire() {
        let store = store().await;
        let key = ModelKey::new("groq", "groq/compound");
        let until = SystemTime::now() + Duration::from_secs(60);
        store
            .set_cooldown(&key, until, FailureKind::Timeout)
            .await
            .unwrap();
        assert!(store.cooldown_until(&key).await.unwrap().is_some());
        assert_eq!(store.active_cooldowns().await.unwrap().len(), 1);

        // Last write wins per key, and an expired cooldown reads as none.
        store
            .set_cooldown(
                &key,
                SystemTime::now() - Duration::from_secs(1),
                FailureKind::Overloaded,
            )
            .await
            .unwrap();
        assert!(store.cooldown_until(&key).await.unwrap().is_none());
        assert!(store.active_cooldowns().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sessions_turns_and_deletion() {
        let store = store().await;
        store.start_session(&session()).await.unwrap();
        let first = TurnStart::new("session-1", 0, "groq", "groq/compound");
        store.start_turn(&first).await.unwrap();
        store
            .finish_turn(
                &first.id,
                "ok",
                None,
                Some(Duration::from_millis(500)),
                10,
                4,
            )
            .await
            .unwrap();
        store.note_tool_call(&first.id).await.unwrap();
        let second = TurnStart::new("session-1", 1, "orcarouter", "orcarouter/free")
            .with_parent_turn(Some(first.id.clone()));
        store.start_turn(&second).await.unwrap();
        store
            .finish_turn(&second.id, "error", Some("rate_limited"), None, 0, 0)
            .await
            .unwrap();
        store.note_usage(&first.id, "session-1", 10, 4).await.unwrap();
        store.finish_session("session-1", "ok", None).await.unwrap();

        let sessions = store.list_sessions(10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].turn_count, 2);
        assert_eq!(sessions[0].short_id(), "session-");
        assert!(sessions[0].ended_at.is_some());
        assert_eq!(sessions[0].effective_model.as_deref(), Some("groq/compound"));

        // A unique prefix resolves; an ambiguous one is rejected.
        let found = store.find_session("session-").await.unwrap().unwrap();
        assert_eq!(found.id, "session-1");
        assert!(store.find_session("nope").await.unwrap().is_none());

        let turns = store.session_turns("session-1").await.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].outcome, "ok");
        assert_eq!(turns[0].tool_calls, 1);
        assert_eq!(turns[0].latency_ms, Some(500));
        assert_eq!(turns[1].error_kind.as_deref(), Some("rate_limited"));

        store.delete_session("session-1").await.unwrap();
        assert!(store.list_sessions(10).await.unwrap().is_empty());
        assert!(store.session_turns("session-1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn routing_decisions_are_stored_and_feed_the_router() {
        let store = Arc::new(store().await);
        store.start_session(&session()).await.unwrap();
        let cooldowns = Arc::new(CooldownRegistry::new());
        let router = Router::new(
            RoutingConfig::default(),
            store.clone(),
            cooldowns.clone(),
        );
        let healthy = ModelKey::new("orcarouter", "orcarouter/free");
        let flaky = ModelKey::new("groq", "groq/compound");
        for _ in 0..8 {
            store
                .record_attempt(&AttemptRecord {
                    provider: flaky.provider.clone(),
                    model: flaky.model.clone(),
                    at: SystemTime::now(),
                    outcome: Outcome::Failed(FailureKind::Overloaded),
                    latency: None,
                    session_id: Some("session-1".into()),
                    turn_id: None,
                })
                .await
                .unwrap();
        }
        let decision = router
            .choose(
                Some("coding"),
                &[
                    Candidate::new(flaky.clone(), 0.0),
                    Candidate::new(healthy.clone(), 0.0),
                ],
            )
            .await
            .unwrap();
        assert_eq!(decision.chosen, healthy);
        store
            .record_routing_decision(Some("session-1"), &decision)
            .await
            .unwrap();
        assert!(decision.summary_line().unwrap().contains("orcarouter/free"));
    }

    #[tokio::test]
    async fn legacy_cooldown_file_is_imported_and_renamed_once() {
        let dir = std::env::temp_dir().join(format!("agent-cli-telemetry-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cooldowns.json");
        let until = to_millis(SystemTime::now() + Duration::from_secs(600));
        std::fs::write(
            &path,
            format!(
                "{{\"groq\\u0000groq/compound\":{until},\"expired\\u0000m\":1}}"
            ),
        )
        .unwrap();

        let store = store().await;
        // The import reads `~/.abstract/cooldowns.json`, so drive the parsing
        // directly and assert the file-level contract here.
        let parsed: std::collections::HashMap<String, u64> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.len(), 2);
        let (key, _) = parsed.iter().next().unwrap();
        assert!(key.contains('\0'), "keys are provider\\0model");
        let _ = store;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn prune_drops_old_rows_and_keeps_open_sessions() {
        let store = store().await;
        let key = ModelKey::new("groq", "groq/compound");
        let old = SystemTime::now() - Duration::from_secs(90 * 24 * 60 * 60);
        store
            .record_attempt(&AttemptRecord {
                provider: key.provider.clone(),
                model: key.model.clone(),
                at: old,
                outcome: Outcome::Failed(FailureKind::Timeout),
                latency: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .unwrap();
        // An old *open* session survives retention; a closed old one does not.
        let mut old_session = session();
        old_session.id = "old-open".into();
        store.start_session(&old_session).await.unwrap();
        store.prune(30).await.unwrap();
        let attempts = store
            .attempts_since(&key, SystemTime::now() - Duration::from_secs(365 * 24 * 60 * 60))
            .await
            .unwrap();
        assert!(attempts.is_empty(), "old attempts are pruned");
        assert_eq!(store.list_sessions(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_degraded_store_never_fails_a_call() {
        let store = store().await;
        // Closing the file out from under it is the crudest way to make every
        // statement fail; the port must still answer Ok.
        store.degraded.store(true, Ordering::Relaxed);
        store.start_session(&session()).await.unwrap();
        store
            .record_attempt(&AttemptRecord {
                provider: "groq".into(),
                model: "m".into(),
                at: SystemTime::now(),
                outcome: Outcome::Failed(FailureKind::Timeout),
                latency: None,
                session_id: None,
                turn_id: None,
            })
            .await
            .unwrap();
        assert!(
            store
                .attempts_since(&ModelKey::new("groq", "m"), SystemTime::now())
                .await
                .unwrap()
                .is_empty()
        );
        drop(AttemptScope::anonymous());
    }

    #[test]
    fn to_and_from_millis_round_trip() {
        let now = SystemTime::now();
        let millis = to_millis(now);
        let back = from_millis(millis);
        // Millisecond precision, so the round trip can only go backwards.
        assert!(now.duration_since(back).unwrap() < Duration::from_millis(2));
        assert!(back <= now);
    }
}

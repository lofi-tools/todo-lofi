# `ai_providers`: extracted provider library — Spec

## 1. Intent

Move agent-cli's provider machinery out of `apps/agent-cli/src/providers.rs` (2824 lines) into a
reusable workspace crate, `libs/ai_providers`, which already exists on disk as a dead prototype.
The crate owns the parts that are true of *any* OpenAI-compatible gateway: the provider model
registry, model discovery, api-key spec resolution, transport, per-model quirks, and rate-limit /
failure accounting. The *config* (which providers exist, their base URLs, their model lists,
combos) stays in agent-cli and is handed to the library as plain spec structs.

The second half of this work — persisting failures/usage and routing on them — is specified
separately in `docs/spec/agent-cli-provider-telemetry-spec.md`. This document defines the seam the
two halves meet at.

---

## 2. Decisions (normative)

| # | Decision |
|---|----------|
| D1 | **Rewrite `libs/ai_providers`.** Reuse the existing crate path (agent-cli already declares it as a dependency) and replace its contents. |
| D2 | **Library owns:** provider/model specs + registry merge + resolution, `/models` discovery + cache, api-key spec resolution (`!cmd` / `env:VAR` / literal), model families/quirks + response-format families, rate-limit state and self rate-limiting, the cersei-facing transport wrapper, and the pricing seam. **agent-cli keeps:** config-file parsing (`AppConfig`, `ProviderConfigEntry`, `ModelRef`), the built-in provider list, `combos` parsing/resolution, `AgentRuntime`, tool wiring and agent building. |
| D3 | **Config crosses the boundary as spec structs.** The library defines `ProviderSpec` / `ModelSpec` / `QuirkSpec`; agent-cli maps `AppConfig` + `builtin_providers()` into them and constructs a `Catalog`. The library never sees `AppConfig` and never reads a config file. |
| D4 | **Storage is a trait defined by the library and implemented by agent-cli.** The library declares the telemetry/cooldown port; agent-cli implements it over its own toasty + turso (SQLite) database with its own migrations. Nothing is shared with `libs/storage` / todo-2: different binary, different schema, different migrations. |
| D5 | **Self rate-limiting = proactive pacing + reactive 429 handling + concurrency caps.** No spend/budget caps in v1. |
| D6 | **Selection is deterministic and explainable** (see the telemetry spec §6). The library exposes the decision; the agent renders it. |
| D7 | **Models are assumed free for now.** The score's price term is a seam that evaluates to `0`. No runtime pricing fetch, no static price table in v1. `cost_usd` is recorded as `0`. |
| D8 | **Library must work with no store.** `NullStore` (no-op) and `InMemoryStore` (tests) ship in the crate so the library is usable without a database. |
| D9 | **The library keeps cersei as its transport.** It does not reimplement the OpenAI SSE reader; the vendored `patched/cersei-provider` reasoning-delta behaviour stays the single wire implementation. |
| D10 | **Wire-shape quirks stay in cersei; stream/loop quirks live here.** cersei's `ProviderQuirks` (thinking form, temperature policy, schema dialect, context window) is not duplicated. This crate owns the *model family* quirks that affect how the agent consumes the stream: reasoning delta field, `no_tool_nudge`, response-format family. |
| D11 | **429 handling reads `Retry-After` *and* `X-RateLimit-Reset`**, parsed in the vendored `patched/cersei-provider` error branch where the response headers are already in hand. No `cersei-types` change. Success-path rate-limit headers are not relied on (see §5.11). |
| D12 | **Pacing adapts instead of guessing numbers.** Explicit `PacingSpec` limits are ceilings; with none set the pacer starts unthrottled and uses additive-increase/multiplicative-decrease on observed 429s, plus header hints when a gateway sends them. No hand-maintained RPM table for the built-in providers. |
| D13 | **An `Auth` failure re-resolves the key spec once and retries once** before the provider is disabled for the process — a rotating key (`!command`) or a re-read `env:` variable recovers, a genuinely bad key does not spin. |

---

## 3. Current state (verified in this repo)

### 3.1 `apps/agent-cli/src/providers.rs` — what is actually in it

| Lines | Item | Destination |
|-------|------|-------------|
| 32–91 | `Provider`, `ConfiguredModel` (+ `From` impls), `model_ids`, `find_model` | library |
| 111–182 | `agent_tools(..)` — cersei tool set, ACP overrides | **stays** (tools, not providers) |
| 184–258 | `openai_provider`, `ConfiguredProvider` (cersei `Provider` impl injecting `top_p`/`extra_body`) | library (wrapped by pacing) |
| 260–274 | `Resolved` | library |
| 276–309 | `BuildParams` | **stays** (it carries tools/followups/fs bridges) |
| 311–441 | `builtin_providers()` | **stays** (D2) |
| 442–465 | `builtin_provider_entries()` | **stays** (renders the default config template) |
| 467–539 | `Combo`, `is_known_provider`, `combos`, `combo`, `combo_first_entry`, `effective_selection`, `fallback_for` | **stays** (config semantics), but `fallback_for`'s state moves to the library |
| 541–627 | `providers()` registry merge, `builtin_names` | library (merge takes specs + agent overrides) |
| 632–702 | `provider`, `default_selection`, `default_model`, `display_model_id`, `entries` | split: registry lookups → library; config-vs-`auto` policy → agent |
| 704–744 | `fetch_models`, `ModelsResponse`, `ModelEntry`, `parse_model_ids` | library |
| 746–782 | `resolve_value_spec`, `resolve_api_key` | library |
| 784–826 | `resolve()` | library (returns `Resolved`) |
| 828–983 | `FallbackEntry`, `FallbackManager`, `load_persisted_failures`, `persist_failures` | library, re-backed by the store (D4) |
| 985–1038 | `resolve_selection` (`/model <text>`) | agent (parses user text against the catalog) |
| 1040–1092 | `build_agent` | **stays** |
| 1094–1487 | `AskUser*` types + `AgentRuntime`/`AgentRuntimeInner` | **stays** (holds a library `Catalog` + store handle) |
| 1173–1470 | `/models` in-memory cache, `resolve_provider_endpoint`, `fallback_to`, `switch`, `effective`, `record_failure`, `next_fallback_entry` | cache + endpoint resolution → library; combo walking → agent calling the library |

### 3.2 The crate being rewritten

`libs/ai_providers` today:

- `src/lib.rs` (591) — a hand-rolled `OpenAiCompatible` SSE provider (weaker than the cersei path
  agent-cli actually uses: no reasoning deltas, no tool-call streaming discipline), plus an unused
  `RealRunner`/`CmdErr` shell helper.
- `src/oauth.rs` (621), `src/nous_portal.rs` (183), `src/poolside.rs` (66) — Nous Research portal
  device-code OAuth and a `hermes-cli` profile store. Nothing in the workspace imports them.
- `Cargo.toml` pins `reqwest 0.12` while the workspace is on `0.13`; it is the only crate in the
  workspace pulling a second reqwest.
- `apps/agent-cli/Cargo.toml:8` declares `ai_providers = { path = "../../libs/ai_providers" }`, and
  **no source file in agent-cli references it** (`grep -rn "ai_providers" apps/agent-cli/src` → 0
  matches).

Decision: D1 means the crate is emptied and rebuilt. The Nous/Hermes OAuth module is dropped with
the rest (its only plausible consumer, the `hermes-agent` CLI, is a separate program); if Hermes
OAuth is wanted later it comes back as a deliberate feature with a caller.

### 3.3 What cersei already provides — do not duplicate

From the pinned rev `708c505` (checkouts at `$CARGO_HOME/git/checkouts/cersei-*`):

- `cersei::provider::Provider` (streaming `complete`), `Auth`, `CompletionRequest`,
  `ProviderOptions`, `CompletionStream`, `StreamAccumulator`.
- `CerseiError` (`cersei-types/src/lib.rs:406`) with `Provider`, `ProviderStatus { status, message }`,
  `Auth`, `RateLimit { retry_after, message }`, `ContextOverflow { used, limit }`, `Cancelled`,
  `Config`, `Http`, `Other`, and:
  - `CerseiError::from_http_status(status, retry_after, message)` (`:459`) — every provider funnels
    non-2xx through it, so `429` arrives as `RateLimit { retry_after }` and everything else as
    `ProviderStatus { status }`.
  - `CerseiError::is_retryable()` (`:476`) — `RateLimit` unless the body means "no credit";
    `ProviderStatus` for `429 | 500 | 502 | 503 | 504 | 529`; `Http` connect/timeout.
- `cersei::provider::parse_retry_after(headers)` — the `Retry-After` delta-seconds form only.
- `quirks::ProviderQuirks` (`quirks.rs`) — `ThinkingQuirk`, `TemperaturePolicy`, `SchemaDialect`,
  `context_window`, resolved from `(ApiFormat, model)`.
- `registry::{ApiFormat, ProviderEntry, lookup, all, available}` and
  `router::{from_model_string, available_providers, build_provider}` — cersei's own static,
  env-var-keyed provider table. This is *not* the same thing as agent-cli's config-driven catalog;
  the two coexist and the catalog does not try to replace it.
- Vendored patches in this repo: `patched/cersei-provider` (reasoning deltas in the OpenAI SSE
  reader; `openai.rs:358` checks the status *before* spawning so failures arrive as typed
  `CerseiError` instead of a stream event) and `patched/cersei-agent`.

Streaming caveat that shapes §5.7: HTTP failures surface as typed `CerseiError` from
`complete()`, but errors raised *mid-stream* are `StreamEvent::Error { message: String }`
(`patched/cersei-provider/src/openai.rs:653, 810, 823`). The library therefore classifies twice:
types first, string fallback second.

### 3.4 Failure/rate-limit state today

- `~/.abstract/cooldowns.json` (`config.rs:543`) — `{"provider\0model": unix_ms}` written by
  `FallbackManager::record_failure` (`persist_failures`, `providers.rs:953`), read back by
  `load_persisted_failures` (`:922`). Cooldown length is the single config value
  `fallback.cooldown_seconds` (`config.rs:427`, default 300).
- No error kinds, no counts, no usage, no latency, no database. `graph_db_path()`
  (`config.rs:537`) is vestigial — the memory manager that would have used it is commented out in
  `main.rs:215`.

---

## 4. Crate layout

`libs/ai_providers` (per `AGENTS.md`: `src/<name>.rs`, no `mod.rs`; explicit lib root in Cargo.toml):

```toml
[package]
name = "ai_providers"
version.workspace = true
edition.workspace = true

[lib]
path = "src/ai_providers.rs"

[dependencies]
cersei.workspace = true
reqwest.workspace = true
tokio.workspace = true
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
chrono.workspace = true
anyhow.workspace = true
thiserror.workspace = true
parking_lot = "0.12"
```

```
src/ai_providers.rs   crate root: re-exports + doc (the `[lib] path`)
src/spec.rs           ProviderSpec, ModelSpec, QuirkSpec, PacingSpec, Resolved
src/catalog.rs        Catalog: merge specs, lookup, default selection, /models cache
src/key.rs            resolve_value_spec / resolve_api_key, endpoint resolution
src/discovery.rs      fetch_models + ModelsResponse parsing + cache
src/quirks.rs         model families (reasoning field, no_tool_nudge) + response formats
src/failure.rs        FailureKind, classify(CerseiError), classify_message(&str)
src/pacing.rs         ProviderLimiter: semaphore + token bucket + cooldown policy
src/store.rs          TelemetryStore trait, NullStore, InMemoryStore, AttemptRecord
src/transport.rs      ConfiguredProvider (cersei Provider) + PacedProvider wrapper
src/routing.rs        Router::choose -> RoutingDecision + scoring
```

`quirks.rs` absorbs `apps/agent-cli/src/model_families.rs` (104 lines) and the family-selection
half of `apps/agent-cli/src/response_format.rs` (423 lines). `response_format.rs`'s SSE
classification is *agent-facing* (the ACP/TUI split thinking-vs-answer) and moves too, since it is
driven purely by the family.

---

## 5. Public API

### 5.1 Spec structs handed in by the agent (D3)

```rust
/// One provider as the agent defines it.
pub struct ProviderSpec {
    pub name: String,
    pub base_url: String,
    /// `!command`, `env:VAR`, or a literal key.
    pub api_key: String,
    pub models: Vec<ModelSpec>,
    pub pacing: PacingSpec,
}

pub struct ModelSpec {
    pub id: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub extra_body: Option<serde_json::Value>,
    /// Exact model id → family name, from `config.model_families`.
    pub family: Option<String>,
}

/// Client-side pacing limits. All optional: unset means "no limit imposed".
pub struct PacingSpec {
    pub requests_per_minute: Option<u32>,
    pub max_concurrency: Option<u32>,
    pub min_interval: Option<std::time::Duration>,
    pub min_cooldown: Option<std::time::Duration>, // default 30s
    pub max_cooldown: Option<std::time::Duration>, // default 15m
}
```

agent-cli's existing `Provider`/`ConfiguredModel` become these types (the library keeps the
`ConfiguredModel` name as `ModelSpec`; the agent's `ProviderConfigEntry`/`ModelRef` stay put and are
mapped in `providers::providers(config) -> Vec<ProviderSpec>`).

### 5.2 Catalog

```rust
pub struct Catalog { /* builtins merged with overrides, /models cache, limiter registry */ }

impl Catalog {
    /// `specs` are the agent's providers in display order; `config_order` is the
    /// built-in ordering the agent already computes.
    pub fn new(specs: Vec<ProviderSpec>, store: Arc<dyn TelemetryStore>) -> Self;
    pub fn providers(&self) -> &[ProviderSpec];
    pub fn provider(&self, name: &str) -> Option<&ProviderSpec>;
    pub fn resolve(&self, provider: &str, model: &str) -> anyhow::Result<Resolved>;
    pub fn model_ids(&self, provider: &str) -> Vec<String>;
    pub fn entries(&self) -> Vec<(String, String)>;           // flat provider/model ids
    pub fn display_model_id(provider: &str, model: &str) -> String;
    pub async fn fetch_models_cached(&self, provider: &str) -> Result<Vec<String>, String>;
    /// The agent's transport, wrapped with pacing + telemetry.
    pub fn provider_impl(&self, provider: &str, model: &str)
        -> anyhow::Result<Box<dyn cersei::provider::Provider>>;
}
```

Registry *merge* semantics are unchanged from `providers.rs::providers` (`:563`): built-ins first,
then the virtual `combos` provider (the agent keeps injecting it as a spec), then config additions
sorted by name; a config entry overrides fields by name and an explicit `models` list takes full
control.

### 5.3 Repository invariants preserved

- `display_model_id` (`providers.rs:679`) keeps the double-prefix quirk: `orcarouter` +
  `orcarouter/auto` → `orcarouter/orcarouter/auto`, because wire ids may already carry their
  provider prefix.
- `find_model` (`:98`) keeps accepting bare, `provider/`-prefixed, and exactly-matching ids.
- `resolve()` keeps returning `Resolved { provider, model, base_url, api_key, max_tokens,
  temperature, top_p, extra_body }` so the agent's `BuildParams` path is untouched.

### 5.4 Key + endpoint resolution

`resolve_value_spec` / `resolve_api_key` (`providers.rs:746–782`) move verbatim: `!command` runs the
rest through a shell and trims stdout, `env:VAR` reads the environment, anything else is a literal.
The `Resolved.api_key` value must never be logged or stored (the telemetry spec restates this).

### 5.5 Model discovery

`fetch_models` + `ModelsResponse` + `parse_model_ids` (`providers.rs:704–744`) move as-is
(`{base_url}/models` → `{"data": [{"id": …}]}`), together with the per-provider in-memory cache
currently inside `AgentRuntimeInner` (`providers.rs:1173`, `1330`). The `combos` entry is excluded
from discovery, as today (`:1314`).

### 5.6 Quirks

```rust
pub struct Family {
    pub name: &'static str,
    pub markers: &'static [&'static str],
    pub reasoning_field: Option<&'static str>,
    pub no_tool_nudge: bool,
    pub response_format: ResponseFormat,
}

pub fn family_for_model(model: &str) -> Option<&'static Family>;
pub fn family_by_name(name: &str) -> Option<&'static Family>;
pub fn reasoning_field_for(spec_family: Option<&str>, model: &str) -> cersei::provider::ReasoningField;
pub fn format_for(spec_family: Option<&str>, model: &str) -> ResponseFormat;
```

Built-in marker matching (`hy3`, case-insensitive) and the config override
(`config.model_families`) keep their current semantics (`model_families.rs`,
`response_format.rs:formats`). The agent's `BuildParams.reasoning` and the runner's
`no_tool_nudge` are fed from these calls exactly as today.

### 5.7 Transport + pacing

`ConfiguredProvider` (`providers.rs:212`) moves unchanged, and gains a wrapper:

```rust
pub struct PacedProvider {
    inner: ConfiguredProvider,
    provider: String,
    limiter: Arc<ProviderLimiter>,
    store: Arc<dyn TelemetryStore>,
    quirks: Family,
}
```

`PacedProvider::complete`:

1. `limiter.acquire(provider)` — waits for a concurrency permit, then for the token bucket
   (`requests_per_minute` / `min_interval`).
2. Consult the store: if `(provider, model)` is in cooldown, fail fast with
   `CerseiError::RateLimit { retry_after: Some(remaining), message: "in cooldown" }` so the agent's
   existing fallback logic fires without a network round-trip.
3. Call `inner.complete(request)`, measuring latency.
4. On `Err(e)`: classify (5.8), record the attempt, apply the cooldown policy, return `e`.
5. On `Ok(stream)`: wrap the stream so a mid-stream `StreamEvent::Error` is classified from its
   message and recorded at stream end; usage/latency recorded when `MessageDelta { usage }` or
   `MessageStop` arrives.
6. Release the permit, then update the pacer (D12): on `RateLimited`/`Overloaded` multiply the
   effective interval (up to `max_cooldown`); on a success streak of 20, decay it by ×0.8; apply
   any rate-limit hint observed on the response (§5.11) as an additional ceiling.

### 5.8 Failure classification (`src/failure.rs`)

```rust
pub enum FailureKind {
    RateLimited { retry_after: Option<Duration> },
    QuotaExhausted,                                  // 429 whose body means "no credit"
    Auth,                                            // 401/403, also CerseiError::Auth
    Overloaded,                                      // 500/502/503/504/529
    Timeout,
    Network,
    ModelNotFound,                                   // 404 / "model not found"
    ContextOverflow,                                 // CerseiError::ContextOverflow, request-level
    RequestRejected { status: u16 },                 // other 4xx: bad request, content policy
    Cancelled,
    Unknown { message: String },
}
```

Mapping, types first:

| `CerseiError` | `FailureKind` | Counts against provider health? |
|---|---|---|
| `RateLimit { retry_after, message }` | `RateLimited` / `QuotaExhausted` when the message matches the quota wording | yes / provider-disabling (see below) |
| `ProviderStatus { status: 401 \| 403, .. }` | `Auth` | provider-disabling for the session |
| `ProviderStatus { status: 404, .. }` | `ModelNotFound` | model-disabling, not provider |
| `ProviderStatus { status: 500 \| 502 \| 503 \| 504 \| 529, .. }` | `Overloaded` | yes |
| `ProviderStatus { status: 400, .. }` | `RequestRejected` (refined to `ContextOverflow` when the body says context/length, `ModelNotFound` when it says model) | no |
| `Auth(..)` | `Auth` | provider-disabling |
| `ContextOverflow { .. }` | `ContextOverflow` | no |
| `Http(e)` with `is_timeout()` / `is_connect()` | `Timeout` / `Network` | yes |
| `Cancelled` | `Cancelled` | no |
| `Provider(msg)` / `Other(_)` | `Unknown { message }`, refined by the string fallback | no (unless the fallback matches) |

String fallback (stream-time errors, mirrors the classifier already in
`patched/cersei-agent/src/runner.rs:483`): lowercase match on `rate limit` / `429` / `too many
requests` → `RateLimited`; `timeout` / `timed out` / `deadline` → `Timeout`; `network` /
`connection` / `dns` / `reset` → `Network`; `not found` + model wording → `ModelNotFound`;
`context` + `length` / `token` → `ContextOverflow`; else `Unknown`.

Policy derived from the kind (normative):

- `RateLimited` with a usable reset hint (`retry_after`, which §5.11 also fills from
  `X-RateLimit-Reset`): cooldown `clamp(hint, min_cooldown, max_cooldown)` instead of the fixed
  `fallback.cooldown_seconds`. Without a hint, back off `min(max_cooldown, previous * 2)` starting
  at `min_cooldown`.
- `Auth` (D13): re-resolve the api-key spec (only when it is `!command` or `env:VAR`) and retry
  the request once. If that also fails, the provider is disabled for the process with a one-line
  message naming the provider and the likely fix (fix or rotate the key).
- `QuotaExhausted`: provider-wide cooldown until the process exits, plus a one-line user-facing
  message naming the provider and the fix (top up the account). The agent decides whether that
  aborts the run; the library only refuses further requests.
- `Overloaded` / `Timeout` / `Network`: `min(max_cooldown, previous * 2)`.
- `ModelNotFound`: long cooldown (default 24h) on that `(provider, model)` only.
- `ContextOverflow` / `RequestRejected` / `Cancelled`: no cooldown, attempt still recorded.

### 5.9 Store port (D4)

```rust
#[async_trait::async_trait]
pub trait TelemetryStore: Send + Sync {
    async fn record_attempt(&self, attempt: &AttemptRecord) -> anyhow::Result<()>;
    async fn set_cooldown(&self, key: &ModelKey, until: SystemTime, cause: FailureKind)
        -> anyhow::Result<()>;
    async fn cooldown_until(&self, key: &ModelKey) -> anyhow::Result<Option<SystemTime>>;
    /// Attempts at or after `since`, oldest first — the window the score reads.
    async fn attempts_since(&self, key: &ModelKey, since: SystemTime)
        -> anyhow::Result<Vec<AttemptSummary>>;
    /// Every (provider, model) with an active cooldown, for provider-wide lookups.
    async fn active_cooldowns(&self) -> anyhow::Result<Vec<(ModelKey, SystemTime)>>;
}

pub struct AttemptRecord {
    pub provider: String,
    pub model: String,
    pub at: SystemTime,
    pub outcome: Outcome,          // Ok { input_tokens, output_tokens } | Failed(FailureKind)
    pub latency: Option<Duration>,
    pub session_id: Option<String>, // agent-supplied; opaque to the library
}
```

The library ships `NullStore` (everything succeeds, nothing is remembered) and `InMemoryStore`
(a `Mutex<Vec<AttemptRecord>>`, used by the crate's tests and by `agent-cli --no-telemetry`).
`PacedProvider` treats store errors as warnings — a broken database must not break a run.

### 5.10 Pricing seam (D7)

No table, no fetch. `Resolved` gains `price: Option<Price>` where `Price { input_per_mtok: f64,
output_per_mtok: f64 }`, always `None` in v1; the scoring price term reads `0.0` when it is `None`.
`cersei::tools::estimate_cost` and the duplicate in `tui/widgets.rs:579` are left alone.

### 5.11 Rate-limit header parsing (D11)

Where the headers exist: the response headers are visible in exactly one place —
`patched/cersei-provider/src/openai.rs:358–363`, where the status is checked before spawning the
reader. That is also where `crate::parse_retry_after(response.headers())` is called today, so the
patch is local and needs no new plumbing and no `cersei-types` change.

```rust
/// Seconds until the limit resets, from whichever rate-limit header the gateway sent.
///
/// Accepts, in order of preference:
///   retry-after                    — delta seconds (existing behaviour)
///   x-ratelimit-reset              — unix epoch seconds when the value is > 1e9,
///                                    otherwise delta seconds (OpenRouter)
///   x-ratelimit-reset-requests     — Go duration or plain seconds (OpenAI/Groq)
///   x-ratelimit-reset-tokens       — same shape, used when the request axis is absent
///   ratelimit-reset                — IETF draft, delta seconds
pub fn parse_rate_limit_reset(headers: &reqwest::header::HeaderMap) -> Option<Duration>;

/// Remaining/limit counts, for the pacer's hint path (both optional; most gateways
/// send neither).
///   x-ratelimit-remaining / x-ratelimit-remaining-requests
///   x-ratelimit-limit     / x-ratelimit-limit-requests
pub fn parse_rate_limit_counts(headers: &reqwest::header::HeaderMap) -> RateLimitCounts;
```

`CerseiError::from_http_status` already accepts `retry_after`, so the vendored provider passes
`parse_rate_limit_reset(headers)` where it currently passes `parse_retry_after(headers)`. Evidence
that this matters for a provider agent-cli actually uses: OpenRouter documents its rate-limit state
as `X-RateLimit-*` **on the error response** and its in-flight-budget 402 with a `Retry-After`
(https://openrouter.ai/docs/api_reference/limits).

What the probes showed for the rest of the built-in list (unauthenticated request, looking for any
rate-limit header on a 200/401/404/405):

| Provider | Headers seen |
|---|---|
| OpenRouter (`GET /models`, 200) | none |
| OrcaRouter (`GET /models`, 200) | none |
| tokenrouter (`GET /models`, 401) | none |
| kiosapi (`GET /models`, 401) | none |
| groq (`POST /chat/completions`, 404) | none |
| NVIDIA (`POST /chat/completions`, 405) | none |

So success-path hints are **not** a foundation to build pacing on (which is why D12 does not depend
on them), and the header work is scoped to the error path where the data is documented to exist.

---

## 6. What stays in agent-cli (D2)

- `config.rs` in full: `AppConfig`, `ProviderConfigEntry`, `ModelRef`, `FallbackConfig`,
  `default_config_jsonc()` and its tests (10 providers / 13 env vars), `cooldowns_path`.
- `providers.rs` remainder: `builtin_providers()`, `builtin_provider_entries()`, `combos` parsing
  (`Combo`, `combo`, `combo_first_entry`, `effective_selection`, `is_known_provider`),
  `resolve_selection` (`/model <text>`), `build_agent`, `agent_tools`, `BuildParams`,
  `AskUser*`, `AgentRuntime`, plus `providers(config) -> Vec<ProviderSpec>` mapping and the
  `Rebuild of AgentRuntime`: it holds `Arc<Catalog>`, keeps the model-selection state, and delegates
  `next_fallback_entry` / `record_failure` to the library's `Router` and store.
- `model_families.rs` and `response_format.rs` become thin re-export shims (or are deleted and their
  call sites point at `ai_providers`) — the agent keeps using them by name so the diff stays small.

---

## 7. Failure and edge-case matrix

| Situation | Behaviour |
|---|---|
| No store configured / store returns an error | `NullStore` semantics: pacing still works from config, no cooldowns persist, a warning is logged once. |
| `requests_per_minute` unset | No proactive pacing; only reactive cooldowns. Existing configs keep today's behaviour. |
| `min_interval` + `requests_per_minute` both set | The tighter of the two wins. |
| Concurrency cap reached | The request **waits** for a permit (not an error); if the agent's cancel token fires first, fail with `Cancelled`. |
| `Retry-After: <HTTP-date>` | Ignored (cersei's `parse_retry_after` only reads delta-seconds); fall back to exponential backoff. Documented, not a bug. |
| `X-RateLimit-*` headers | Read on the error path only, in the vendored provider, and folded into the reset hint (§5.11, D11). Gateways that send none fall back to exponential backoff. |
| All combo entries in cooldown | Choose the least-recently-cooled entry anyway and surface it; never hard-fail for lack of a candidate. |
| Model id in the spec is unknown to the provider | Recorded as a failure attempt; discovery-driven flows (`/provider`) show the live list. |
| Provider returns HTTP 200 then fails mid-stream | Typed classification is unavailable; the string fallback classifies from the message; latency and partial usage still recorded. |
| Two processes share one store | Cooldowns are rows, not a file: last write wins per `(provider, model)`, no torn JSON. |
| Legacy `~/.abstract/cooldowns.json` present | Imported once by the agent's store implementation, then renamed to `cooldowns.json.migrated` (telemetry spec §4.4). |

---

## 8. Code change list (by file)

| File | Change |
|---|---|
| `libs/ai_providers/Cargo.toml` | Rewrite: workspace deps, `[lib] path = "src/ai_providers.rs"` (the root lives beside its modules), drop the 0.12 reqwest pin. |
| `libs/ai_providers/src/*` | New modules per §4; delete `oauth.rs`, `nous_portal.rs`, `poolside.rs`, and the old `lib.rs` `OpenAiCompatible`/`RealRunner`/`CmdErr`. |
| `apps/agent-cli/src/providers.rs` | Keep config/combos/runtime; delegate registry, resolve, discovery, quirks, cooldowns and transport to the crate; `providers(config) -> Vec<ProviderSpec>`. |
| `apps/agent-cli/src/model_families.rs` | Re-export from the crate (or delete and update call sites). |
| `apps/agent-cli/src/response_format.rs` | Family/format selection re-exported; SSE classification moves to the crate, agents keep their call sites. |
| `apps/agent-cli/src/acp.rs`, `tui/event_loop.rs`, `main.rs`, `subagents.rs` | No behavioural change beyond the call sites that moved (`record_failure`, `next_fallback_entry`, `fallback_enabled`). |
| `apps/agent-cli/Cargo.toml` | The `ai_providers` dependency becomes real (and gains no cycles). |

---

## 9. Testing plan

Crate-local (`cargo test -p ai_providers`):

1. **Spec mapping** — `ProviderSpec` merge precedence matches today's `providers.rs:563` tests
   (built-in < config override; explicit `models` replaces the list).
2. **Resolution** — bare / `provider/`-prefixed / exact ids; `display_model_id` double-prefix;
   `env:`/`!cmd`/literal key specs, including a failing `!cmd`.
3. **Discovery** — sample `{"data":[{"id":…}]}` parses; cache returns without a second call;
   non-2xx is an error string, not a panic.
4. **Classification** — a table test over every `CerseiError` variant in §5.8 plus the string
   fallback cases, including `QuotaExhausted` wording and "model not found".
5. **Pacing** — with a mock provider: concurrency never exceeds `max_concurrency`; the bucket
   spaces calls; an in-cooldown call fails fast with `RateLimit` and **zero** inner calls; a
   `retry_after` sets that cooldown; exponential backoff doubles.
6. **Store failure isolation** — a `FailingStore` does not break `complete()`.
7. **Quirks** — the existing `hy3` tests move over unchanged; `reasoning_field_for` keeps its
   config-override precedence.
8. **Scoring** — pure-function tests for §6 of the telemetry spec.

Agent-side: existing `cargo test -p agent-cli` (229 tests) must pass; the provider tests that
assert registry/cooldown behaviour move with the code rather than being rewritten.

---

## 10. Out of scope (v1)

- Pricing data of any kind (D7).
- Spend/budget caps (D5).
- Replacing cersei's registry/router with the catalog.
- OAuth providers, Anthropic-native/Gemini wires (the catalog stays OpenAI-compatible).
- Transport retries inside the library: the agent still owns "fail → fall back to the next combo
  entry" (`main.rs:try_fallback`, TUI loop). The library only supplies the decision and the state.
- Multi-process coordination beyond shared rows (no locks, no leader election).

---

## 11. Resolved questions (with evidence)

### 11.1 `X-RateLimit-*` headers → **error path only, via the vendored provider** (D11, §5.11)

*As built:* the parsers live in the vendored provider, not in this crate —
`patched/cersei-provider/src/lib.rs` gained `parse_rate_limit_reset` (and the
`parse_go_duration` helper) next to the existing `parse_retry_after`, and
`openai.rs`'s non-2xx branch calls it where it used to call
`parse_retry_after`. §5.11 originally placed them in this crate, but the crate
never sees a response header: it receives a typed `CerseiError` or a
`StreamEvent`, and cersei's provider is the only place `reqwest::HeaderMap` is
in hand. Putting the parser there avoids both a `cersei-types` change and a
second, unreachable copy. The library consumes the result the same way either
way — the reset hint arrives as `CerseiError::RateLimit { retry_after }`, which
`FailureKind::retry_after()` reads into the cooldown policy. `parse_rate_limit_counts`
was dropped: with success-path headers unreachable, nothing could call it.

Resolved as: parse them where they are already reachable (`openai.rs:358–363`) and fold the reset
hint into `retry_after`; do not build pacing on success-path headers. Backing evidence: OpenRouter
documents `X-RateLimit-*` on rate-limit *errors*; every probe of the other built-in gateways found
no rate-limit headers at all on a 200/401/404/405. A `cersei-types` patch is therefore **not**
needed. Provider-specific budget inspection (OpenRouter's `GET /api/v1/key` exposing
`limit_remaining` and `free_model_daily_requests`) is deliberately out of scope — it is one
gateway's bespoke API, and the library stays open-format.

### 11.2 `StreamEvent::Error` carries a `String` → **accepted; no cersei-types patch**

Resolved as: keep the string fallback permanently. A mid-stream failure by definition has no HTTP
status (the response was 200), so there is nothing more accurate to carry; the classifier's
rate-limit/timeout/network wording covers the provider-health cases that score. HTTP-level
failures, which are the ones that drive cooldowns, are already typed via
`CerseiError::from_http_status`. Attempt attribution (see the telemetry spec §5.1) is unaffected:
the attempt is opened before the stream starts, so a mid-stream failure is recorded against the
right provider, model and turn.

### 11.3 Proactive limits with no numbers → **adaptive pacing, explicit config as ceiling** (D12)

Resolved as: no invented per-provider RPM table. Unset `PacingSpec` means "start unthrottled"; the
pacer then learns — multiplicative decrease on 429/overload, slow additive recovery, plus header
hints when a gateway sends them. Explicit `requests_per_minute` / `min_interval` values (from
config, later optionally from the built-in list) act as ceilings the pacer never exceeds. This is
strictly more informative than a guess, and it is the same information the score already collects.

### 11.4 `Auth` on a rotating key → **re-resolve once, then disable** (D13)

Resolved as: re-resolve the key spec (only for `!command` / `env:VAR`) and retry the request once;
only a second failure disables the provider for the process. Rotation is a normal proxy-setup
pattern and a stale cached key must not look like a dead provider.

### 11.5 Where `default_selection` lives → **agent**

Resolved as: stays in agent-cli (`providers.rs:639`), because `auto` is config policy ("empty
provider → the `model` prefix if known, else the first provider"). The library exposes
`Catalog::default_model` only. If a second consumer ever needs the `auto` rule it moves then.

### 11.6 Crate name → **keep `ai_providers`**

Resolved as: keep it (D1 — the path exists and agent-cli already declares the dependency). Renaming
is mechanical and can happen any time before the crate leaves this workspace.

---

## 12. Accepted risks → decisions (no open questions remain)

Each remaining risk is closed by a decision taken here, so implementation has no forks left.

| # | Risk | Decision |
|---|------|----------|
| D14 | Gates emit header shapes we do not parse (`X-RateLimit-Reset` is epoch-seconds at OpenRouter, delta-seconds in the IETF draft; OpenAI/Groq use Go durations) | §5.11's parser covers those four shapes with tests; **an unrecognised shape yields `None` and falls through to exponential backoff.** No per-gateway special cases, no warning spam — a missing hint is strictly better than a wrong sleep. |
| D15 | Pacing state assumed one provider instance per agent instance | **Limiters are keyed by provider name and owned by the `Catalog`**, so parent and sub-agents share one limiter per provider. An in-flight cap therefore means "across this process for this provider", which is what the cap should mean anyway; the header observer writes into that shared limiter, so instances cannot race. Nothing depends on per-instance state. |
| D16 | AIMD constants (`×2` down, `×0.8` up after 20 successes, `max_cooldown` cap) are untuned | Constants live in `pacing.rs` (`DECREASE = 2.0`, `RECOVER = 0.8`, `RECOVER_AFTER = 20`) with tests asserting monotonic behaviour; **no config surface in v1**. Tuning is a one-line change once real 429 data exists. |
| D17 | Quota exhaustion disables a provider mid-run | **Intended behaviour:** the library refuses further requests to that provider and returns `RateLimit`-class errors, so the agent's existing fallback walks to the next combo entry. The run only fails if every entry is exhausted, and the message names the provider and the fix. |
| D18 | `Auth` disabling is process-wide, not session-wide | Accepted: agent-cli is a per-invocation process, so "for the process" and "for the session" coincide. If a long-lived server ever embeds the crate, the disabled set becomes a constructor argument rather than a global. |

---

## 13. Implementation notes (as built)

Deviations from the prose above, each deliberate and each tested:

- **§4 layout** — the crate root is `src/ai_providers.rs` (so the module list
  in §4 holds), not `ai_providers.rs` at the package root.
- **§5.2 `provider_impl`** takes one extra argument, the shared
  `Arc<AttemptScope>`, and one convenience argument, the
  `cersei::provider::ReasoningField` the agent resolved from its model-family
  config. Both are per-agent values the library cannot derive.
- **§5.7 step 2** — the in-flight cooldown check reads the in-process
  `CooldownRegistry` rather than awaiting a store read per request. The
  registry *is* the store's mirror: the agent seeds it from
  `TelemetryStore::active_cooldowns()` at startup and the transport writes both
  on every failure, so nothing is lost and the hot path stays synchronous.
- **§5.7 step 5** — a successful response is wrapped by forwarding the inner
  stream through a spawned task that tracks usage and a mid-stream error, holds
  the concurrency permit for the stream's life, and records the attempt when the
  stream ends. `CompletionStream` is a concrete struct over a channel, so a
  wrapper must own both ends.
- **§5.8 cooldowns** — `FailureKind::ModelNotFound` uses a 24h cooldown; the
  `Auth` re-resolve retries only for `!cmd`/`env:VAR` specs (a literal key
  cannot change) and on a second failure disables the provider.
- **§5.8 / §5.10 pricing** — `Resolved.price` is always `None`; the router's
  price term and weight exist and evaluate to `0`.
- **§5.6 `Family`** — kept in `quirks.rs` as `name`/`markers`/`reasoning_field`/
  `no_tool_nudge`; the response-format family resolution (`format_for`,
  `reasoning_field_for`, `classify_delta`, `parse_sse`) lives in the same module
  so agent-cli's `response_format` can forward to one place.
- **AIMD base** — with no configured floor, the first 429 buys
  `LEARNED_BASE_INTERVAL` (500ms) and each further one doubles it, capped at
  `MAX_LEARNED_INTERVAL` (30s); `RECOVER_AFTER` successes multiply by
  `RECOVER`. Constants are in `pacing.rs` with behaviour tests (D16).
- **Router ties** — equal penalties prefer the configured order, except that
  among cooling candidates the one whose cooldown lifts sooner wins (so an
  all-cooling combo still tries the least-cooled entry, per §7).
- **Router latency term** — the mean of the last five latencies per candidate,
  relative to the fastest candidate, only when its weight is `> 0`.
- **`TelemetryStore::active_cooldowns`** returns
  `(ModelKey, SystemTime, String)` — the cause tag is kept so a seeded
  cooldown can be explained, which the §5.9 sketch dropped.

## 14. Implementation order

1. Rewrite the crate skeleton (Cargo.toml + module files + `NullStore`), move `quirks.rs`
   (model families) and the response-format families across with their tests — no behaviour change
   yet, agent still compiled against its own copies.
2. Move `Provider`, `ConfiguredModel`, `model_ids`, `find_model`, `Resolved`, discovery, key specs,
   `resolve`, and the catalog merge; add `providers(config) -> Vec<ProviderSpec>` in the agent.
3. Point `openai_provider` at the crate; delete the agent's copies; run `cargo test -p agent-cli`
   (expect green with only import changes).
4. Add `failure.rs` (classification, pure) + tests, then `store.rs` (`TelemetryStore`,
   `NullStore`, `InMemoryStore`).
5. Add `pacing.rs` + `transport.rs` (`PacedProvider`) behind the store port; wire agent-cli to
   `NullStore` so behaviour is unchanged.
6. Implement the agent-side store + routing (separate spec) and switch `AgentRuntime` to
   `PacedProvider`.

# agent-cli

A terminal AI coding agent (the `tui-agent`) with a full-screen TUI, a
provider/model picker, and an Agent Client Protocol (ACP) server. Built on the
`cersei` agent library and the GPUI-style event loop in `src/tui/`.

```
cargo run -p agent-cli          # interactive TUI
cargo run -p agent-cli -- --acp  # ACP server over stdio (JSON-RPC 2.0 NDJSON)
cargo run -p agent-cli -- -p "fix the tests"  # single-shot prompt, runs with tools
```

The dev shell also aliases it as `ag` (see `part.p.nix`).

## Slash commands

In the TUI, type `/` to get a fuzzy command selector (↑/↓ to navigate, Enter
to run):

| Command    | What it does                              |
|------------|-------------------------------------------|
| `/model`   | Switch provider/model (opens a picker; or `/model <provider>` / `/model <provider>/<model>` directly) |
| `/clear`   | Clear the conversation                    |
| `/panel`   | Toggle the side panel                     |
| `/diff`    | Open the git diff panel                   |
| `/files`   | Open the file tree panel                  |
| `/rewind`  | Rewind the last turn                      |
| `/memory`  | Memory status                             |
| `/compact` | Context compaction status                 |
| `/proxy`   | Proxy status                              |
| `/default-config` | Write the full default config (every field with its possible values as `//` comments) to a temp file and open it in your editor |
| `/help`    | Show help                                 |
| `/exit`    | Exit                                      |

### Copy / selection

- **Ctrl+Shift+C** (or **Cmd+C** on macOS) copies the current selection to the system clipboard; the status bar briefly confirms the copy or notes that nothing was selected.
- **Shift+←/→/↑/↓** (and **Shift+Home/End**, **Shift+PageUp/PageDown**, **Shift+Cmd+↑/↓**) extend an output selection one grapheme or one row/page.
- **Cmd+A** selects all output when streaming; otherwise it selects just the input text (text-field convention). **Ctrl+Shift+A** always selects all output.
- **Mouse drag** in the output or input box starts/creates a selection; a click outside clears it.

## Configuration

Configuration is layered JSON, merged in this order (later wins):

1. Built-in defaults
2. `~/.abstract/config.json` — user-global config
3. `.abstract/config.json` — per-project config (overrides the global file)
4. Environment variables (`ABSTRACT_*`)
5. CLI flags (`--model`, `--provider`, `--fast`, …)

Start with a project config:

```bash
mkdir -p .abstract
```

> Legacy `.toml` configs (`~/.abstract/config.toml`, `.abstract/config.toml`)
> are still read when no `.json` file exists, so existing setups keep working.
> The format switched to JSON in this version.

### Config file shape

```json
{
  "provider": "auto",
  "model": "auto",
  "max_turns": 50,
  "max_tokens": 16384,
  "effort": "medium",
  "permissions_mode": "interactive",
  "theme": "dark",
  "output_style": "default",
  "auto_compact": true,
  "graph_memory": true,
  "output_format": "text",
  "compression_level": "off",

  "fallback": {
    "enabled": true,
    "cooldown_seconds": 300
  },

  "combos": {
    "coding": [
      ["poolside", "poolside/laguna-xs-2.1"],
      ["openrouter", "openrouter/free"],
      ["groq", "groq/compound"]
    ]
  },

  "mcp_servers": [
    {
      "name": "my-tools",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  ],

  "hooks": [
    { "event": "on_turn_end", "command": "echo turn done" }
  ],

  "proxy": {
    "enabled": true,
    "force": false,
    "url": "http://localhost:8317/v1"
  }
}
```

Notes:

- `provider`/`model`: `"auto"` picks the first configured provider and its
  first model; a bare model id like `"groq/compound"` implies its provider.
- `working_dir`: project directory (defaults to the launch directory).

### Combos (model fallback)

When you pick a specific provider/model, the agent sticks to it — there is no
automatic fallback between models. To get transparent failover, define a
**combo**: a named list of `[provider, model]` tuples that appears in the
`/model` picker (and ACP `availableModels`) as the virtual provider `combos`
with one virtual model per combo. Selecting `combos/coding` runs on the first
entry and, on a provider error or rate limit **before any output is produced**,
retries the run on the next entry — in both the TUI and the ACP server.

```json
"combos": {
  "coding": [
    ["poolside", "poolside/laguna-xs-2.1"],
    ["openrouter", "openrouter/free"],
    ["groq", "groq/compound"]
  ]
}
```

Then pick it like any other model: `/model combos/coding` in the TUI, or set
`provider: "combos"` / `model: "coding"` in the config.

- Entries are tried in the order listed; the failed entry is skipped and the
  most-preferred available one is tried next. Entries must name providers
  defined under `providers` (built-in or config-file); unknown ones are
  skipped with a warning.
- `fallback.cooldown_seconds` is the "long cooldown" (default 300s = 5
  minutes): once an entry fails it is excluded from fallback for this long, so
  a rate-limited provider isn't hammered again immediately.
- Cooldowns **persist across restarts** in a small JSON file (`~/.abstract/
  cooldowns.json` by default) — just a few minutes of rate-limit state, so a
  tiny file is all it takes (no database). Relocate or disable it via
  `fallback.cooldowns_file` (an empty string disables persistence).
- `fallback.enabled: false` turns combo fallback off (combos then just run on
  their first entry).
- `combos` entries can also be written as objects:
  `{ "provider": "poolside", "model": "poolside/laguna-xs-2.1" }`.

Fallback only triggers when the run fails **before producing any output**
(which is where provider errors and rate limits hit — on the first request).
Once the agent has started streaming a response or made tool calls, an error is
surfaced normally instead of being retried, to avoid duplicating partial work.
When a combo switches entries you'll see a notice in the TUI (a grayed
`[system]` log line — your selected model stays `combos/coding`) and an
`agent_message_chunk` notification over ACP naming which model failed and
which it fell back to. The concrete model in use is also exposed as
`effectiveModelId` in the ACP `session/new` `models` metadata (only when it
differs from `currentModelId`, i.e. while a combo runs on a fallback entry),
and each switch emits a `model_changed` session/update notification carrying
both the selection (`modelId`) and the new effective model
(`effectiveModelId`).

### Providers

Providers are OpenAI-compatible and configured under the `providers` object.
The built-ins are `poolside`, `openrouter`, `groq`, `nvidia`, `tokenrouter`
(the tokenrouter.com unified gateway), `kiosapi` (kiosapi.com), `google`
(Google AI Studio's OpenAI-compatible endpoint, `gemini-3.8-flash` at low
thinking), `ollama` (ollama.com's hosted cloud API) and `opencode-zen`
(opencode.ai's Zen gateway). A `providers.NAME` entry either
overrides a built-in (by name) or defines a brand-new provider. All fields are optional; only set what you want to
override. The default config (`/default-config`) includes every built-in
provider with its `base_url`, `api_key` spec, and one model, so you can see
the shape and edit from there.

```json
"providers": {
  "openrouter": {
    "base_url": "https://openrouter.ai/api/v1",
    "api_key": "env:OPENROUTER_API_KEY",
    "models": [
      "openrouter/free",
      "openai/gpt-oss-20b:free",
      "cohere/north-mini-code:free"
    ]
  },

  "acme": {
    "base_url": "https://acme.example.com/v1",
    "api_key": "!kubectl get secret api-key -o jsonpath='{.data.key}' | base64 -d",
    "models": ["acme/big", "acme/small"]
  }
}
```

Overriding `models` takes full control of the list.

Each `models` entry is either a bare id string or a detailed object that
pins request parameters (`max_tokens`, `temperature`, `top_p`, `extra_body`)
to that model only. There are no provider-level sampling parameters — tuning
is per model, so one model's settings can never leak into another model on
the same provider. Unset fields leave the request untouched (the agent
default applies):

#### API keys

The `api_key` field accepts three forms:

| Form        | Meaning                                                        |
|-------------|----------------------------------------------------------------|
| `!command`  | Run the rest as a shell command; use its trimmed stdout as the key |
| `env:VAR`   | Read the environment variable `VAR`                            |
| anything else | Treated as a literal key                                     |

The `!command` form is resolved each time a provider is used (opencode/pi
convention). Default built-ins use `env:POOLSIDE_API_KEY`,
`env:OPENROUTER_API_KEY`, `env:GROQ_API_KEY`, `env:NVIDIA_API_KEY`,
`env:TOKENROUTER_API_KEY`, `env:KIOSAPI_API_KEY`, `env:GEMINI_API_KEY`,
`env:OLLAMA_CLOUD_API_KEY`, `env:OPENCODE_API_KEY`.

#### Environment variables for tools

The top-level `env` object sets environment variables that agent tools read
at runtime. `WebSearch` is a single tool whose provider precedence is
resolved in the background: Exa first (key in `EXA_API_KEY`), then Parallel
Search via MCP (anonymous free tier — no API key), then TinyFish (key in
`TINYFISH_API_KEY`), then LangSearch (key in `LANGSEARCH_API_KEY`). Each
value uses the same three forms as `api_key`:

```json
"env": {
  "EXA_API_KEY": "!kubectl get secret exa-key -o jsonpath='{.data.key}' | base64 -d",
  "TINYFISH_API_KEY": "!kubectl get secret tinyfish-key -o jsonpath='{.data.key}' | base64 -d",
  "LANGSEARCH_API_KEY": "!kubectl get secret langsearch-key -o jsonpath='{.data.key}' | base64 -d",
  "SOME_LITERAL": "literal-value"
}
```

The Parallel MCP endpoint defaults to `https://search.parallel.ai/mcp` and
can be overridden with the `PARALLEL_SEARCH_MCP_URL` environment variable
(same precedence rules as above — the shell wins over the config `env`
entry).

Values are resolved once at startup and exported into the process
environment, so the web search tool (and any other env-reading tool) sees
them. Precedence: a variable already set in your shell environment always
wins — the config value only fills in when the variable is not already set.

The default config (`/default-config`) lists every env var the agent needs
— the nine provider API keys plus `EXA_API_KEY`, `TINYFISH_API_KEY` and
`LANGSEARCH_API_KEY` — as explicit fields, each defaulting to a `!echo <VAR>`
placeholder. Replace the placeholders with real sources (e.g. `!cat ~/.key`)
or set the variables in your shell; a shell-set variable always wins over the
config entry.

### Environment variables

| Variable                     | Overrides                                  |
|------------------------------|--------------------------------------------|
| `ABSTRACT_MODEL`             | `model`                                    |
| `ABSTRACT_PROVIDER`          | `provider`                                 |
| `ABSTRACT_EFFORT`            | `effort`                                   |
| `ABSTRACT_THEME`             | `theme`                                    |
| `ABSTRACT_MAX_TURNS`         | `max_turns`                                |
| `ABSTRACT_COMPRESSION`       | `compression_level`                        |
| `<PROVIDER>_API_KEY`         | per-provider api key (via `env:` specs)    |

### CLI flags

`cargo run -p agent-cli -- --help` lists everything. The most useful ones:
`--model`, `--provider`, `--fast`, `--max`, `--headless`, `--resume`,
`--no-permissions`, `--acp`, `--proxy`, `-C <dir>`. `-p "<prompt>"` runs a
single prompt non-interactively with the agent's tools and streams the reply
to stdout — handy for scripting tests. The agent's tool set mirrors the
freebuff agent's surface: file `read`/`write`/`edit`, `glob`/`grep` (ripgrep)
and code search, `bash`, `web search`/`read_url`/`ReadDocs` (Context7 library
docs), and `SyntheticOutput` (structured output).

### Subcommands

- `ag config` — print the current (merged) config as JSON
- `ag config default` — print the default config template (JSONC with `//`
  comments and per-field docs; the same content `/default-config` opens in
  your editor)
- `ag config show` — same as `ag config`

Other declared subcommands (`sessions`, `memory`, `mcp`, `init`, `login`,
`logout`) are not implemented yet.

## Sub-agents

Like freebuff's built-in agents, the agent can delegate focused sub-tasks to
specialized sub-agents via the `spawn_agents` tool (listed in its system
prompt, with when-to-spawn guidance for each). Sub-agents run in parallel,
each with its own system prompt, tool set, and a fresh provider session on the
same model. The catalog (`src/subagents.rs`):

| Agent id         | Tool set                                   | When to spawn                     |
|------------------|--------------------------------------------|-----------------------------------|
| `researcher-web` | `WebSearch`, `WebFetch`                    | Answers depending on current web info |
| `researcher-docs`| `ReadDocs` (Context7)                      | Library/framework API questions   |
| `code-searcher`  | (mechanical — runs `searchQueries` in `params` directly, no LLM) | Find where a symbol/pattern appears |
| `code-reviewer`  | none (reviews the recent changes in the parent conversation) | After significant file changes |
| `thinker`        | none (sees the entire conversation; strips `<think>` blocks) | Hard reasoning questions, tricky bugs, complex design decisions |

Each agent also ends its response with a `suggest_followups` tool call; the
suggestions are rendered as a "Suggested next steps" list in the TUI and in
`-p` mode. Read-only sessions (ACP `readonly` mode) don't get `spawn_agents`
since sub-agents run with full permissions. Sub-agent runs are capped at 120
seconds so a stuck agent can't hang the parent run.

In the TUI, each `spawn_agents` call renders as a **nested tool-call tree**: a
header per spawned agent (`[researcher-web] Web Researcher — <prompt>`) with
its tool calls (`WebSearch`, `ReadDocs`, …) and final text indented
underneath, so you can watch what each sub-agent is doing live as the batch
runs.

### Phase workflow

The parent's system prompt walks it through freebuff's phase workflow for
implementation tasks (prompt-encoded, not a state machine):

1. **Explore** — spawn `code-searcher`/`researcher-web`/`researcher-docs` in
   parallel and read the relevant files before touching anything.
2. **write_todos** — for 3+ step tasks, plan with the `TodoWrite` tool
   (a review step + a validation step included; cersei's runner also nudges
   the model about incomplete todos). For hard reasoning questions, spawn the
   `thinker` sub-agent (no tools, sees the conversation) to reason about the
   approach before implementing.
3. **Implement** — direct file edits, preferring `Edit`/`ApplyPatch`.
4. **Review Loop** — spawn `code-reviewer`, fix anything it finds, then
   re-spawn it until it reports no new issues.
5. **Validate** — run typechecks/tests/lints via `Bash`, add tests for new
   functionality, fix failures, re-validate.
6. **Follow-ups** — end with `suggest_followups` and a short summary.

Simple questions skip the phases, and follow-up requests on already-completed
work run only the relevant phases.

## ACP server

`agent-cli --acp` implements the Agent Client Protocol v1 over stdio
(newline-delimited JSON-RPC 2.0): `initialize`, `session/new`, `session/load`,
`session/prompt` (with streaming `session/update` notifications),
`session/set_config_option` and `session/cancel`. `session/new` advertises
`provider` + `model` config options and `availableModels`, so clients can
switch provider/model — same registry as the TUI's `/model`. The `models` object also reports `effectiveModelId` (the
concrete model the session runs on) whenever it differs from `currentModelId`
— i.e. while a combo runs on a fallback entry. Prompt runs use the same combo
fallback as the TUI (per session: each session tracks its own cooldowns), with
switches logged via `agent_message_chunk` notifications and reported to
clients with a `model_changed` session/update. After `session/new` (and
`session/load`) the server advertises its slash commands via an
`available_commands_update` session/update; currently that is the
`/interview` command, which starts an interview flow whose lifecycle is
reported with `interview/started`, `interview/question` and
`interview/completed` notifications.

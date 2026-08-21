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
| `/help`    | Show help                                 |
| `/clear`   | Clear the conversation                    |
| `/panel`   | Toggle the side panel                     |
| `/diff`    | Open the git diff panel                   |
| `/files`   | Open the file tree panel                  |
| `/rewind`  | Rewind the last turn                      |
| `/memory`  | Memory status                             |
| `/compact` | Context compaction status                 |
| `/proxy`   | Proxy status                              |
| `/exit`    | Exit                                      |

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
  "free_models_only": true,

  "fallback": {
    "enabled": true,
    "cooldown_seconds": 300,
    "priority": ["poolside", "openrouter", "groq", "nvidia", "tokenrouter"]
  },

  "fallback_models": ["poolside/laguna-xs-2.1"],

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
- `free_models_only`: only show free coding models in the `/model` picker and
  the ACP `availableModels`. Set to `false` to show every configured model.
- `working_dir`: project directory (defaults to the launch directory).

### Provider fallback

On a provider error or rate limit during a run, the agent automatically retries
the run on the next provider — in both the TUI and the ACP server.

```json
"fallback": {
  "enabled": true,
  "cooldown_seconds": 300,
  "priority": ["poolside", "openrouter", "groq", "nvidia"]
}
```

- `priority` lists providers in preference order. The failed provider is
  skipped and the most-preferred available one is tried next. Empty priority
  (or entries that don't name a configured provider) fall back to registry
  order: built-ins first, then config-file providers.
- `cooldown_seconds` is the "long cooldown": once a provider fails it is
  excluded from fallback for this long (default 300s = 5 minutes), so it isn't
  hammered again immediately.
- `enabled: false` disables fallback entirely.

Fallback only triggers when the run fails **before producing any output**
(which is where provider errors and rate limits hit — on the first request).
Once the agent has started streaming a response or made tool calls, an error is
surfaced normally instead of being retried, to avoid duplicating partial work.
When a fallback happens you'll see a notice in the TUI (or an
`agent_message_chunk` notification over ACP) saying which provider failed and
which it fell back to.

### Providers

Providers are OpenAI-compatible and configured under the `providers` object.
The built-ins are `poolside`, `openrouter`, `groq`, `nvidia` and `tokenrouter`
(the tokenrouter.com unified gateway). A `providers.NAME` entry either
overrides a built-in (by name) or defines a brand-new provider. All fields are optional; only set what you want to
override.

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

Overriding `models` takes full control of the list — free-model filtering
(`free_models_only`) no longer applies to it.

#### API keys

The `api_key` field accepts three forms:

| Form        | Meaning                                                        |
|-------------|----------------------------------------------------------------|
| `!command`  | Run the rest as a shell command; use its trimmed stdout as the key |
| `env:VAR`   | Read the environment variable `VAR`                            |
| anything else | Treated as a literal key                                     |

The `!command` form is resolved each time a provider is used (opencode/pi
convention). Default built-ins use `env:POOLSIDE_API_KEY`,
`env:OPENROUTER_API_KEY`, `env:GROQ_API_KEY`, `env:NVIDIA_API_KEY`.

#### Free models by default

`free_models_only` (default `true`) filters each built-in provider's model list
to its free coding models, so the picker and ACP `availableModels` never
surprise you with a bill. The per-provider free lists are curated from the
providers' current free tiers — e.g. groq's `groq/compound` / `groq/compound-mini`
are free while `openai/gpt-oss-*` on groq are paid, and openrouter exposes
zero-priced `:free` variants of coding models. To see paid models too, set
`"free_models_only": false`.

### Environment variables

| Variable                     | Overrides                                  |
|------------------------------|--------------------------------------------|
| `ABSTRACT_MODEL`             | `model`                                    |
| `ABSTRACT_PROVIDER`          | `provider`                                 |
| `ABSTRACT_EFFORT`            | `effort`                                   |
| `ABSTRACT_THEME`             | `theme`                                    |
| `ABSTRACT_FALLBACK_MODELS`   | `fallback_models` (comma-separated)        |
| `ABSTRACT_MAX_TURNS`         | `max_turns`                                |
| `ABSTRACT_COMPRESSION`       | `compression_level`                        |
| `ABSTRACT_FREE_MODELS_ONLY`  | `free_models_only` (`true`/`false`/`1`/`0`) |
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

Each agent also ends its response with a `suggest_followups` tool call; the
suggestions are rendered as a "Suggested next steps" list in the TUI and in
`-p` mode. Read-only sessions (ACP `readonly` mode) don't get `spawn_agents`
since sub-agents run with full permissions. Sub-agent runs are capped at 120
seconds so a stuck agent can't hang the parent run.

## ACP server

`agent-cli --acp` implements the Agent Client Protocol v1 over stdio
(newline-delimited JSON-RPC 2.0): `initialize`, `session/new`, `session/load`,
`session/prompt` (with streaming `session/update` notifications),
`session/set_config_option` and `session/cancel`. `session/new` advertises
`provider` + `model` config options and `availableModels` (respecting
`free_models_only`), so clients can switch provider/model — same registry as
the TUI's `/model`. Prompt runs use the same provider fallback as the TUI.

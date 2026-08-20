# agent-cli

A terminal AI coding agent (the `tui-agent`) with a full-screen TUI, a
provider/model picker, and an Agent Client Protocol (ACP) server. Built on the
`cersei` agent library and the GPUI-style event loop in `src/tui/`.

```
cargo run -p agent-cli          # interactive TUI
cargo run -p agent-cli -- --acp  # ACP server over stdio (JSON-RPC 2.0 NDJSON)
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

Configuration is layered TOML, merged in this order (later wins):

1. Built-in defaults
2. `~/.abstract/config.toml` — user-global config
3. `.abstract/config.toml` — per-project config (overrides the global file)
4. Environment variables (`ABSTRACT_*`)
5. CLI flags (`--model`, `--provider`, `--fast`, …)

Start with a project config:

```bash
mkdir -p .abstract
```

### Config file shape

```toml
# .abstract/config.toml  (or ~/.abstract/config.toml)

# Default provider/model. "auto" picks the first configured provider and its
# first model. A bare model id like "groq/compound" implies its provider.
provider = "auto"
model = "auto"

# Only show free coding models in the /model picker and the ACP
# availableModels. Set to false to show every configured model.
free_models_only = true

# Agent behavior
max_turns = 50
max_tokens = 16384
effort = "medium"          # low | medium | max
permissions_mode = "interactive"  # interactive | allow_all
working_dir = "."

# TUI appearance
theme = "dark"
output_style = "default"
auto_compact = true
graph_memory = true
output_format = "text"     # text | stream-json
compression_level = "off"  # off | minimal | aggressive

# Fallback models tried in order when the primary model errors
fallback_models = ["poolside/laguna-xs-2.1"]

# MCP servers started for the agent (name → launch command)
[[mcp_servers]]
name = "my-tools"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

# Hooks run on agent events
[[hooks]]
event = "on_turn_end"
command = "echo turn done"

# Optional local proxy (VibeProxy or compatible)
[proxy]
enabled = true
force = false
url = "http://localhost:8317/v1"
```

### Providers

Providers are OpenAI-compatible and configured under `[providers.NAME]`. The
built-ins are `poolside`, `openrouter`, `groq` and `nvidia`. A `[providers.NAME]`
section either overrides a built-in (by name) or defines a brand-new provider.
All fields are optional; only set what you want to override.

```toml
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
# api_key: see "API keys" below
api_key = "env:OPENROUTER_API_KEY"
# Overriding `models` takes full control of the list — free-model filtering
# (free_models_only) no longer applies to it.
models = [
  "openrouter/free",
  "openai/gpt-oss-20b:free",
  "cohere/north-mini-code:free",
]

# A brand-new provider:
[providers.acme]
base_url = "https://acme.example.com/v1"
api_key = "!kubectl get secret api-key -o jsonpath='{.data.key}' | base64 -d"
models = ["acme/big", "acme/small"]
```

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
zero-priced `:free` variants of coding models. To see paid models too:

```toml
free_models_only = false
```

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
`--no-permissions`, `--acp`, `--proxy`, `-C <dir>`.

## ACP server

`agent-cli --acp` implements the Agent Client Protocol v1 over stdio
(newline-delimited JSON-RPC 2.0): `initialize`, `session/new`, `session/load`,
`session/prompt` (with streaming `session/update` notifications),
`session/set_config_option` and `session/cancel`. `session/new` advertises
`provider` + `model` config options and `availableModels` (respecting
`free_models_only`), so clients can switch provider/model — same registry as
the TUI's `/model`.

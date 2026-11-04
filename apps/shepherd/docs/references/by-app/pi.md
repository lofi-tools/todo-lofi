# Pi

A minimal, extensible agent harness you can make your own: an interactive coding
agent for the terminal, plus the libraries it is built from. Pi deliberately
ships fewer features than comparable harnesses (no sub-agents, no plan mode) and
expects users to add what they want through extensions, skills, packages and the
SDK.

- Checkout: `.cache/checkout/pi` at `a7a75a9dc6891281eed58906c07c55929e7b6c13`
- Upstream: <https://github.com/earendil-works/pi>
- License: MIT
- Runtime: Node.js 22.19 or newer; installed by script, npm, or Nix

It is a monorepo: `packages/coding-agent` is the CLI, `apps/p2` is the companion
app, and the other packages are reusable libraries. Docs live in
`packages/coding-agent/docs/`.

## Features

### Four ways to run the agent

| Mode | Entry point | Use |
| --- | --- | --- |
| Interactive TUI | `pi` | day-to-day terminal use (`docs/usage.md`) |
| Print mode | `pi -p "..."` | one-off and scripted tasks (`docs/cli.md`) |
| JSON event stream | CLI flags | structured events from one run (`docs/json.md`) |
| RPC | `docs/rpc.md` | control a separate Pi process (commands, extension UI) |

- A TypeScript SDK (`docs/sdk.md`) runs Pi inside an application: session
  lifecycle, prompting, event subscription, configuration and resources.
- RPC has its own protocol records, framing, run lifecycle, error and shutdown
  semantics, and a minimal client example; extension UI can be driven over it
  (`docs/rpc-extension-ui.md`).
- `docs/cli-integration.md`, `docs/cli-commands.md`-style coverage of options,
  models, sessions, tools, resources, prompts and package commands.

### The agent

- Built-in tools for inspecting files, editing content and running commands;
  follow Pi's work while it runs, redirect it mid-task, run terminal commands,
  copy/export results (`docs/usage.md`).
- `docs/how-pi-works.md` documents tools, context, sessions and the agent loop;
  `docs/message-types.md` and `docs/session-format.md` describe the wire and
  storage formats.
- **Codemode**: model-written JavaScript runs in a QuickJS VM compiled to WASM
  where the only capability is calling injected tools; nested tool calls never
  enter the model context, only the script's output and return value
  (`packages/codemode`, `docs/codemode.md`).
- **MCP**: a standalone MCP client with stdio and Streamable HTTP transports,
  tool exposure control, resources and OAuth (`packages/mcp`, `docs/mcp.md`).
- Images in prompts, and answer/thinking rendering in the TUI.

### Models and providers

- `@earendil-works/pi-ai` is a unified multi-provider LLM API: provider
  collections, automatic auth resolution, token and cost tracking, and handing a
  session off to another model mid-conversation.
- Interactive `/login` for subscriptions or API keys; keys from the environment
  or a command; named profiles; provider-specific configuration
  (`docs/providers.md`).
- Model catalog with tool-calling-only entries for the chat catalog; model
  configuration, virtual models, and local inference through llama.cpp (including
  an NVIDIA RTX setup guide) in `docs/models*.md`, `docs/llama-cpp.md`,
  `docs/virtual-models.md`, `docs/custom-provider.md`.
- Reasoning/thinking levels per model, with the TUI rendering them
  (and a setting to hide the thinking block).

### Sessions and context

- Continue or switch sessions, branch a conversation to explore another
  approach without losing history, control storage and export/share results
  (`docs/sessions.md`).
- Compaction and branch summarization with a documented summary format (goal,
  constraints and preferences, progress), plus context management and
  project-level `.pi` configuration and context files (`docs/compaction.md`,
  `docs/configuration.md`).

### Customization and distribution

- **Extensions** (`docs/extensions.md`): JavaScript loaded from user, project or
  package locations, with an event API, tool registration and an
  `ExtensionContext`; can add commands, tools, UI and integrations.
- **Skills** (`docs/skills.md`): portable Agent Skills with frontmatter, loaded
  from known locations, shareable and validatable.
- **Prompt templates** (`docs/prompt-templates.md`), **themes**
  (`docs/themes.md`), **keybindings** (`docs/keybindings.md`, configurable
  actions rather than hard-coded keys).
- **Pi packages** (`docs/packages.md`): bundle extensions, skills, templates,
  themes and providers, install from npm or git, manage versions and changelogs,
  declare dependencies, select which resources a package contributes.
- Slash commands (`docs/slash-commands.md`) for models and settings, sessions and
  context, export/share and runtime/project actions, plus commands contributed by
  resources.

### The libraries

| Package | What it provides |
| --- | --- |
| `@earendil-works/pi-ai` | unified multi-provider LLM API, auth, tokens/cost, model hand-off |
| `@earendil-works/pi-agent-core` | stateful agent with tool execution and event streaming |
| `@earendil-works/pi-coding-agent` | the interactive coding-agent CLI |
| `@earendil-works/pi-tui` | terminal UI framework: differential rendering, synchronized output (CSI 2026), main/alternate screen, app-owned scrolling, bracketed paste, components |
| `@earendil-works/pi-durable` | experimental durable harness: conversations, turns and tool calls committed before display; resumes after a crash and retries the interrupted request |
| `@earendil-works/chord` | object runtime across processes: contracts, documents, stores, links, plugins with hot reload, HTTP/workers/web/terminal clients |
| `@earendil-works/pi-mcp` | standalone MCP client (stdio, Streamable HTTP, pipe, in-memory test transport) |
| `@earendil-works/pi-codemode` | QuickJS-in-WASM sandbox for tool-calling scripts (Node worker threads or a Durable Object) |
| `@earendil-works/pi-schema` | JSON Schema builders and `eval`-free validation passing 2,222/2,223 JSON-Schema-Test-Suite core tests, 8.5 KB gzipped |
| `@earendil-works/pi-telemetry` | vendor-neutral telemetry contracts, spans, metrics, propagation, in-memory adapter |
| `@earendil-works/pi-env` | remote execution environments: a small Rust daemon over SSH while storage and credentials stay local |
| `@earendil-works/pi-evals` | evaluation harness |

### Session durability and crash recovery

- `pi-durable` commits conversations, model turns, tool calls and user state to
  storage before anything is shown. If the process dies mid-turn, reopening the
  storage resumes from the last committed point; a model request it died during
  counts as a failed attempt and is retried, so a request that repeatedly kills
  the process ends rather than looping.
- `chord` provides the document/store layer underneath, with plugins reloaded
  host by host while data, connections and running work survive where they can.

### Security posture

- Pi has no built-in permission system for filesystem, process, network or
  credential access: tools and extensions run with the permissions of the Pi
  process.
- Project trust controls which project resources load, but does not sandbox tool
  calls (`docs/security.md`).
- `docs/containerization.md` documents three isolation patterns: a Gondolin
  extension that keeps Pi and provider auth on the host while routing built-in
  tools and `!` commands into a Linux micro-VM; plain Docker; and OpenShell with
  policy control.

### Platform and tooling

- Installers for macOS, Linux and Windows that can also install Node; npm
  install without lifecycle scripts; Nix flake with a pinned offline model
  catalog and a `stable` branch reference.
- Terminal setup notes for tmux, Termux on Android, Windows and various
  terminals (`docs/terminal-setup.md`, `docs/tmux.md`, `docs/termux.md`,
  `docs/windows.md`).
- Development: npm workspaces, Biome for lint/format, Vitest, `npm run build`
  (refreshes model data first) and `build:offline`, `npm run check`, `./test.sh`
  (skips LLM-dependent tests without keys), `./pi-test.sh` to run from sources.
- Code rules in `AGENTS.md`: erasable TypeScript only, no `any`, no inline
  imports, no defensive checks for typed values, one concept per file in Chord,
  tests per mechanism, examples per slice.

## Deliberate omissions

- No sub-agents and no plan mode out of the box; the project says to ask Pi to
  build them or install a package that does.
- No permission prompts or sandboxing in the core; use containerization.
- No built-in multi-agent coordination; that lives in extensions and in the
  separate `pi-chat` project for Slack/chat automation.

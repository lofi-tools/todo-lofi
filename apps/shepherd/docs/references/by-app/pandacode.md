# PandaCode

A small native desktop workspace for coding agents. It runs the coding CLIs you
already use side by side, with projects, Git, terminals and a browser in one
window. Built with Tauri 2, Rust, SQLite and plain TypeScript on the operating
system's webview, with no bundled Chromium, resident Node server or embedded
editor.

- Checkout: `.cache/checkout/pandacode` at `28b835f8b8cb72dcda5506d4bcfff489ad9a98dc`
- Version: `0.3.92` (release commit)
- Upstream: <https://github.com/aicodeking/pandacode>
- License: PandaCode License (MIT plus a fork-credit requirement)
- Privacy stance: no account, no license key, no telemetry

Source map: `src-tauri/src/` (Rust backend: sessions, storage, providers, git,
cloud, vm, bots, github, skills), `src/` (TypeScript UI, one module per panel),
`docs/` (one page per subsystem), `packages/npx`, `mobile/`, `relay/`,
`website/`.

## Features

### Harnesses and providers

PandaCode does not ship a model client; it drives installed coding CLIs and
speaks each one's own protocol.

| Protocol | Harnesses |
| --- | --- |
| Native CLI integration | Claude Code, Codex, Kimi Code, OpenCode, GLM Coding Plan, Xiaomi MiMo (through Claude Code) |
| ACP | GitHub Copilot CLI, Antigravity, Grok Build (xAI), DeepSeek (`dsh --profile acp`), Command Code, Cline, Kilo Code, Devin CLI, Hermes Agent |
| App-server | ZCode (`zcode app-server`) |
| RPC mode | Pi and its fork omp (with a loopback bridge extension for app tools and approvals) |
| MSP | Meta Muse Code (`muse serve`) |

- Pick harness, model and reasoning effort per task, and switch them from the
  composer; the searchable model picker lists recent models, named teams and each
  installed harness with its connections.
- Named provider connections, environment-variable credentials, per-provider
  settings, conversation continuity, response streaming and subagent activity
  are covered in `docs/providers.md`.
- Pi has no approval system of its own, so PandaCode's bridge extension enforces
  the permission modes.

### Tasks, queueing and steering

- Parallel tasks across projects; message queueing and steering while an agent
  runs; approvals collect in a slim bar above the prompt and can be approved all
  at once.
- Task board with Pending → In progress → Review → Completed columns
  (`docs/task-board.md`).
- Sidebar sections: Pinned, Active (running, needs you, limited, unseen failure)
  and Finished, with Settled docked at the bottom; sorting (Manual/Recent/Created),
  per-section caps, filtering, keyboard navigation, row actions and undo.
- Find in thread (`⌘/Ctrl F`) with every match highlighted, plus `⌘G`/`⇧⌘G`.
- Rewind and edit a sent prompt (`docs/rewind.md`); `/btw` side threads
  (`docs/btw.md`); goals and harness command workflows
  (`docs/goals-and-command-workflows.md`); task schedules (`docs/schedules.md`);
  workflow recipes (`docs/recipes.md`); slash commands (`docs/slash-commands.md`).
- Token saver: per-task response-style packs (Caveman Lite/Full/Ultra, Lean code,
  Quiet tools) folded into the prompt.
- Quick overlay launcher bound to a global shortcut for new and running tasks.

### Agent teams and model routing

- Architect plus up to three workers, mixing harnesses and models, or named team
  profiles; each spawned worker is a link in the turn that started it.
- Automatic teams (Cheap, Balanced, Best) where a local router picks each
  worker's model in 10–25 ms without sending the task anywhere; failed workers
  escalate to a stronger model (`docs/model-routing.md`, `docs/architect-workers.md`).
- Agent panels in the dock (team, subagents, machine threads) with a running
  count badge and a 150 ms hover flyout.

### Git, worktrees and GitHub

- Diffs, staging, commits, branch switching from a branch chip, and per-task Git
  worktrees with a one-click local **Merge into main** (`docs/worktrees.md`).
- GitHub view: repositories with threads, pull requests and issues; review, fix
  checks, merge or solve an issue, each opening as a linked thread beside the
  view (`docs/github.md`).
- Optional Jujutsu (jj) support (`docs/jj.md`).

### Review and preview surfaces

- Every turn saves its changes, reviewed as unified or split diffs
  (`docs/turn-changes.md`).
- Embedded browser (`docs/browser.md`), file previews, design canvas
  (`docs/design-canvas.md`), moodboards (`docs/moodboards.md`), a built-in mockup
  sketcher, file mentions and file icons (`docs/file-mentions.md`,
  `docs/file-icons.md`).
- Run and preview a project from inside the app (`docs/run-project.md`).
- Attachments: images, PDFs, documents, folders and sketches, rendered as
  read-only chips in the transcript (`docs/attachments.md`).

### Bots

- Put an agent behind Telegram, Slack, Discord, WhatsApp or a local web chat;
  each conversation is an ordinary task (`docs/bots.md`).
- Bot settings cover profile, conversations, memory, schedules, channels, rules
  and activity; shared bot channels and a Bots view listing every bot with its
  latest message.
- Safety model and connection details in `docs/bots.md`; headless operation in
  `docs/headless.md`.

### Local VMs, cloud sandboxes and cloud agents

- Headless macOS/Linux VMs on Apple silicon through Tart, with a built-in RFB
  client for the experimental VNC server and `tart exec` for commands; agents get
  `pandacode_vm_*` tools and the inspector has a VM tab.
- Cloud sandboxes: Daytona, Cloudflare, AWS (EC2 via SSM, AgentCore code
  interpreters), Google Cloud, E2B, Modal, Vercel Sandbox, CodeSandbox,
  Runloop, Blaxel, Tensorlake, Freestyle, boat; Docker/Podman/Apple Containers/
  LXD; VPS providers (DigitalOcean, Vultr, Hetzner, RunPod) over SSH.
- Cloud agents run the ordinary pipeline over a remote transport, install the
  harness there, copy logins and return changes to a local worktree, surviving
  laptop sleep (`docs/cloud-agents.md`).
- Machine orchestration links between machines (`docs/machine-orchestration.md`).
- iOS Simulator and Android emulator tools for agents (`docs/approvals.md` is
  separate; see `docs/capabilities.md` and the README's simulator section).

### Deploys

Start a deploy task for Vercel, Cloudflare, Railway, Netlify, AWS or Google
Cloud from the header, with approval-gated CLI runs; AWS and Google Cloud use the
installed CLI and its login, credentials are scoped to the child process and
scrubbed from output (`docs/netlify.md`, `src-tauri/src/cloud/deploy.rs`).

### Theming, design system and platform behaviour

- VS Code theme import (colors only, nothing executed), wallpaper and procedural
  sky scenes, glass surfaces with reduced-transparency fallbacks, Increase
  Contrast and forced-colors support, and motion that never gates input.
- A token-based design system (`src/design-system.css`) with state tones for
  plan/edit/full/team/saver/remote/worktree, checked for contrast by tests.
- Chat titles and activity (`docs/conversation-presentation.md`), settings
  (`docs/settings.md`), skills library and rules registry
  (`docs/skills-library.md`), agent tools (`docs/agent-tools.md`), agent usage
  and cost estimates (`docs/agent-usage.md`), agent CLI updates
  (`docs/agent-updates.md`).

### Remote access, mobile and headless

- PandaCode Connect: GitHub sign-in, a relay that forwards end-to-end encrypted
  messages between your devices, and mirrors for remote viewing
  (`docs/remote-access.md`).
- Mobile app with inbox and push notifications (generic alerts only, never task
  content) and live activities (`docs/app-store/`).
- `pandacode serve` on a Linux server or Mac mini, with systemd guidance
  (`docs/headless.md`).

### Privacy and security

- Projects, conversations and settings are local SQLite; nothing phones home.
- Prompt and provider keys go only to the provider chosen; keys live in a 0600
  `secrets.json` on macOS, Secret Service on Linux, Credential Manager on
  Windows.
- One switch starts every agent with its vendor's telemetry opt-outs.
- A documented list of the only automatic outbound requests (version checks,
  price list, update check) and why each runs.

### Development and verification

- Typecheck with `npx tsc --noEmit`, Node unit tests with `npm test`, release
  script tests with `npm run test:release`.
- Playwright suite against the Vite preview, plus a WebKit config that matches the
  real WKWebView target.
- Release flow (version bump, notes, tag, CI builds for macOS arm64, Windows x64,
  Linux x64) in `docs/releasing.md`.
- UI conventions are strict and documented in `AGENTS.md`: no literal accent
  colours, one glass material, one set of surface tokens, hairline borders, no
  layout animations.

## Notes for comparison

- PandaCode is the most feature-dense reference: task board, agent teams, bots,
  cloud execution, deploy flows, mobile and remote access all in one app.
- It is the only reference with a mobile client and end-to-end-encrypted remote
  access.
- Its model of an agent is a harness process plus a task record, not a pane: the
  terminal is one surface among many, unlike herdr and herdr-core where panes and
  terminals are the primary objects.

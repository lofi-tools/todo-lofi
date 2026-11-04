# Lifecycle and automation

How the app is driven by other programs, kept up to date, and what it does with
data.

## Common to both

- A command-line surface so an agent or script can inspect the open app and
  perform actions without a human clicking.
- Documentation aimed at agents (a `--skill` manual in herdr-app; agent tools and
  a headless mode in pandacode).
- Self-update: check in the background, download beside the running app, and take
  over on restart; work in progress is not lost.
- Signed/verified downloads with published checksums, and an installer per
  platform.
- A privacy stance documented in the repository rather than implied.

## herdr-app only

- `herdr-app ls` and `herdr-app status` inspect the open window; `herdr-app
  commands` lists every command with arguments and works without a window;
  `herdr-app check` validates a mod; `herdr-app shot` saves a frame as PNG.
- `herdr-app --skill` prints the window manual that herdr-core's skill points to.
- The app controls the window; `hc` controls herdr-core. Agents driving terminals
  use `hc`, agents changing what is on screen use `herdr-app`.
- **Usage telemetry is on by default in releases** and can be turned off in
  Settings → Privacy. Mods are trusted code and are not sandboxed.
- Platform notes: Linux needs a desktop session and a working Vulkan driver,
  macOS builds are Apple Silicon (Intel from source), Windows installs from
  PowerShell into the user profile; the installer also installs or updates
  herdr-core.
- Drawing performance has a benchmark harness (`scripts/bench.py`,
  `bench_compare.py`) with recorded visibility and display-Hz observations.

## pandacode only

- **`pandacode serve`**: a headless server with first-run setup, a Linux systemd
  unit example and Mac mini guidance, covering bots, tasks after a crash and
  security.
- **Tauri IPC contract**: documented commands for prompts, slash commands,
  goals/workflows, saved turn changes, machines and remote access, so the
  frontend and external automation share one API.
- **Agent tools** injected into harnesses: `pandacode_vm_*`, cloud CLI, machine
  orchestration, browser captures and workspace image access, with approval gates
  for anything that runs a deploy or a cloud CLI.
- **No telemetry, analytics or accounts**, and a documented table of every
  automatic outbound request (version checks, public price list, update check,
  optional model router download) with frequency and purpose.
- One switch applies every installed agent's vendor telemetry opt-out plus
  `DO_NOT_TRACK=1`, without editing agent config files.
- Secrets in a 0600 `secrets.json` (macOS), Secret Service (Linux) or Credential
  Manager (Windows); deploy credentials scoped to the child process and scrubbed
  from output.
- Platform: macOS 12+ (arm64 and x64), Windows 10 1809+ (x64 and arm64), Linux
  x64/arm64 AppImage on X11 or XWayland; unsigned builds with per-platform
  workarounds, and `npx pandacode` as a verified installer.

## Divergence worth knowing

Herdr-app automates the window and leaves execution to `hc`; its telemetry is
opt-out. PandaCode automates whole tasks, including cloud execution and deploys,
and has no telemetry at all. If Shepherd exposes automation, the decision is
whether the automation target is a view (herdr-app) or a task (pandacode) — and
the telemetry default should be chosen explicitly rather than inherited.

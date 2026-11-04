# Git, machines and cloud

How work leaves the local machine, and how version control is handled.

## Common to both

- SSH machines: add a host (from the SSH config or typed), and work on it as if
  it were local, with its own work continuing while your window is closed.
- Work is scoped per repository and usually per machine; a repository on a remote
  host is opened on that host.
- Remote work survives a local disconnect as long as the remote runtime stays up.
- Local Git checkouts are used directly; neither app reimplements Git.

## herdr-app only

- Machines are a first-class part of the model: a space belongs to a machine,
  each machine keeps its own spaces, tabs and splits in its own core, and
  connected screens share structure and live terminals while keeping their own
  focus and zoom. Only SSH is supported; there are no cloud providers.
- `machine-add` is a mod, and Settings → Machines or the sidebar add action opens
  it.
- A **worktree mod** and worktree-oriented space creation; the core itself has no
  Git commands, so branch or worktree operations happen through the shell or the
  mod.
- Separate clients can attach to the same remote core; agents reach machines with
  `hc machine add` and `hc run --on`.

## pandacode only

- **Git**: diffs, staging, commits, branch switching from a branch chip, and
  per-task worktrees with one-click **Merge into main**. Optional Jujutsu (jj)
  support.
- **GitHub view**: repositories with their threads, pull requests and issues;
  review, fix checks, merge or solve an issue, each opening as a linked thread
  beside the view. Uses the installed `gh` where appropriate.
- **Local VMs** on Apple silicon through Tart: headless macOS/Linux guests, a
  built-in RFB client, `tart exec` for commands, screenshot-to-click input
  mapping, and idle suspend/stop.
- **Cloud sandboxes**: Daytona, Cloudflare, AWS (EC2 via SSM, AgentCore code
  interpreters), Google Cloud, E2B, Modal, Vercel Sandbox, CodeSandbox, Runloop,
  Blaxel, Tensorlake, Freestyle, boat, plus Docker/Podman/Apple Containers/LXD
  and VPS providers (DigitalOcean, Vultr, Hetzner, RunPod) over SSH.
- **Cloud agents** install the harness on the chosen machine, copy the login,
  keep the run going while the laptop sleeps, and bring changes back to a local
  worktree through git bundles.
- **Machine orchestration**: links between machines and agent tools for driving
  another machine.
- **Deploys**: Vercel, Cloudflare, Railway, Netlify, AWS and Google Cloud as
  approval-gated CLI runs with credentials scoped to the child process and
  scrubbed from output.
- **PandaCode Connect** (remote access), mirrors, an end-to-end encrypted relay,
  a mobile app and push notifications.

## Divergence worth knowing

Herdr-app's remote story is "the core is there, the window is here": one protocol,
SSH only, and the remote side runs the same herdr-core. PandaCode's is a provider
matrix with credentials, cost and cleanup per provider, and it treats Git and
GitHub as core product surfaces. Herdr-app has no Git UI; its worktree support is
a mod over the shell.

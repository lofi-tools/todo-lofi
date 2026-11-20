# Antigravity ACP integration

## Evidence and scope

- The installed Antigravity app reports 2.32.1. Its separately installed
  `agy` CLI reports 1.5.9 and provides the official `agy acp` stdio server.
  Use CLI 1.5.9 as the integration baseline and report the app version separately.
- A prompt-free ACP initialization and session creation on this machine
  advertised Gemini 3.8 Flash and a native `auto` mode. The mode describes
  automatic approval of safe commands with review of irreversible actions.
  `--dangerously-skip-permissions` is a different, blanket permission bypass.
  A separate prompt-free `session/set_config_option` check confirmed that the
  installed server accepts `mode = auto` and `model = gemini-3.8-flash`.
- Follow the existing ACP transport and terminal lifecycle; keep agent-owned
  permission requests visible and never implement Auto as unconditional allow.

## Implementation stages

1. Add Antigravity to the core agent identity, durable persistence, and ACP
   launch/discovery, with tests for executable selection and the installed
   CLI baseline. Keep the existing app detection as a separate installation.
2. Wire availability, onboarding, agent priority, model and mode discovery,
   and settings in the desktop UI. Map the Safety picker's Auto to the native
   ACP `auto` mode only when advertised; retain approval requests and surface
   failures. Document the interaction with other ACP modes.
3. Add an opt-in Antigravity live capture and offline replay before submitting
   any prompt. Capture the ACP exchange, tool requests, replies and relevant
   fixture effects; never record credentials. The live test must select
   `gemini-3.8-flash` and submit at most one short prompt.
4. In an isolated temporary project, use one prompt asking the agent to inspect
   a project file, an explicitly created neighboring file, and run `pwd` and
   `ls`, then summarize. Cache even a partial or failed run; do not automatically
   retry or send another prompt. Replay the cached interaction in default tests.
5. Run focused checks, inspect the resulting commits and document any capability
   not verified by the single live exchange.

## Decisions

- The desktop app and CLI have independent version numbers. Google describes
  a unified authentication system, while ACP uses the CLI's cached sign-in;
  installing the desktop app alone does not establish that `agy acp` can run.
- ACP exposes Auto as a session mode alongside Plan and Accept edits. Selecting
  one replaces the other; do not imply they can be enabled independently.
- An ACP turn that ends without a substantive agent message reports an
  incomplete response instead of silently appearing successful.
- The live probe uses disposable files rather than another project or private
  home-directory files. Explicitly handling access outside the project must not
  weaken the ACP client's existing filesystem boundaries.

## Single live run

The one authorized prompt ran with Gemini 3.8 Flash and Auto. The server
reported three tool calls: a successful README read, an outside-file read
which requested approval and was denied by the capture client, and a failed
`pwd && ls` command. It ended the turn without a final message. The raw ACP
exchange is retained under `.cache/shepherd/test/replay/antigravity/` and
disposable projects under `.cache/shepherd/test/runs/live-probes/`.
The repository fixture contains only the seven path-redacted tool updates.

The capture mistakenly started the CLI in the repository directory while
passing the disposable project to `session/new`. The agent used its process
directory and read the repository README rather than the intended fixture. The
capture now starts the process in the disposable project and tests that the
two directories match. Shepherd's normal launch already starts ACP processes
in the task directory. No second prompt was sent, so command success, a final
answer and an approved outside-file read remain unverified in a real run.

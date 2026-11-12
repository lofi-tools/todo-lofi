# Freebuff interactive CLI adapter

Status: planned. This document records the agreed design; it does not claim
that the adapter exists or that Freebuff's current terminal protocol has been
verified. It supersedes the terminal-only restriction for the new integrated
agent, while retaining the existing standalone terminal option.

## Goal and user-visible behavior

Add Freebuff as an integrated Shepherd agent. A Shepherd prompt is submitted to
Freebuff's interactive CLI input, and the adapter reads its terminal display to
produce Shepherd's normal task and transcript events, including streamed
assistant text, tool activity, approvals, errors, and turn completion. The task
runs in Shepherd's usual task worktree and uses the existing Freebuff login.
Keep **Freebuff (terminal)** as a separate, standalone option with its existing
behavior and **Back to chat** control. The integrated option participates in
the normal task lifecycle and persisted transcript.

Keep the integrated session's actual Freebuff terminal visible in the terminal
panel while the user interacts with it. Open the terminal panel automatically
when the integrated task starts, while allowing the user to switch to other
inspector tabs. Both the Shepherd composer and that
terminal send input to the same Freebuff process: the user can type prompts
directly into the terminal, and the adapter must observe those turns and
include both the user prompt and response in the same task transcript. A
composer submission replaces any unfinished terminal prompt; clear it using
the CLI's verified editing behavior before entering the composer text.
Coordinate the two input paths so their keystrokes cannot interleave. If the
adapter cannot recognize or safely clear the current input, report the state
rather than guessing. Retain ordinary terminal input and resizing.

Before forwarding a terminal-origin submission, admit it through the task
lifecycle: establish the worktree's turn baseline, update task state, and
persist the prompt. Hold submission until admission succeeds, so file changes
made immediately by the CLI cannot race the baseline. On failure, leave the
prompt unsubmitted and show the error. Composer-origin submissions retain their
existing admission path and must not be duplicated by terminal observation.

When Freebuff requests approval or asks an interactive question, present it in
Shepherd's existing approval or question UI and relay the user's response to
the CLI. Inspect the current harness and UI contracts before implementation;
if questions or text corrections cannot be expressed by them, extend the
contracts and persistence deliberately rather than inventing an event mapping.
Never infer approval or submit an answer from an ambiguous screen.

The current harness approval contract supports decision choices, not an
open-ended answer, and no separate question control was found in the shell.
Add a question card with a text-answer control for Freebuff's open-ended
questions. Relay the answer to the same terminal session and persist the
question and answer in the transcript. Do not represent free-text questions
as approval decisions; stop on an unrecognized question interaction.

## Executable, updates, and compatibility

Resolve a configured Freebuff executable first and then search PATH. Use the
existing installation and login; do not install Freebuff. The user's Nix-provided
entry point is a launcher (observed wrapper version 0.0.171); its bundled code
can download or update the underlying runtime. Allow its normal update behavior
and surface detectable update progress or status in Shepherd. Distinguish the
launcher version from the actual runtime version and determine how to discover
both without assuming they are identical. Re-probe compatibility after an
update or launcher-driven restart; if the runtime cannot be identified or is
unsupported, stop and provide actionable diagnostics rather than parsing as a
known version.

Target the user's installed runtime version first. Capture its observable
terminal behavior and document the supported version and recognition rules.
Unknown screens, uncertain prompt boundaries, ambiguous output, and unsupported
versions must stop the adapter with context; do not guess or submit input. A
terminal parser must account for cursor movement, ANSI/VT control sequences,
partial UTF-8, screen clears, resizing, redraws and reflow. Recognize semantic
state from the rendered terminal state and transitions, not a naive line or
escape-code substring. Preserve the CLI's normal interpretation of input,
including multiline text and terminal control characters, as requested.

### Local inspection on 2026-11-10

Without launching the CLI, the `freebuff` entry on PATH was observed to be a
Nix-provided Node launcher, version `0.0.171`. Its configured runtime path is
`~/.config/manicode/freebuff`; adjacent metadata reported runtime version
`0.2.36` and the runtime was an arm64 macOS executable. The launcher source
ensures the runtime is present, launches it, and schedules an update check
shortly afterward. These are local observations, not a verified screen
protocol or a guarantee that the runtime will remain at that version.

Read-only inspection on 2026-11-12 of the same 0.0.171 launcher found that
startup repairs a missing or outdated cached runtime before starting it. It
then schedules an update check after 100 ms. For a newer release it stages the
download, stops the running child, prints an update notice, replaces the
runtime and metadata, and starts a new child under the same launcher process.
If installation fails after the child stops, it attempts to restart the cached
runtime; a staging failure leaves the old child running. The binary is replaced
before the metadata file, so a metadata read during an update can describe a
different executable. Metadata identifies a cached release, not the running
process or a supported screen contract. A launcher child can restart without
the terminal process exiting, so terminal-exit notifications alone cannot
establish continuity or detect every update.

Read-only inspection of that executable's bundled UI code found the default
input placeholder “enter a coding task or / for commands” and help labels for
Ctrl+J or Option+Enter to insert a newline. Its key-dispatch code appears to
clear a non-whitespace input on Ctrl+C before checking for an interrupt; it
checks `trim()` rather than merely the input length. The same key can stop or
quit in other states, and streaming/queue state changes the placeholder. These
are static code observations, not verified screen or input behavior: never use
them alone to recognize readiness, clear a draft, or send an interrupt.

The same bundled code describes an `ask_user` tool that accepts a list of
questions, each with at least two options, optional multi-select, and optional
validation for custom text. Its described answer format distinguishes selected
options, custom text, and skipping. This is static evidence, not an observed
terminal interaction. Shepherd's current text-only question contract cannot
faithfully represent all of those cases. For multiple-choice questions, show
the supplied choices and an **Other** text option. Support multiple selections
when the verified interaction permits them, apply its custom-text validation,
and preserve whether the user selected options, entered text, or skipped.
Verify the available actions and terminal interaction before enabling them.

## Session and transcript

Support multiple turns in the same running process. Investigate whether the
installed Freebuff runtime can restore a session after a Shepherd restart; if
it can, implement and verify resume. Otherwise advertise resume as unavailable
and clearly report the limitation rather than fabricating continuity. Expose
only capabilities supported by observed behavior. Start with Freebuff's own
configured model and agent defaults; expose selectors only when options and
selection can be detected and controlled reliably.

Translate observed updates into stable session, turn, message, tool and approval
identities. Avoid duplicated text or tool events on redraw. Stream text as it
appears; when a redraw revises already displayed text, correct the normalized
transcript where the contract supports it and report uncertainty when it cannot
be reconciled. Persist the normalized transcript through Shepherd's normal
storage. Terminal captures are diagnostic opt-in, not a default transcript;
that runtime preference does not waive the required live-test captures below.
Ensure diagnostics and captures do not expose credentials or unrelated data.

On cancellation, send Freebuff's normal interrupt. Preserve the process and
session only when the CLI returns to a reliably recognized prompt. Define and
surface failure or interrupted state if the prompt does not return; do not
claim a successful cancellation or automatically start a replacement session.
Shutdown and process-exit handling must clean up the PTY and child process tree
and report failures through Shepherd's normal diagnostics.

## Investigation and validation

1. Inspect the installed launcher/runtime and existing terminal infrastructure
   without installing anything. Determine version detection, update signals,
   login requirements, prompt entry/submit, screen states, approvals, questions,
   interrupt, turn completion, and whether session resume exists. Record any
   behavior that cannot be observed reliably as a limitation, not a capability.
2. Build a versioned fixture corpus from sanitized PTY byte streams and screen
   snapshots. Cover streaming, redraws, reflow, split escape sequences and UTF-8,
   tool activity, approval and question flows, errors, cancellation, launcher
   updates, unknown screens, process exit, and restart/resume where supported.
3. Implement and test the parser/state machine and `AgentHarness` adapter with
   a deterministic local fake CLI. Verify event ordering, backpressure, prompt
   routing, reliable failure on ambiguity, cleanup, and normalized persistence.
   Assert the integrated and standalone Freebuff choices remain distinct.
4. Limited manual live verification is permitted, subject to explicit user
   approval **before each submission of live data**. Show the exact proposed
   message, the project and worktree, and the files or other context that may
   be sent; obtain confirmation before submitting anything. Choose a small
   number of useful tasks related to one of the user's actual projects, and
   avoid unnecessary metered queries. Prefer the user's earlier question-only
   messages from the ChatGPT project “tools” that required tool calls, using
   the corresponding Shepherd project at `/Users/me/src/me/tools`. Obtain
   the exact messages from the user if the project is inaccessible. This
   preference does not approve any live submission. Default tests must not
   make live calls.
   One proposed question is “give me the dotenv fragment to override / ignore
   battery / wifi and run ar3 regardless”. The user approved one live submission
   of this exact question in the `tools` project when capture is ready. Use
   Freebuff's existing default model, which the user identifies as DeepSeek 4.1;
   do not change the model. This approval does not extend to other messages.
   Cache every live response, including partial responses, together with raw
   PTY bytes, terminal-size changes and rendered terminal-state transitions
   in their original order, plus external effects needed by assertions, under
   the repository `.cache/`; replay them on later runs.
   Treat a capture write failure as a live-test failure and stop further
   submissions rather than continuing without a replayable record.
   Gate genuinely live Freebuff probes behind a separate `live-freebuff`
   feature disabled by default. Use disposable worktrees and the existing
   login; do not publish captures or credentials. Document how to record,
   replay, invalidate and safely sanitize captures while retaining the terminal
   details needed for accurate replay, and distinguish replay evidence from a
   live run.
5. Verify the integrated flow from prompt through streamed reply and tool or
   approval interaction, cancellation and a subsequent turn, error reporting,
   persistence, and restart behavior. Use background native checks by default;
   run visible window checks only when the changed behavior needs them.

### Opt-in capture runner

The shell contains an ignored `approved_freebuff_capture` diagnostic behind
the non-default `live-freebuff` feature. Its capture and worktree-effect path
has passed an offline fake CLI check; Freebuff itself remains unverified.
Before each live submission, obtain the user's approval of the exact prompt,
project, disposable worktree and context. The runner reads the prompt from a
file, displays PTY output, and waits for separate `send` and `stop` marker
files outside the worktree. The operator creates `send` only after recognizing
the ready prompt and `stop` only after recognizing the complete response. It
records output, screens, input, runtime metadata and supported worktree effects;
an error or missing drain leaves the capture incomplete. Marker files are local
controls, not a substitute for user approval. This diagnostic does not yet
classify the screen, test approval flows, or establish runtime compatibility.

For faithful replay, the capture records the terminal type and renderer version,
and the process exit code or signal, distinguishing an operator-requested stop
from a natural exit. Keep the original byte stream: PTY reads can split UTF-8
characters or contain non-text bytes. The [asciicast v3 format](https://docs.asciinema.org/manual/asciicast/v3/)
provides a useful reference for timed input, output, resizing and exit events;
consider an optional export for playback without replacing the native capture
or silently losing bytes. [tlog's format](https://github.com/Scribery/tlog/blob/main/doc/log_format.md)
is another reference for preserving non-text bytes. Captures may include
sensitive input and output and must remain private.

## Acceptance criteria

- Freebuff is available as an integrated task agent and the standalone terminal
  remains available.
- A prompt reaches the installed CLI in the task worktree, and recognized output
  streams into the persisted Shepherd transcript without redraw duplication.
- The integrated Freebuff terminal remains visible in the terminal panel, and
  prompts entered directly there and through the composer share one session
  and produce correctly attributed transcript turns.
- Observed tools, approvals, interactive questions, failures and completion map
  to accurate Shepherd controls and events; unsupported or ambiguous states
  stop with diagnostics rather than guessing.
- A normal interrupt returns to the same session only when prompt readiness is
  confirmed; resume is advertised only if demonstrated.
- Launcher update activity is surfaced when detectable, and unsupported or
  changed runtime versions are detected instead of silently misparsed.
- Fixtures and replay run without live Freebuff or network access. Every live
  submission is confirmed with the user first; all responses and terminal
  streams are cached along with the effects their assertions depend on.

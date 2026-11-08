# Pi live model smoke

Status: **not yet run**. This is the human- and provider-gated check for a real
Pi model turn through Shepherd's `PiHarness`. It captures the two things the
offline fixtures cannot: a provider turn that ends with `agent_settled`, and a
real tool call that passes through the bundled extension's confirmation bridge.

The replayable contract tests remain the normal-suite evidence. This procedure
does not claim that a live provider turn passed until its evidence row is
filled.

## Prerequisites

- Pi 1.5.0 or newer on the machine: `command -v pi && pi --version`.
- A Pi model provider already configured by running `pi` once and completing
  its sign-in/model selection. A prompt such as `Reply with OK.` must work
  outside Shepherd, with no `PI_OFFLINE=1` in the environment.
- The repository in the Nix dev shell (`direnv allow .` once), and `jq` on
  `PATH`.
- macOS 13+ or Ubuntu 24.04+, with a desktop session for the approval cards.
- A disposable project and task. This smoke spends model-provider credit and
  runs a shell command in that project.

## Build and prepare the RPC recorder

From the repository root, run the debug dev shell, not a release bundle:

```sh
direnv exec . shep
```

Before launching it, create a small executable wrapper that passes every
argument to the installed Pi and tees its RPC stdout to a log:

```sh
pi_path=$(command -v pi)
wrapper=.cache/pi-live-tee
mkdir -p .cache
cat > "$wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec "$pi_path" "\$@" | tee -a "$PWD/.cache/pi-live-rpc.jsonl"
EOF
chmod +x "$wrapper"
rm -f .cache/pi-live-rpc.jsonl
```

Point Shepherd at the wrapper in `settings.json` (macOS:
`~/Library/Application Support/dev.shepherd.shell/settings.json`; Linux:
`dev.shepherd.shell/settings.json` under `$XDG_CONFIG_HOME`):

```json
{
  "version": 1,
  "app": {
    "agents.pi.executable": "/absolute/path/to/.cache/pi-live-tee",
    "agents.pi.args": []
  }
}
```

Restart `shep` after changing the setting. The wrapper keeps Pi's stdin and
stdout connected to Shepherd; only stdout is duplicated into the log.

## Procedure

1. Create a disposable project and a task whose agent is **Pi**. Confirm the
   task header does **not** say **unreviewed tools**; the bundled approval
   extension must be loaded.
2. Send: `Use a shell tool to run printf pi-live-smoke, then reply with exactly
   DONE.` Do not press Return again while the turn is running.
3. When the approval card appears, check that its title names the real tool and
   that the card is not a fixture/dialog text. Choose **Allow once**.
4. Verify that the command output appears in the transcript, the final answer
   is `DONE`, and the task returns to a settled state rather than finishing at
   an intermediate `agent_end`.
5. Repeat in a second task and choose **Deny** at the approval card. Verify that
   the tool does not run, the transcript records the denial, and the turn still
   reaches a settled state.
6. Inspect the recorded RPC stdout:

```sh
jq -c 'select(
  .type == "agent_settled" or
  (.type == "extension_ui_request" and .method == "confirm")
)' .cache/pi-live-rpc.jsonl
```

The log must contain at least one real `extension_ui_request` with
`"method":"confirm"` before each decision, and at least one
`"type":"agent_settled"` with `"aborted":false` after the allowed turn. An
`agent_end` record may also appear; it is not completion evidence.

## Pass condition

All of the following must hold:

- the approval card names the real tool call and both Allow and Deny reach Pi;
- the allowed command output and `DONE` appear exactly once in the transcript;
- the denied task records the denial and does not show command output;
- the log contains the real `confirm` request and a final `agent_settled`
  record for the allowed turn;
- the task header never reports **unreviewed tools**.

## Record

| Commit | Date | Machine | Pi version | Provider/model | Result | Operator |
| --- | --- | --- | --- | --- | --- | --- |
| | | | | | Pending — human-gated | |

Keep `.cache/pi-live-rpc.jsonl` and screenshots of the approval cards with the
evidence. Until this row is filled, the live model-turn criterion is not
claimed as passed.

## If it fails

Keep the log and record whether the failure was provider authentication, a
missing `agent_settled`, a missing extension approval request, or a command that
ran without an approval card. Reproduce the transport or mapping part with the
normal-suite fixtures before changing the Pi adapter. Do not mark the criterion
passed when the provider turn did not run.

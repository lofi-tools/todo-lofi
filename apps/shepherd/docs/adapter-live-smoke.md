# Adapter live smoke

Status: **partially run**. OpenCode 2.0.14 and Pi 1.5.0 passed their installed
probes on this machine. Claude Code is not installed here, and the opt-in CI
workflow has not been dispatched on its self-hosted runner; those steps remain
human-gated and are not claimed as passed.

The default Rust suite excludes every live probe behind its `live-*` feature.
The workflow also runs `scripts/check-adapter-smoke-config.sh` before compiling;
a missing repository variable fails the dispatch instead of letting a probe
pass without an integration.

## OpenCode

Create a settings file whose executable points at the installed OpenCode:

```json
{
  "version": 1,
  "app": {
    "agents.opencode.executable": "/absolute/path/to/opencode"
  }
}
```

Run from the repository root:

```sh
SHEPHERD_OPENCODE_TEST_SETTINGS=/absolute/path/settings.json \
  direnv exec . cargo test -p agent-harness --features live-opencode \
  --lib opencode::tests::installed_
```

The seven probes create and resume a session, switch and restore a mode,
stream text and an image through a loopback model fixture, report a provider
sign-in failure, approve a shell call, deny one, cancel a pending approval, and
load a session through the ACP fallback. The loopback fixture does not spend
provider credit.

## Pi

The Pi probe uses the installed binary and its local RPC dialog; no provider
turn is sent.

```sh
SHEPHERD_PI_TEST_EXECUTABLE=$(command -v pi) \
  direnv exec . cargo test -p agent-harness --features live-pi \
  --lib pi::tests::installed_pi_exchanges_extension_dialog_records
```

It checks startup state, command listing, a real extension `confirm` request,
the reply by request ID, and shutdown. A live model turn is a separate
provider-gated procedure in [pi-live-smoke.md](./pi-live-smoke.md).

## Claude Code ACP

Create a settings file with the absolute ACP executable and any required
arguments:

```json
{
  "version": 1,
  "app": {
    "agents.claude.executable": "/absolute/path/to/claude-code-acp",
    "agents.claude.args": []
  }
}
```

Run:

```sh
SHEPHERD_CLAUDE_TEST_SETTINGS=/absolute/path/settings.json \
  direnv exec . cargo test -p agent-harness --features live-claude \
  --lib acp::tests::installed_claude_code_opens_an_acp_session
```

The probe initializes ACP and opens a session in a disposable project and
profile without sending a model prompt. It has not run here because the
executable is absent.

## CI dispatch

The **Shepherd adapter smoke** workflow is manually dispatched from the default
branch on a self-hosted Nix runner labelled `shepherd-adapters`:

1. Set `SHEPHERD_ADAPTER_TEST_SETTINGS` to the absolute settings path on the
   runner for `opencode` or `claude-acp`.
2. Set `SHEPHERD_PI_TEST_EXECUTABLE` to the absolute Pi executable for `pi`.
3. Dispatch **Shepherd adapter smoke**, choose exactly one agent, and keep the
   log.

The first step runs `scripts/check-adapter-smoke-config.sh` with the selected
agent. It rejects an empty variable, an unreadable settings file or a
non-executable Pi path, so a missing integration cannot silently pass. The
probe step then runs the same feature-gated test command shown above with
`--locked`.

## Pass condition

A run passes only when the configuration gate succeeds, the selected feature
is enabled, and the named test reports `ok` with no failures. A missing
variable, unreadable settings file or absent executable is a failed dispatch,
not a skip.

## Record

| Agent | Version | Date | Command | Result |
| --- | --- | --- | --- | --- |
| OpenCode | 2.0.14 | 2026-11-08 | `cargo test -p agent-harness --features live-opencode --lib opencode::tests::installed_` | 7 passed |
| Pi | 1.5.0 | 2026-11-08 | `cargo test -p agent-harness --features live-pi --lib pi::tests::installed_pi_exchanges_extension_dialog_records` | 1 passed |
| Claude Code ACP | — | — | gated command above | Pending — human-gated |
| CI dispatch | — | — | workflow above | Pending — human-gated |

Keep the command output with the evidence. Until the Claude row and a CI
dispatch row are filled, the all-integration and CI-run criteria are not
claimed as passed.

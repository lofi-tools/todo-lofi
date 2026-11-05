# Codex wire fixtures

One JSON message per line, exactly as the adapter reads them from the child's
stdout. `cargo test -p agent-harness` replays these through the same translation
functions the live reader uses, so the parsing path is covered without a Codex
install or a network.

| File | What it holds |
| --- | --- |
| `app-server-session.jsonl` | A streamed turn: two deltas that merge into one row, a completed command item, turn completion. |
| `app-server-approval.jsonl` | A turn that pauses on a command approval, then completes. |
| `exec-session.jsonl` | The `codex exec --json` tier: prompt echo, command begin/end, token usage. |

These were recorded against the scripted stub servers in `codex.rs`, whose
payloads mirror the documented app-server and exec wire shapes. They are not
captures of a signed-in run: replacing them with live captures is part of the S2
gate, and nothing in CI depends on that happening.

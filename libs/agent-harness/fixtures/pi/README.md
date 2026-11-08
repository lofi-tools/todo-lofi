# Pi RPC fixtures

One JSON record per line, exactly as `pi --mode rpc` wrote it to stdout. The
contract tests replay these through `PiRpc`, so the record shapes and the ID
correlation of the live transport are covered without a model provider.

| File | What it holds |
| --- | --- |
| `offline-extension-session.jsonl` | The startup `get_state` reply, an extension `confirm` request, and the prompt reply with a `handled` disposition. |

`offline-extension-session.jsonl` was recorded from Pi 1.5.0 on 2026-11-08 with
`--no-session --no-extensions --no-skills --no-prompt-templates --extension
dialog.ts` and `PI_OFFLINE=1`, driving `/shepherd-rpc-fixture` so the extension
asked the question through the RPC channel. The driver used request IDs `1` and
`2`; the replay script rewrites those to the IDs the transport actually sends,
the same way a live Pi echoes them.

The recording contains no agent-turn lifecycle records: an offline extension
command answers through the extension API without starting a model turn, so Pi
emits no `agent_start`, streaming, turn-completion or `agent_settled` record.
Recording those needs a live provider turn, which the repo cannot run here.

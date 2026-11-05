"""Replay one cached app-server session against the real Rust adapter."""

import json
from pathlib import Path
import sys
import time


def replace_path(value, before, after):
    if isinstance(value, str):
        return value.replace(before, after)
    if isinstance(value, list):
        return [replace_path(item, before, after) for item in value]
    if isinstance(value, dict):
        return {key: replace_path(item, before, after) for key, item in value.items()}
    return value


def main():
    cache_file, session, cwd, *gate = sys.argv[1:]
    cache = json.loads(Path(cache_file).read_text())
    placeholder = "__SHEPHERD_TEST_WORKTREE__"

    for entry in cache["sessions"][session]:
        kind = entry["kind"]
        value = entry["data"]
        if kind == "request":
            line = sys.stdin.readline()
            if not line:
                raise ValueError(f"adapter disconnected while replaying {session}")
            actual = replace_path(json.loads(line), cwd, placeholder)
            if actual != value:
                raise ValueError(f"cached request mismatch in {session} ({value.get('method', 'response')})")
        elif kind == "response":
            print(json.dumps(replace_path(value, placeholder, cwd)), flush=True)
        elif kind == "exit":
            if gate:
                until = time.monotonic() + 90
                while not Path(gate[0]).is_file():
                    if time.monotonic() >= until:
                        raise TimeoutError("test did not request the cached exit")
                    time.sleep(0.025)
            print(value.replace(placeholder, cwd), file=sys.stderr, flush=True)
            return 1
        else:
            raise ValueError(f"invalid cached Codex message kind: {kind}")

    # The fixture ended normally only when the adapter shut it down.
    for _ in sys.stdin:
        raise ValueError(f"unexpected request after the cached {session} responses")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (KeyError, OSError, ValueError) as error:
        print(error, file=sys.stderr, flush=True)
        sys.exit(2)

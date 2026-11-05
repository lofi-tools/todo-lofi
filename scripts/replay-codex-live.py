"""Replay one cached app-server session against the real Rust adapter."""

import json
import os
from pathlib import Path
import subprocess
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
    patch = None
    if os.environ.get("SHEPHERD_REPLAY_WORKTREE_PATCH") == "1":
        test_root = Path(__file__).resolve().parent.parent / ".cache/test-db"
        if not Path(cwd).resolve().is_relative_to(test_root.resolve()):
            raise ValueError("worktree patch replay requires a disposable project")
        patch = cache.get("note")
        if not isinstance(patch, str) or not patch:
            raise ValueError("the cached run has no worktree patch")

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
            if patch is not None and value.get("method") == "turn/completed":
                subprocess.run(["git", "apply", "-"], cwd=cwd, input=patch, text=True, check=True)
                patch = None
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
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr, flush=True)
        sys.exit(2)

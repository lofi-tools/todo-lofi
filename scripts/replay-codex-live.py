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
    patches = []
    patch_mode = os.environ.get("SHEPHERD_REPLAY_WORKTREE_PATCH")
    if patch_mode in ("1", "sequence"):
        test_root = Path(__file__).resolve().parent.parent / ".cache/test-db"
        if not Path(cwd).resolve().is_relative_to(test_root.resolve()):
            raise ValueError("worktree patch replay requires a disposable project")
        note = cache.get("note")
        if not isinstance(note, str) or not note:
            raise ValueError("the cached run has no worktree patch")
        patches = [note] if patch_mode == "1" else json.loads(note)
        if not isinstance(patches, list) or not patches or not all(isinstance(patch, str) and patch for patch in patches):
            raise ValueError("the cached worktree patches are invalid")

    previous_patch = None
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
            if patches and value.get("method") == "turn/completed":
                command = ["git", f"--work-tree={cwd}", "apply"]
                if previous_patch is not None:
                    subprocess.run([*command, "--reverse", "-"], cwd=cwd, input=previous_patch, text=True, check=True)
                previous_patch = patches.pop(0)
                subprocess.run([*command, "-"], cwd=cwd, input=previous_patch, text=True, check=True)
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

    if patches:
        raise ValueError("the cached turns ended before all worktree patches were applied")

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

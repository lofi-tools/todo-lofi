#!/usr/bin/env python3
"""Use with git bisect run; exit 125 when a candidate cannot be assessed."""

import json
import subprocess
import sys


def main(arguments):
    if len(arguments) != 1:
        print("usage: bisect-rust-test.py EXACT_TEST_NAME", file=sys.stderr)
        return 125
    name = arguments[0]
    command = ["cargo", "nextest"]
    try:
        result = subprocess.run([*command, "list", "--workspace", "--message-format", "json", "--", "--exact", name],
                                text=True, stdout=subprocess.PIPE, check=True)
        inventory = json.loads(result.stdout)
        present = any(
            case_name == name and not case.get("ignored") and case.get("filter-match", {}).get("status") == "matches"
            for suite in inventory.get("rust-suites", {}).values()
            for case_name, case in suite.get("testcases", {}).items()
        )
        if not present:
            print("Test is missing, ignored or unavailable at this revision; skipping.", file=sys.stderr)
            return 125
        result = subprocess.run([*command, "run", "--workspace", "--", "--exact", name])
        if result.returncode == 0:
            return 0
        if result.returncode == 100:
            return 1
        print(f"Runner failed with status {result.returncode}; skipping.", file=sys.stderr)
        return 125
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"Could not assess candidate: {error}", file=sys.stderr)
        return 125


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

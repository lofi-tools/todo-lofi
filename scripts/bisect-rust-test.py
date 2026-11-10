#!/usr/bin/env python3
"""Use with git bisect run; exit 125 when a candidate cannot be assessed."""

import json
import subprocess
import sys


def main(arguments):
    if len(arguments) == 1:
        selection, name = ["--workspace"], arguments[0]
    elif len(arguments) == 3 and arguments[0] == "--package":
        selection, name = ["--package", arguments[1]], arguments[2]
    else:
        print("usage: bisect-rust-test.py [--package PACKAGE] EXACT_TEST_NAME", file=sys.stderr)
        return 125
    command = ["cargo", "nextest"]
    try:
        result = subprocess.run([*command, "list", *selection, "--message-format", "json", "--", "--exact", name],
                                text=True, stdout=subprocess.PIPE, check=True)
        inventory = json.loads(result.stdout)
        matches = [suite["binary-id"]
            for suite in inventory.get("rust-suites", {}).values()
            for case_name, case in suite.get("testcases", {}).items()
            if case_name == name and not case.get("ignored") and case.get("filter-match", {}).get("status") == "matches"]
        if not matches:
            print("Test is missing, ignored or unavailable at this revision; skipping.", file=sys.stderr)
            return 125
        if len(matches) != 1:
            print("The test name matches multiple binaries; skipping this ambiguous revision.", file=sys.stderr)
            return 125
        result = subprocess.run([*command, "run", *selection, "--", "--exact", name])
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

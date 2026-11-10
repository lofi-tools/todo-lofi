"""Capture test identities that completed in successful runner invocations."""

import json
import os
import socket
import subprocess
import tempfile
from pathlib import Path

from test_shard_history import append_run


def rust_test_ids(data):
    result = []
    for suite in data.get("rust-suites", {}).values():
        for name, case in suite.get("testcases", {}).items():
            if not case.get("ignored") and case.get("filter-match", {}).get("status") == "matches":
                result.append(f"rust:{suite['binary-id']}::{name}")
    return sorted(result)


def ui_test_ids(data, workspace=""):
    result = []

    def visit(suite, parents):
        title = suite.get("title", "")
        names = parents + ([title] if title else [])
        for spec in suite.get("specs", []):
            for test in spec.get("tests", []):
                results = test.get("results", [])
                if results and results[-1].get("status") == "passed":
                    label = "::".join(names + [spec["title"]])
                    result.append(f"ui:{workspace}:{test['projectName']}:{spec['file']}::{label}")
        for child in suite.get("suites", []):
            visit(child, names)

    for suite in data.get("suites", []):
        visit(suite, [])
    return sorted(result)


def vitest_ids(data, root):
    result = []
    for suite in data.get("testResults", []):
        filename = Path(suite["name"]).relative_to(root)
        for case in suite.get("assertionResults", []):
            if case.get("status") == "passed":
                result.append(f"vitest:{filename}::{case['fullName']}")
    return sorted(result)


def rust_list_command(command):
    command = [part for part in command if part != "--no-tests=pass"]
    command[2] = "list"
    position = command.index("--") if "--" in command else len(command)
    command[position:position] = ["--message-format", "json"]
    return command


def available_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Recorder:
    def __init__(self, root: Path, history: Path, revision: str, seed: str, shard: str):
        self.root = root
        self.history = history
        self.revision = revision
        self.seed = seed
        self.shard = shard
        self.started_clean = self.is_clean()
        self.versions = {
            "nextest": self.output("cargo", "nextest", "--version").splitlines()[0],
            "rustc": self.output("rustc", "--version"),
            "playwright": self.output("pnpm", "--dir", "apps/shepherd/ui", "exec", "playwright", "--version"),
            "vitest": self.output("pnpm", "--dir", "libs/web-design-system", "exec", "vitest", "--version"),
        }

    def output(self, *command):
        return subprocess.check_output(command, cwd=self.root, text=True).strip()

    def is_clean(self):
        return not self.output("git", "status", "--porcelain", "--untracked-files=normal")

    def save(self, tests, full=False):
        clean = self.started_clean and self.is_clean() and self.output("git", "rev-parse", "HEAD") == self.revision
        append_run(self.history, {"revision": self.revision, "seed": self.seed, "shard": self.shard,
                                  "success": True, "clean": clean, "full": full,
                                  "versions": self.versions, "tests": tests})

    def run(self, command, directory):
        if command[:3] == ["cargo", "nextest", "run"]:
            inventory = subprocess.check_output(rust_list_command(command), cwd=directory, text=True)
            subprocess.run(command, cwd=directory, check=True)
            self.save(rust_test_ids(json.loads(inventory)))
        elif command[:4] == ["pnpm", "exec", "playwright", "test"]:
            self.history.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=self.history.parent) as temporary:
                report = Path(temporary) / "playwright.json"
                environment = dict(os.environ, PLAYWRIGHT_JSON_OUTPUT_FILE=str(report),
                                   TEST_SHARD_PORT=str(available_port()))
                environment.pop("ASTRO_PREVIEW_BACKGROUND", None)
                environment.pop("ASTRO_DEV_BACKGROUND", None)
                subprocess.run([*command, "--reporter=list,json"], cwd=directory, env=environment, check=True)
                workspace = str(directory.relative_to(self.root))
                self.save(ui_test_ids(json.loads(report.read_text(encoding="utf-8")), workspace))
        elif command[:4] == ["pnpm", "exec", "vitest", "run"]:
            self.history.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=self.history.parent) as temporary:
                report = Path(temporary) / "vitest.json"
                subprocess.run([*command, "--reporter=default", "--reporter=json", f"--outputFile.json={report}"],
                               cwd=directory, check=True)
                self.save(vitest_ids(json.loads(report.read_text(encoding="utf-8")), self.root))
        else:
            subprocess.run(command, cwd=directory, check=True)

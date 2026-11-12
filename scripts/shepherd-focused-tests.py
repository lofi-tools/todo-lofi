#!/usr/bin/env python3
"""Run default unit tests for directly changed workspace crates."""

import argparse
import importlib.util
import subprocess
import sys
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("test_shards", ROOT / "scripts/test-shards.py")
SHARDS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SHARDS)


def select(paths, roots, omitted, web_roots=None, omitted_web=frozenset()):
    packages = set()
    ui = False
    shared = False
    runner_tests = False
    web_packages = set()
    candidates = [(root, name, False) for root, name in roots.items()]
    candidates += [(root, name, True) for root, name in (web_roots or {}).items()]
    candidates.sort(key=lambda item: len(item[0]), reverse=True)
    for path in paths:
        path = str(PurePosixPath(path))
        if path == "apps/shepherd/ui" or path.startswith("apps/shepherd/ui/"):
            ui = True
            continue
        if path.startswith(("scripts/test_shard", "scripts/test_test_shard")) or path in {
            "scripts/test-shards.py", "scripts/shepherd-focused-tests.py", "scripts/python-unit-shard.py"
        }:
            runner_tests = True
            continue
        for root, name, is_web in candidates:
            if path == root or path.startswith(root + "/"):
                if is_web and name not in omitted_web:
                    web_packages.add(name)
                elif not is_web and name not in omitted:
                    packages.add(name)
                break
        else:
            if path.startswith("demos/"):
                continue
            if path.endswith(".md"):
                continue
            shared = True
    if shared:
        packages.update(set(roots.values()) - omitted)
        web_packages.update(set((web_roots or {}).values()) - omitted_web)
        ui = True
    return sorted(packages), ui, shared, runner_tests, web_packages


def full_due():
    head = SHARDS.output("git", "rev-parse", "HEAD")
    return SHARDS.full_suite_due(SHARDS.read_runs(SHARDS.HISTORY), SHARDS.first_parent_ancestors(ROOT, head))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--git", action="store_true", help="include opt-in Git-backed tests in selected crates")
    parser.add_argument("--pre-commit", action="store_true", help="run one rotating shard of workspace unit tests")
    parser.add_argument("--package", help="package containing an exact test to rerun")
    parser.add_argument("--test", help="fully qualified test name to rerun")
    args = parser.parse_args()
    if bool(args.package) != bool(args.test):
        parser.error("--package and --test must be used together")
    if args.pre_commit and args.test:
        parser.error("--pre-commit cannot be combined with --test")
    if full_due():
        print("A 16-commit full-suite run is due: direnv exec . python3 scripts/test-shards.py --all", file=sys.stderr)
    if args.pre_commit:
        if args.git:
            parser.error("--git cannot be combined with --pre-commit")
        SHARDS.main(["--pre-commit", *(["--dry-run"] if args.dry_run else [])])
        return
    if args.test:
        command = ["cargo", "nextest", "run", "-p", args.package]
        library_test = SHARDS.library_test_name(args.test, args.package, "git" if args.git else None)
        if library_test is not None:
            command.append("--lib")
        if args.git:
            command.extend(["--features", "git"])
        command.extend(["--", "--exact", library_test or args.test])
        print("+", subprocess.list2cmdline(command), flush=True)
        if not args.dry_run and subprocess.run(command, cwd=ROOT).returncode != 0:
            sys.exit(1)
        return
    roots, dependents = SHARDS.workspace()
    omitted = SHARDS.excluded_packages(roots, dependents)
    web_roots, web_dependents, _ = SHARDS.web_workspace()
    omitted_web = SHARDS.excluded_packages(web_roots, web_dependents)
    packages, ui, shared, runner_tests, web_packages = select(
        SHARDS.changed_paths(), roots, omitted, web_roots, omitted_web
    )
    if shared:
        print("Shared or unknown paths changed; selecting all supported workspace unit suites.", flush=True)
    commands = []
    unsupported = False
    if runner_tests:
        commands.append(["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_test_shard*.py"])
    if packages:
        command = ["cargo", "nextest", "run", *[argument for package in packages for argument in ("-p", package)], "--no-fail-fast"]
        if args.git and {"shepherd", "shepherd-core", "terminal-replay"}.intersection(packages):
            command.extend(["--features", "git"])
        commands.append(command)
    web_suites = {str(directory.relative_to(ROOT)): script for directory, script in SHARDS.web_test_suites()}
    for root, name in sorted(web_roots.items()):
        if name not in web_packages:
            continue
        script = web_suites.get(root)
        if script and script.strip() == "vitest run":
            commands.append(["pnpm", "--dir", root, "exec", "vitest", "run"])
        elif script:
            print(f"No supported unit-test runner for {root}; inspect its test script separately.", file=sys.stderr)
            unsupported = True
    if ui:
        print("Shepherd UI changed; browser tests are opt-in with test-shards.py --all --browser.", flush=True)
    if not commands:
        print("No affected unit-test commands selected.")
    failed = unsupported
    for command in commands:
        print("+", subprocess.list2cmdline(command), flush=True)
        if not args.dry_run:
            failed |= subprocess.run(command, cwd=ROOT).returncode != 0
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"shepherd-focused-tests: {error}", file=sys.stderr)
        sys.exit(1)

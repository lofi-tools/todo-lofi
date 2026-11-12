#!/usr/bin/env python3
"""Run affected tests and a reproducible rotating shard without Git hooks."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

from test_shard_history import first_parent_ancestors, full_suite_due, last_test_run, read_runs
from test_shard_recording import Recorder
from test_sharding import affected_tests, excluded_packages, shard_for_revision

ROOT = Path(__file__).resolve().parent.parent
UI = ROOT / "apps/shepherd/ui"
HISTORY = ROOT / ".cache/test-shards/runs.jsonl"
RECORDER = None
CONTEXT = None


def web_workspace():
    patterns = ("apps/*/package.json", "apps/*/ui/package.json", "libs/*/package.json", "demos/*/package.json")
    manifests = sorted({path for pattern in patterns for path in ROOT.glob(pattern)})
    packages = [(path, json.loads(path.read_text(encoding="utf-8"))) for path in manifests]
    roots = {str(path.parent.relative_to(ROOT)): data["name"] for path, data in packages}
    names = set(roots.values())
    dependents = {}
    for _, data in packages:
        for section in ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies"):
            for dependency in data.get(section, {}):
                if dependency in names:
                    dependents.setdefault(dependency, set()).add(data["name"])
    return roots, dependents, packages


def playwright_suites():
    roots, dependents, _ = web_workspace()
    omitted = excluded_packages(roots, dependents)
    excluded_roots = {ROOT / path for path, name in roots.items() if name in omitted}
    patterns = ("apps/*/playwright.config.*", "apps/*/ui/playwright.config.*", "libs/*/playwright.config.*")
    return sorted({config.parent for pattern in patterns for config in ROOT.glob(pattern)
                   if config.parent not in excluded_roots})


def run_playwright(index=None, count=None, affected_directories=frozenset(), dry_run=False):
    for directory in playwright_suites():
        command = ["pnpm", "exec", "playwright", "test"]
        if index is not None and directory not in affected_directories:
            command.extend(["--pass-with-no-tests", "--shard", f"{index}/{count}"])
        execute(command, directory, dry_run)


def web_test_suites():
    roots, dependents, packages = web_workspace()
    omitted = excluded_packages(roots, dependents)
    for manifest, data in packages:
        script = data.get("scripts", {}).get("test")
        if script and data["name"] not in omitted:
            yield manifest.parent, script


def run_web_unit_tests(index=None, count=None, affected_directories=frozenset(), dry_run=False):
    for directory, script in web_test_suites():
        if script.strip() != "vitest run":
            execute(["pnpm", "run", "test"], directory, dry_run)
            continue
        command = ["pnpm", "exec", "vitest", "run"]
        if index is not None and directory not in affected_directories:
            command.extend(["--passWithNoTests", "--shard", f"{index}/{count}"])
        execute(command, directory, dry_run)


def output(*command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def git_paths(*arguments):
    data = subprocess.check_output(["git", *arguments, "-z"], cwd=ROOT)
    return [path.decode("utf-8", "surrogateescape") for path in data.split(b"\0") if path]


def changed_paths():
    parent = subprocess.run(["git", "rev-parse", "--verify", "HEAD^"], cwd=ROOT, capture_output=True)
    if parent.returncode == 0:
        paths = set(git_paths("diff", "--no-renames", "--name-only", "HEAD^", "HEAD"))
    else:
        paths = set(git_paths("diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "HEAD"))
    paths.update(git_paths("diff", "--no-renames", "--name-only", "HEAD"))
    paths.update(git_paths("ls-files", "--others", "--exclude-standard"))
    return sorted(paths)


def workspace():
    metadata = json.loads(output("cargo", "metadata", "--no-deps", "--format-version", "1"))
    members = set(metadata["workspace_members"])
    packages = [package for package in metadata["packages"] if package["id"] in members]
    roots = {str(Path(package["manifest_path"]).parent.relative_to(ROOT)): package["name"] for package in packages}
    by_absolute_path = {str((ROOT / path).resolve()): name for path, name in roots.items()}
    dependents = {}
    for package in packages:
        for dependency in package["dependencies"]:
            path = dependency.get("path")
            if path and str(Path(path).resolve()) in by_absolute_path:
                dependency_name = by_absolute_path[str(Path(path).resolve())]
                dependents.setdefault(dependency_name, set()).add(package["name"])
    return roots, dependents


def execute(command, directory=ROOT, dry_run=False):
    global RECORDER
    print(f"+ [{directory.relative_to(ROOT)}]", subprocess.list2cmdline(command), flush=True)
    if not dry_run:
        if RECORDER is None:
            RECORDER = Recorder(ROOT, HISTORY, *CONTEXT)
        RECORDER.run(command, directory)


def library_test_name(test, package=None, features=None):
    """The fully qualified name of a unique matching library test."""
    selection = ["--package", package] if package else ["--workspace"]
    if features:
        selection.extend(["--features", features])
    completed = subprocess.run(
        ["cargo", "nextest", "list", *selection, "--lib", "--message-format", "json"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if completed.returncode != 0:
        return None
    try:
        suites = json.loads(completed.stdout).get("rust-suites", {})
    except json.JSONDecodeError:
        return None
    matches = [
        name
        for suite in suites.values()
        for name in suite.get("testcases", {})
        if name == test or name.endswith(f"::{test}")
    ]
    if test in matches:
        return test
    return matches[0] if len(matches) == 1 else None


def parse_shard(value):
    try:
        index, count = map(int, value.split("/"))
        if count < 1 or not 1 <= index <= count:
            raise ValueError
        return index, count
    except ValueError as error:
        raise argparse.ArgumentTypeError("shard must be INDEX/COUNT, with 1 <= INDEX <= COUNT") from error


def main(argv=None):
    global RECORDER, CONTEXT
    RECORDER = None
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--all", action="store_true", help="run all default Rust and UI tests, including doctests")
    parser.add_argument("--browser", action="store_true", help="include browser suites with --all")
    parser.add_argument("--pre-commit", action="store_true", help="run one shard of workspace unit tests")
    parser.add_argument("--test", help="run an exact Rust test name, preferring its library target")
    parser.add_argument("--package", help="limit --test to a Rust workspace package")
    parser.add_argument("--last-ran", help="find the latest clean ancestor that ran an exact test identity")
    parser.add_argument("--seed", default="HEAD", help="revision used to select the shard (default: HEAD)")
    parser.add_argument("--shard", type=parse_shard, help="override the shard as INDEX/COUNT")
    parser.add_argument("--shards", type=int, default=8, help="number of rotating shards (default: 8)")
    parser.add_argument("--dry-run", action="store_true", help="show commands without running tests")
    args = parser.parse_args(argv)
    if args.shards < 1:
        parser.error("--shards must be positive")
    if sum((args.all, args.pre_commit, bool(args.test))) > 1:
        parser.error("--all, --pre-commit and --test cannot be combined")
    if args.browser and not args.all:
        parser.error("--browser requires --all")
    if args.package and not args.test:
        parser.error("--package requires --test")
    if args.last_ran:
        ancestors = first_parent_ancestors(ROOT, "HEAD")
        record = last_test_run(read_runs(HISTORY), ancestors, args.last_ran)
        if record is None:
            print("No recorded clean successful ancestor ran that exact test; history may be incomplete.")
            return
        print(f"{record['revision']} {record['timestamp']} {args.last_ran}")
        return
    revision = output("git", "rev-parse", "--verify", f"{args.seed}^{{commit}}")
    head = output("git", "rev-parse", "HEAD")
    index, count = args.shard or (shard_for_revision(revision, args.shards), args.shards)
    CONTEXT = (head, revision, f"{index}/{count}")
    print(f"Selection seed {revision}; shard {index}/{count}", flush=True)
    if args.test:
        selection = ["--package", args.package] if args.package else ["--workspace"]
        # A matching library test needs only the library target; integration
        # targets are still built when the test lives there instead.
        library_test = library_test_name(args.test, args.package)
        if library_test is not None:
            selection.append("--lib")
        execute(
            ["cargo", "nextest", "run", *selection, "--", "--exact", library_test or args.test],
            dry_run=args.dry_run,
        )
        return
    roots, dependents = workspace()
    omitted = excluded_packages(roots, dependents)
    excluded = [item for name in sorted(omitted) for item in ("--exclude", name)]
    if args.all:
        execute(["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_test_shard*.py"], dry_run=args.dry_run)
        execute(["cargo", "nextest", "run", "--workspace", *excluded, "--features", "shepherd/git,shepherd-core/git,terminal-replay/git", "--no-fail-fast"], dry_run=args.dry_run)
        execute(["cargo", "test", "--workspace", *excluded, "--doc"], dry_run=args.dry_run)
        run_web_unit_tests(dry_run=args.dry_run)
        if args.browser:
            run_playwright(dry_run=args.dry_run)
        if RECORDER is not None:
            RECORDER.save([], full=True)
        return
    if args.pre_commit:
        web_suites = list(web_test_suites())
        unsupported = [str(directory.relative_to(ROOT)) for directory, script in web_suites if script.strip() != "vitest run"]
        if unsupported:
            parser.error("cannot shard test scripts in: " + ", ".join(unsupported))
        execute(["cargo", "nextest", "run", "--workspace", *excluded, "--partition", f"hash:{index}/{count}", "--no-tests=pass", "--no-fail-fast"], dry_run=args.dry_run)
        execute(["python3", "scripts/python-unit-shard.py", "--shard", f"{index}/{count}"], dry_run=args.dry_run)
        run_web_unit_tests(index, count, dry_run=args.dry_run)
        return
    if full_suite_due(read_runs(HISTORY), first_parent_ancestors(ROOT, head)):
        print("A full default-suite run is due: use --all.", file=sys.stderr)
    paths = [path for path in changed_paths() if not path.startswith("demos/")]
    web_roots, web_dependents, _ = web_workspace()
    selection = affected_tests(paths, roots, dependents, web_roots, web_dependents)
    if selection.all_tests:
        print("Unknown or shared paths changed; running default unit tests.", flush=True)
        execute(["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_test_shard*.py"], dry_run=args.dry_run)
        execute(["cargo", "nextest", "run", "--workspace", *excluded, "--no-fail-fast"], dry_run=args.dry_run)
        run_web_unit_tests(dry_run=args.dry_run)
        return
    affected = selection.packages - omitted
    if "shepherd-app" in selection.web_packages and "shepherd" in roots.values():
        affected |= {"shepherd"}
    if affected:
        packages = [item for name in sorted(affected) for item in ("-p", name)]
        execute(["cargo", "nextest", "run", *packages], dry_run=args.dry_run)
    excluded = [item for name in sorted(omitted | affected) for item in ("--exclude", name)]
    if affected != frozenset(roots.values()) - omitted:
        execute(["cargo", "nextest", "run", "--workspace", *excluded, "--partition", f"hash:{index}/{count}", "--no-tests=pass"], dry_run=args.dry_run)
    omitted_web = excluded_packages(web_roots, web_dependents)
    affected_directories = {ROOT / path for path, name in web_roots.items()
                            if name in selection.web_packages - omitted_web}
    if "shepherd-app" in selection.web_packages or affected.intersection({"shepherd", "shepherd-core", "agent-harness"}):
        affected_directories.add(UI)
    run_web_unit_tests(index, count, affected_directories, args.dry_run)


if __name__ == "__main__":
    try:
        main()
    except (subprocess.CalledProcessError, OSError) as error:
        print(f"test-shards: {error}", file=sys.stderr)
        sys.exit(1)

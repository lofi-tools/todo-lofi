#!/usr/bin/env python3
"""Run affected tests and a reproducible rotating shard without Git hooks."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

from test_sharding import affected_tests, shard_for_revision

ROOT = Path(__file__).resolve().parent.parent
UI = ROOT / "apps/shepherd/ui"


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
    print("+", subprocess.list2cmdline(command), flush=True)
    if not dry_run:
        subprocess.run(command, cwd=directory, check=True)


def parse_shard(value):
    try:
        index, count = map(int, value.split("/"))
        if count < 1 or not 1 <= index <= count:
            raise ValueError
        return index, count
    except ValueError as error:
        raise argparse.ArgumentTypeError("shard must be INDEX/COUNT, with 1 <= INDEX <= COUNT") from error


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--all", action="store_true", help="run all default Rust and UI tests, including doctests")
    parser.add_argument("--test", help="run an exact Rust test name across the workspace")
    parser.add_argument("--seed", default="HEAD", help="revision used to select the shard (default: HEAD)")
    parser.add_argument("--shard", type=parse_shard, help="override the shard as INDEX/COUNT")
    parser.add_argument("--shards", type=int, default=8, help="number of rotating shards (default: 8)")
    parser.add_argument("--dry-run", action="store_true", help="show commands without running tests")
    args = parser.parse_args(argv)
    if args.shards < 1:
        parser.error("--shards must be positive")
    if args.all and args.test:
        parser.error("--all and --test cannot be combined")
    revision = output("git", "rev-parse", "--verify", f"{args.seed}^{{commit}}")
    index, count = args.shard or (shard_for_revision(revision, args.shards), args.shards)
    print(f"Selection seed {revision}; shard {index}/{count}", flush=True)
    if args.test:
        execute(["cargo", "nextest", "run", "--workspace", "--", "--exact", args.test], dry_run=args.dry_run)
        return
    if args.all:
        execute(["cargo", "nextest", "run", "--workspace"], dry_run=args.dry_run)
        execute(["cargo", "test", "--workspace", "--doc"], dry_run=args.dry_run)
        execute(["pnpm", "exec", "playwright", "test"], UI, args.dry_run)
        return
    commit_count = int(output("git", "rev-list", "--first-parent", "--count", "HEAD"))
    if commit_count % 8 == 0:
        print("A full default-suite run is due: use --all.", file=sys.stderr)
    roots, dependents = workspace()
    selection = affected_tests(changed_paths(), roots, dependents)
    if selection.all_tests:
        print("Unknown or shared paths changed; running the full default suite.", flush=True)
        execute(["cargo", "nextest", "run", "--workspace"], dry_run=args.dry_run)
        execute(["cargo", "test", "--workspace", "--doc"], dry_run=args.dry_run)
        execute(["pnpm", "exec", "playwright", "test"], UI, args.dry_run)
        return
    if selection.packages:
        packages = [item for name in sorted(selection.packages) for item in ("-p", name)]
        execute(["cargo", "nextest", "run", *packages], dry_run=args.dry_run)
    excluded = [item for name in sorted(selection.packages) for item in ("--exclude", name)]
    if selection.packages != frozenset(roots.values()):
        execute(["cargo", "nextest", "run", "--workspace", *excluded, "--partition", f"hash:{index}/{count}", "--no-tests=pass"], dry_run=args.dry_run)
    ui_command = ["pnpm", "exec", "playwright", "test", "--pass-with-no-tests"]
    if not selection.ui and not selection.packages.intersection({"shepherd", "shepherd-core", "agent-harness"}):
        ui_command.extend(["--shard", f"{index}/{count}"])
    execute(ui_command, UI, args.dry_run)


if __name__ == "__main__":
    try:
        main()
    except (subprocess.CalledProcessError, OSError) as error:
        print(f"test-shards: {error}", file=sys.stderr)
        sys.exit(1)

#!/usr/bin/env python3
"""Selection policy shared by the explicit sharded test runner."""

from dataclasses import dataclass
from pathlib import PurePosixPath

PROTOTYPE_PACKAGES = frozenset({"dson_freeform_task", "dson_typed_task", "test_dson", "op_crdt"})


@dataclass(frozen=True)
class Selection:
    packages: frozenset[str]
    ui: bool
    all_tests: bool


def excluded_packages(packages: dict[str, str], dependents: dict[str, set[str]]) -> frozenset[str]:
    names = set(packages.values())
    demos = {name for root, name in packages.items() if root.startswith("demos/")}
    dependencies = {name: set() for name in names}
    for dependency, users in dependents.items():
        for user in users:
            if user in dependencies and dependency in names:
                dependencies[user].add(dependency)

    def reachable(start):
        found = set(start)
        pending = list(start)
        while pending:
            for dependency in dependencies[pending.pop()] - found:
                found.add(dependency)
                pending.append(dependency)
        return found

    demo_closure = reachable(demos)
    apps = {name for root, name in packages.items() if root.startswith("apps/")}
    required_elsewhere = reachable((names - demo_closure) | apps)
    demo_only_libraries = {name for root, name in packages.items()
                           if root.startswith("libs/") and name in demo_closure - required_elsewhere}
    return frozenset(demos | demo_only_libraries | (PROTOTYPE_PACKAGES & names))


def shard_for_revision(revision: str, count: int) -> int:
    if count < 1:
        raise ValueError("shard count must be positive")
    return int(revision, 16) % count + 1


def affected_tests(paths: list[str], packages: dict[str, str], dependents: dict[str, set[str]]) -> Selection:
    """Map repository paths to packages, conservatively selecting all for unknown paths."""
    affected: set[str] = set()
    ui = False
    roots = sorted(packages.items(), key=lambda item: len(item[0]), reverse=True)
    for path in paths:
        normalized = str(PurePosixPath(path))
        if normalized == "apps/shepherd/ui" or normalized.startswith("apps/shepherd/ui/"):
            ui = True
            continue
        for root, name in roots:
            if normalized == root or normalized.startswith(root + "/"):
                affected.add(name)
                break
        else:
            return Selection(frozenset(packages.values()), True, True)

    pending = list(affected)
    while pending:
        for dependent in dependents.get(pending.pop(), set()):
            if dependent not in affected:
                affected.add(dependent)
                pending.append(dependent)
    return Selection(frozenset(affected), ui, False)

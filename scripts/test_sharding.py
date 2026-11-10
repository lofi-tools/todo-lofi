#!/usr/bin/env python3
"""Selection policy shared by the explicit sharded test runner."""

from dataclasses import dataclass
from pathlib import PurePosixPath


@dataclass(frozen=True)
class Selection:
    packages: frozenset[str]
    ui: bool
    all_tests: bool


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

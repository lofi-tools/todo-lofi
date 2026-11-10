"""Record verified test runs and query their Git ancestry."""

import json
import subprocess
from datetime import datetime, timezone
from pathlib import Path


def append_run(path: Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {"timestamp": datetime.now(timezone.utc).isoformat(), **record}
    with path.open("a", encoding="utf-8") as history:
        history.write(json.dumps(record, sort_keys=True) + "\n")


def read_runs(path: Path) -> list[dict]:
    if not path.exists():
        return []
    with path.open(encoding="utf-8") as history:
        return [json.loads(line) for line in history if line.strip()]


def last_test_run(records: list[dict], ancestors: list[str], test: str):
    """Find a clean successful run of an exact test identity on an ancestor."""
    by_revision = {}
    for record in records:
        if record.get("clean") and record.get("success") and test in record.get("tests", []):
            by_revision.setdefault(record["revision"], []).append(record)
    for ancestor in ancestors:
        if ancestor in by_revision:
            return by_revision[ancestor][-1]
    return None


def first_parent_ancestors(root: Path, revision: str) -> list[str]:
    output = subprocess.check_output(["git", "rev-list", "--first-parent", revision], cwd=root, text=True)
    return output.splitlines()


def full_suite_due(records: list[dict], ancestors: list[str], interval: int = 8) -> bool:
    successful_full = {record["revision"] for record in records
                       if record.get("clean") and record.get("success") and record.get("full")}
    for distance, ancestor in enumerate(ancestors):
        if ancestor in successful_full:
            return distance >= interval
    return len(ancestors) >= interval

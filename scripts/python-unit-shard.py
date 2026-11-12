#!/usr/bin/env python3
"""Run one deterministic shard of the test-runner unit tests."""

import argparse
import hashlib
import sys
import unittest
from pathlib import Path

from test_sharding import shard_for_revision

ROOT = Path(__file__).resolve().parent.parent


def cases(suite):
    for item in suite:
        if isinstance(item, unittest.TestSuite):
            yield from cases(item)
        else:
            yield item


def shard_for_test(name, count):
    digest = hashlib.sha256(name.encode("utf-8")).hexdigest()
    return shard_for_revision(digest, count)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shard", required=True)
    args = parser.parse_args()
    try:
        index, count = map(int, args.shard.split("/"))
        if count < 1 or not 1 <= index <= count:
            raise ValueError
    except ValueError:
        parser.error("shard must be INDEX/COUNT, with 1 <= INDEX <= COUNT")
    discovered = unittest.defaultTestLoader.discover(str(ROOT / "scripts"), pattern="test_test_shard*.py")
    selected = unittest.TestSuite(test for test in cases(discovered) if shard_for_test(test.id(), count) == index)
    return 0 if unittest.TextTestRunner(verbosity=1).run(selected).wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())

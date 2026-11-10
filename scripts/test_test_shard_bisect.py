import importlib.util
import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("shard_bisect", Path(__file__).with_name("bisect-rust-test.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def inventory(name="the_test"):
    return json.dumps({"rust-suites": {"binary": {"testcases": {name: {"ignored": False, "filter-match": {"status": "matches"}}}}}})


class BisectTests(unittest.TestCase):
    def test_missing_test_is_skipped(self):
        with patch.object(module.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, inventory("other"))):
            self.assertEqual(module.main(["the_test"]), 125)

    def test_failure_and_infrastructure_errors_are_distinct(self):
        for status, expected in [(0, 0), (100, 1), (101, 125)]:
            with self.subTest(status=status), patch.object(module.subprocess, "run", side_effect=[
                subprocess.CompletedProcess([], 0, inventory()), subprocess.CompletedProcess([], status),
            ]):
                self.assertEqual(module.main(["the_test"]), expected)


if __name__ == "__main__":
    unittest.main()

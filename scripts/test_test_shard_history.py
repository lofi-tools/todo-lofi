import tempfile
import unittest
from pathlib import Path

from test_shard_history import append_run, full_suite_due, last_test_run, read_runs


class HistoryTests(unittest.TestCase):
    def test_round_trip_records_and_missing_history(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history" / "runs.jsonl"
            self.assertEqual(read_runs(path), [])
            append_run(path, {"revision": "abc", "success": True})
            self.assertEqual(read_runs(path)[0]["revision"], "abc")
            self.assertIn("timestamp", read_runs(path)[0])

    def test_last_run_requires_clean_successful_ancestor_and_exact_name(self):
        records = [
            {"revision": "a", "success": True, "clean": True, "tests": ["rust:pkg::case"]},
            {"revision": "b", "success": True, "clean": False, "tests": ["rust:pkg::case"]},
            {"revision": "c", "success": False, "clean": True, "tests": ["rust:pkg::case"]},
        ]
        self.assertEqual(last_test_run(records, ["c", "b", "a"], "rust:pkg::case")["revision"], "a")
        self.assertIsNone(last_test_run(records, ["c", "b"], "rust:pkg::case"))
        self.assertIsNone(last_test_run(records, ["a"], "case"))

    def test_full_suite_remains_due_until_a_clean_success(self):
        ancestors = [str(index) for index in range(12)]
        records = [{"revision": "8", "success": True, "clean": True, "full": True},
                   {"revision": "1", "success": True, "clean": False, "full": True}]
        self.assertTrue(full_suite_due(records, ancestors))
        self.assertFalse(full_suite_due(records, ancestors[:7]))
        records.append({"revision": "2", "success": True, "clean": True, "full": True})
        self.assertFalse(full_suite_due(records, ancestors))


if __name__ == "__main__":
    unittest.main()

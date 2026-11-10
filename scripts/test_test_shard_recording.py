import unittest
from test_shard_recording import rust_list_command, rust_test_ids, ui_test_ids


class RecordingTests(unittest.TestCase):
    def test_only_selected_nonignored_rust_tests_are_recorded(self):
        data = {"rust-suites": {"suite": {"binary-id": "crate::integration", "testcases": {
            "runs": {"ignored": False, "filter-match": {"status": "matches"}},
            "ignored": {"ignored": True, "filter-match": {"status": "matches"}},
            "excluded": {"ignored": False, "filter-match": {"status": "mismatch"}},
        }}}}
        self.assertEqual(rust_test_ids(data), ["rust:crate::integration::runs"])

    def test_list_preserves_exact_filter_and_removes_run_only_flags(self):
        command = ["cargo", "nextest", "run", "--workspace", "--no-tests=pass", "--", "--exact", "example"]
        self.assertEqual(rust_list_command(command), ["cargo", "nextest", "list", "--workspace", "--message-format", "json", "--", "--exact", "example"])

    def test_only_passed_ui_results_are_recorded(self):
        data = {"suites": [{"title": "file", "specs": [{"title": "works", "file": "one.spec.ts", "tests": [
            {"projectName": "chromium", "results": [{"status": "passed"}]},
            {"projectName": "other", "results": [{"status": "skipped"}]},
        ]}]}]}
        self.assertEqual(ui_test_ids(data), ["ui:chromium:one.spec.ts::file::works"])


if __name__ == "__main__":
    unittest.main()

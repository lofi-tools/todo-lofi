import unittest
import json
import os
import subprocess
import tempfile
from pathlib import Path
from unittest.mock import patch
from test_shard_recording import Recorder, rust_list_command, rust_test_ids, ui_test_ids, vitest_ids


class RecordingTests(unittest.TestCase):
    def test_only_selected_nonignored_rust_tests_are_recorded(self):
        data = {"rust-suites": {"suite": {"binary-id": "crate::integration", "testcases": {
            "runs": {"ignored": False, "filter-match": {"status": "matches"}},
            "ignored": {"ignored": True, "filter-match": {"status": "matches"}},
            "excluded": {"ignored": False, "filter-match": {"status": "mismatch"}},
        }}}}
        self.assertEqual(rust_test_ids(data), ["rust:crate::integration::runs"])

    def test_list_preserves_exact_filter_and_removes_run_only_flags(self):
        command = ["cargo", "nextest", "run", "--workspace", "--no-tests=pass", "--no-fail-fast", "--", "--exact", "example"]
        self.assertEqual(rust_list_command(command), ["cargo", "nextest", "list", "--workspace", "--message-format", "json", "--", "--exact", "example"])

    def test_only_passed_ui_results_are_recorded(self):
        data = {"suites": [{"title": "file", "specs": [{"title": "works", "file": "one.spec.ts", "tests": [
            {"projectName": "chromium", "results": [{"status": "passed"}]},
            {"projectName": "other", "results": [{"status": "skipped"}]},
        ]}]}]}
        self.assertEqual(ui_test_ids(data, "apps/example"), ["ui:apps/example:chromium:one.spec.ts::file::works"])

    def test_only_passed_vitest_cases_are_recorded(self):
        data = {"testResults": [{"name": "/repo/tests/one.test.ts", "assertionResults": [
            {"fullName": "group runs", "status": "passed"},
            {"fullName": "group skipped", "status": "pending"},
        ]}]}
        self.assertEqual(vitest_ids(data, Path("/repo")), ["vitest:tests/one.test.ts::group runs"])

    def test_browser_run_uses_own_port_and_foreground_astro(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            recorder = Recorder.__new__(Recorder)
            recorder.root = root
            recorder.history = root / "runs.jsonl"
            environments = []

            def run(command, *, cwd, env, check):
                environments.append(env)
                Path(env["PLAYWRIGHT_JSON_OUTPUT_FILE"]).write_text(json.dumps({"suites": []}))
                return subprocess.CompletedProcess(command, 0)

            with patch.dict(os.environ, {"ASTRO_PREVIEW_BACKGROUND": "1", "ASTRO_DEV_BACKGROUND": "1"}), \
                 patch("test_shard_recording.available_port", return_value=45678), \
                 patch("test_shard_recording.subprocess.run", side_effect=run), \
                 patch.object(recorder, "save") as save:
                recorder.run(["pnpm", "exec", "playwright", "test"], root)
            self.assertEqual(environments[0]["TEST_SHARD_PORT"], "45678")
            self.assertNotIn("ASTRO_PREVIEW_BACKGROUND", environments[0])
            self.assertNotIn("ASTRO_DEV_BACKGROUND", environments[0])
            save.assert_called_once_with([])


if __name__ == "__main__":
    unittest.main()

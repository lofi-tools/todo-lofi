import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "shepherd_focused_tests", Path(__file__).with_name("shepherd-focused-tests.py")
)
FOCUSED = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FOCUSED)


class FocusedSelectionTests(unittest.TestCase):
    def setUp(self):
        self.roots = {
            "apps/shepherd/src-tauri": "shepherd",
            "libs/shepherd-core": "shepherd-core",
            "libs/terminal-renderer": "terminal-renderer",
            "demos/example": "example",
        }
        self.omitted = {"example"}

    def test_selects_changed_crates_without_dependents(self):
        self.assertEqual(
            FOCUSED.select(["libs/shepherd-core/src/tasks.rs"], self.roots, self.omitted),
            (["shepherd-core"], False, False, False, set()),
        )

    def test_selects_ui_without_rust_crates(self):
        self.assertEqual(
            FOCUSED.select(["apps/shepherd/ui/src/App.tsx"], self.roots, self.omitted),
            ([], True, False, False, set()),
        )

    def test_shared_paths_select_supported_packages_and_ui(self):
        self.assertEqual(
            FOCUSED.select(["Cargo.lock"], self.roots, self.omitted),
            (["shepherd", "shepherd-core", "terminal-renderer"], True, True, False, set()),
        )

    def test_omits_demo_paths(self):
        self.assertEqual(FOCUSED.select(["demos/example/file.rs"], self.roots, self.omitted), ([], False, False, False, set()))

    def test_runner_changes_select_runner_unit_tests_without_workspace_crates(self):
        self.assertEqual(
            FOCUSED.select(["scripts/test_shard_history.py"], self.roots, self.omitted),
            ([], False, False, True, set()),
        )

    def test_shared_documentation_does_not_select_workspace_tests(self):
        self.assertEqual(FOCUSED.select(["AGENTS.md"], self.roots, self.omitted), ([], False, False, False, set()))

    def test_web_changes_select_only_the_changed_web_package(self):
        self.assertEqual(
            FOCUSED.select(["apps/site/src/page.ts"], self.roots, self.omitted, {"apps/site": "site"}),
            ([], False, False, False, {"site"}),
        )

    def test_rust_failure_is_reported_without_running_browser_tests(self):
        with (
            patch.object(FOCUSED.SHARDS, "workspace", return_value=(self.roots, {})),
            patch.object(FOCUSED.SHARDS, "web_workspace", return_value=({}, {}, [])),
            patch.object(FOCUSED.SHARDS, "web_test_suites", return_value=[]),
            patch.object(FOCUSED.SHARDS, "excluded_packages", return_value=self.omitted),
            patch.object(FOCUSED.SHARDS, "changed_paths", return_value=["Cargo.lock"]),
            patch.object(FOCUSED, "full_due", return_value=False),
            patch.object(FOCUSED.sys, "argv", ["shepherd-focused-tests.py"]),
            patch.object(FOCUSED.subprocess, "run") as run,
        ):
            run.return_value = FOCUSED.subprocess.CompletedProcess([], 1)
            with self.assertRaises(SystemExit) as error:
                FOCUSED.main()
            self.assertEqual(error.exception.code, 1)
            self.assertEqual(run.call_count, 1)
            self.assertIn("--no-fail-fast", run.call_args_list[0].args[0])

    def test_git_flag_enables_opt_in_tests_for_selected_crates(self):
        with (
            patch.object(FOCUSED.SHARDS, "workspace", return_value=(self.roots, {})),
            patch.object(FOCUSED.SHARDS, "web_workspace", return_value=({}, {}, [])),
            patch.object(FOCUSED.SHARDS, "web_test_suites", return_value=[]),
            patch.object(FOCUSED.SHARDS, "excluded_packages", return_value=self.omitted),
            patch.object(FOCUSED.SHARDS, "changed_paths", return_value=["libs/shepherd-core/src/tasks.rs"]),
            patch.object(FOCUSED, "full_due", return_value=False),
            patch.object(FOCUSED.sys, "argv", ["shepherd-focused-tests.py", "--git"]),
            patch.object(FOCUSED.subprocess, "run") as run,
        ):
            run.return_value = FOCUSED.subprocess.CompletedProcess([], 0)
            FOCUSED.main()
            self.assertEqual(run.call_args.args[0][-2:], ["--features", "git"])

    def test_git_flag_enables_replay_git_tests_for_changed_replay_crate(self):
        roots = self.roots | {"libs/terminal-replay": "terminal-replay"}
        with (
            patch.object(FOCUSED.SHARDS, "workspace", return_value=(roots, {})),
            patch.object(FOCUSED.SHARDS, "web_workspace", return_value=({}, {}, [])),
            patch.object(FOCUSED.SHARDS, "web_test_suites", return_value=[]),
            patch.object(FOCUSED.SHARDS, "excluded_packages", return_value=self.omitted),
            patch.object(FOCUSED.SHARDS, "changed_paths", return_value=["libs/terminal-replay/src/terminal_replay.rs"]),
            patch.object(FOCUSED, "full_due", return_value=False),
            patch.object(FOCUSED.sys, "argv", ["shepherd-focused-tests.py", "--git"]),
            patch.object(FOCUSED.subprocess, "run") as run,
        ):
            run.return_value = FOCUSED.subprocess.CompletedProcess([], 0)
            FOCUSED.main()
            self.assertEqual(run.call_args.args[0][-2:], ["--features", "git"])

    def test_web_change_runs_its_unit_suite(self):
        with (
            patch.object(FOCUSED.SHARDS, "workspace", return_value=(self.roots, {})),
            patch.object(FOCUSED.SHARDS, "web_workspace", return_value=({"apps/site": "site"}, {}, [])),
            patch.object(FOCUSED.SHARDS, "web_test_suites", return_value=[(FOCUSED.ROOT / "apps/site", "vitest run")]),
            patch.object(FOCUSED.SHARDS, "excluded_packages", return_value=self.omitted),
            patch.object(FOCUSED.SHARDS, "changed_paths", return_value=["apps/site/src/page.ts"]),
            patch.object(FOCUSED, "full_due", return_value=False),
            patch.object(FOCUSED.sys, "argv", ["shepherd-focused-tests.py"]),
            patch.object(FOCUSED.subprocess, "run") as run,
        ):
            run.return_value = FOCUSED.subprocess.CompletedProcess([], 0)
            FOCUSED.main()
        self.assertEqual(run.call_args.args[0], ["pnpm", "--dir", "apps/site", "exec", "vitest", "run"])

    def test_pre_commit_delegates_to_the_workspace_shard(self):
        with patch.object(FOCUSED.sys, "argv", ["shepherd-focused-tests.py", "--pre-commit", "--dry-run"]), \
             patch.object(FOCUSED, "full_due", return_value=False), \
             patch.object(FOCUSED.SHARDS, "main") as main:
            FOCUSED.main()
        main.assert_called_once_with(["--pre-commit", "--dry-run"])

    def test_exact_retry_runs_only_the_named_test(self):
        with (
            patch.object(FOCUSED.sys, "argv", ["shepherd-focused-tests.py", "--package", "shepherd", "--test", "module::case"]),
            patch.object(FOCUSED, "full_due", return_value=False),
            patch.object(FOCUSED.SHARDS, "library_test_name", return_value="module::case"),
            patch.object(FOCUSED.subprocess, "run") as run,
        ):
            run.return_value = FOCUSED.subprocess.CompletedProcess([], 0)
            FOCUSED.main()
        self.assertEqual(run.call_args.args[0], ["cargo", "nextest", "run", "-p", "shepherd", "--lib", "--", "--exact", "module::case"])


if __name__ == "__main__":
    unittest.main()

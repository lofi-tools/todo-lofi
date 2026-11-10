import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("shards_cli", Path(__file__).with_name("test-shards.py"))
shards_cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shards_cli)


class RunnerTests(unittest.TestCase):
    def test_demo_only_web_library_suites_are_excluded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for directory, data in {
                "apps/app": {"name": "app"},
                "demos/example": {"name": "demo", "dependencies": {"demo-lib": "workspace:*"}},
                "libs/demo-lib": {"name": "demo-lib", "scripts": {"test": "vitest run"}},
            }.items():
                path = root / directory
                path.mkdir(parents=True)
                (path / "package.json").write_text(json.dumps(data))
                (path / "playwright.config.ts").write_text("")
            with patch.object(shards_cli, "ROOT", root):
                self.assertEqual(shards_cli.playwright_suites(), [root / "apps/app"])
                self.assertEqual(list(shards_cli.web_test_suites()), [])

    def test_real_git_change_selects_package_and_rotating_shard(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*arguments):
                subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *arguments],
                               cwd=root, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

            git("init", "-q")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.invalid")
            (root / "libs/core").mkdir(parents=True)
            (root / "libs/core/source.rs").write_text("before")
            git("add", ".")
            git("commit", "-qm", "initial")
            (root / "libs/core/source.rs").write_text("after")
            git("add", ".")
            git("commit", "-qm", "change")
            with patch.object(shards_cli, "ROOT", root), \
                 patch.object(shards_cli, "HISTORY", root / "history.jsonl"), \
                 patch.object(shards_cli, "workspace", return_value=({"libs/core": "core", "libs/other": "other"}, {})), \
                 patch.object(shards_cli, "playwright_suites", return_value=[]), \
                 patch.object(shards_cli, "web_test_suites", return_value=[]), \
                 patch.object(shards_cli, "execute") as execute:
                shards_cli.main(["--shard", "3/8", "--dry-run"])
            commands = [call.args[0] for call in execute.call_args_list]
            self.assertEqual(commands[0], ["cargo", "nextest", "run", "-p", "core"])
            self.assertIn("hash:3/8", commands[1])
            self.assertEqual(commands[1][4:6], ["--exclude", "core"])

    def test_changed_paths_include_commit_staged_unstaged_and_untracked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*arguments):
                subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *arguments], cwd=root,
                               check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

            git("init", "-q")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.invalid")
            (root / "deleted").write_text("before")
            (root / "modified").write_text("before")
            git("add", ".")
            git("commit", "-qm", "initial")
            (root / "committed").write_text("committed")
            git("add", "committed")
            git("commit", "-qm", "change")
            (root / "deleted").unlink()
            git("add", "-u")
            (root / "modified").write_text("after")
            (root / "untracked").write_text("new")
            with patch.object(shards_cli, "ROOT", root):
                self.assertEqual(set(shards_cli.changed_paths()), {"committed", "deleted", "modified", "untracked"})

    def test_explicit_full_includes_doctests_and_ui(self):
        with patch.object(shards_cli, "output", return_value="a"), \
             patch.object(shards_cli, "workspace", return_value=({}, {})), \
             patch.object(shards_cli, "playwright_suites", return_value=[shards_cli.UI]), \
             patch.object(shards_cli, "web_test_suites", return_value=[(shards_cli.ROOT, "vitest run")] * 2), \
             patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--all"])
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands, [["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_test_shard*.py"],
                                    ["cargo", "nextest", "run", "--workspace", "--no-fail-fast"],
                                    ["cargo", "test", "--workspace", "--doc"],
                                    ["pnpm", "exec", "vitest", "run"],
                                    ["pnpm", "exec", "vitest", "run"],
                                    ["pnpm", "exec", "playwright", "test"]])

    def test_affected_packages_and_remaining_shard(self):
        def output(*command):
            return "12" if command[:3] == ("git", "rev-list", "--first-parent") else "a"
        with patch.object(shards_cli, "output", side_effect=output), \
             patch.object(shards_cli, "first_parent_ancestors", return_value=[]), \
             patch.object(shards_cli, "playwright_suites", return_value=[shards_cli.UI]), \
             patch.object(shards_cli, "web_test_suites", return_value=[(shards_cli.ROOT, "vitest run")] * 2), \
             patch.object(shards_cli, "workspace", return_value=({"libs/core": "core", "libs/other": "other"}, {})), \
             patch.object(shards_cli, "changed_paths", return_value=["libs/core/src/lib.rs"]), \
             patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--shard", "3/8"])
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands[0], ["cargo", "nextest", "run", "-p", "core"])
        self.assertIn("--exclude", commands[1])
        self.assertIn("hash:3/8", commands[1])
        self.assertIn("--passWithNoTests", commands[2])
        self.assertEqual(commands[2][-2:], ["--shard", "3/8"])
        self.assertIn("--pass-with-no-tests", commands[-1])
        self.assertEqual(commands[-1][-2:], ["--shard", "3/8"])

    def test_demo_and_prototype_changes_are_excluded(self):
        packages = {"demos/example": "demo", "libs/dson_freeform_task": "dson_freeform_task",
                    "libs/core": "core"}
        with patch.object(shards_cli, "output", return_value="a"), \
             patch.object(shards_cli, "first_parent_ancestors", return_value=[]), \
             patch.object(shards_cli, "workspace", return_value=(packages, {})), \
             patch.object(shards_cli, "changed_paths", return_value=["demos/example/README.md", "libs/dson_freeform_task/src/lib.rs"]), \
             patch.object(shards_cli, "playwright_suites", return_value=[]), \
             patch.object(shards_cli, "web_test_suites", return_value=[]), \
             patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--shard", "3/8"])
        command = execute.call_args.args[0]
        self.assertIn("hash:3/8", command)
        self.assertIn("demo", command)
        self.assertIn("dson_freeform_task", command)
        self.assertNotIn("-p", command)

    def test_affected_web_packages_run_in_full(self):
        web_roots = {"libs/design": "design", "apps/site": "site"}
        with patch.object(shards_cli, "output", return_value="a"), \
             patch.object(shards_cli, "first_parent_ancestors", return_value=[]), \
             patch.object(shards_cli, "workspace", return_value=({"libs/core": "core"}, {})), \
             patch.object(shards_cli, "web_workspace", return_value=(web_roots, {"design": {"site"}}, [])), \
             patch.object(shards_cli, "changed_paths", return_value=["libs/design/src/button.ts"]), \
             patch.object(shards_cli, "playwright_suites", return_value=[shards_cli.ROOT / "apps/site"]), \
             patch.object(shards_cli, "web_test_suites", return_value=[(shards_cli.ROOT / "libs/design", "vitest run")]), \
             patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--shard", "3/8"])
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertIn("hash:3/8", commands[0])
        self.assertEqual(commands[1], ["pnpm", "exec", "vitest", "run"])
        self.assertEqual(commands[2], ["pnpm", "exec", "playwright", "test"])

    def test_full_run_excludes_demo_and_prototype_packages(self):
        roots = {"demos/example": "demo", "libs/dson_freeform_task": "dson_freeform_task", "libs/core": "core"}
        with patch.object(shards_cli, "output", return_value="a"), \
             patch.object(shards_cli, "workspace", return_value=(roots, {})), \
             patch.object(shards_cli, "playwright_suites", return_value=[]), \
             patch.object(shards_cli, "web_test_suites", return_value=[]), \
             patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--all"])
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands[1], ["cargo", "nextest", "run", "--workspace", "--exclude", "demo",
                                       "--exclude", "dson_freeform_task", "--no-fail-fast"])
        self.assertEqual(commands[2], ["cargo", "test", "--workspace", "--exclude", "demo",
                                       "--exclude", "dson_freeform_task", "--doc"])

    def test_invalid_shard_rejected(self):
        with self.assertRaises(Exception):
            shards_cli.parse_shard("9/8")

    def test_exact_test_can_be_scoped_to_a_package(self):
        with patch.object(shards_cli, "output", return_value="a"), patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--package", "core", "--test", "module::test"])
        self.assertEqual(execute.call_args.args[0],
                         ["cargo", "nextest", "run", "--package", "core", "--", "--exact", "module::test"])


if __name__ == "__main__":
    unittest.main()

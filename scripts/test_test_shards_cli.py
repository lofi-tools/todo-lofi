import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("shards_cli", Path(__file__).with_name("test-shards.py"))
shards_cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shards_cli)


class RunnerTests(unittest.TestCase):
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

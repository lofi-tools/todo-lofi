import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("shards_cli", Path(__file__).with_name("test-shards.py"))
shards_cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shards_cli)


class RunnerTests(unittest.TestCase):
    def test_explicit_full_includes_doctests_and_ui(self):
        with patch.object(shards_cli, "output", return_value="a"), \
             patch.object(shards_cli, "playwright_suites", return_value=[shards_cli.UI]), \
             patch.object(shards_cli, "web_test_suites", return_value=[(shards_cli.ROOT, "vitest run")] * 2), \
             patch.object(shards_cli, "execute") as execute:
            shards_cli.main(["--all"])
        commands = [call.args[0] for call in execute.call_args_list]
        self.assertEqual(commands, [["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_test_shard*.py"],
                                    ["cargo", "nextest", "run", "--workspace"],
                                    ["cargo", "test", "--workspace", "--doc"],
                                    ["pnpm", "exec", "vitest", "run", "--passWithNoTests"],
                                    ["pnpm", "exec", "vitest", "run", "--passWithNoTests"],
                                    ["pnpm", "exec", "playwright", "test", "--pass-with-no-tests"]])

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
        self.assertEqual(commands[2][-2:], ["--shard", "3/8"])
        self.assertEqual(commands[-1][-2:], ["--shard", "3/8"])

    def test_invalid_shard_rejected(self):
        with self.assertRaises(Exception):
            shards_cli.parse_shard("9/8")


if __name__ == "__main__":
    unittest.main()

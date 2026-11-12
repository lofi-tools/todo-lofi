import importlib.util
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location("python_unit_shard", Path(__file__).with_name("python-unit-shard.py"))
SHARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SHARD)


class PythonShardTests(unittest.TestCase):
    def test_nested_suites_are_flattened_and_each_case_has_one_stable_shard(self):
        first = unittest.FunctionTestCase(lambda: None, description="first")
        second = unittest.FunctionTestCase(lambda: None, description="second")
        suite = unittest.TestSuite([first, unittest.TestSuite([second])])
        self.assertEqual(list(SHARD.cases(suite)), [first, second])
        for name in ["module.Test.first", "module.Test.second"]:
            selected = [index for index in range(1, 9) if SHARD.shard_for_test(name, 8) == index]
            self.assertEqual(len(selected), 1)
            self.assertEqual(selected[0], SHARD.shard_for_test(name, 8))


if __name__ == "__main__":
    unittest.main()

import unittest

from test_sharding import affected_tests, excluded_packages, shard_for_revision


class SelectionTests(unittest.TestCase):
    def test_excludes_demo_closure_but_keeps_shared_and_non_demo_dependencies(self):
        packages = {"demos/example": "demo", "apps/app": "app", "libs/demo": "demo-lib",
                    "libs/shared": "shared", "libs/consumer": "consumer",
                    "libs/independent": "independent", "libs/dson_freeform_task": "dson_freeform_task"}
        dependents = {"demo-lib": {"demo"}, "shared": {"demo", "app"},
                      "independent": {"demo", "consumer"}}
        self.assertEqual(excluded_packages(packages, dependents),
                         {"demo", "demo-lib", "dson_freeform_task"})

    def test_modulo_selection_is_one_based_and_reproducible(self):
        self.assertEqual(shard_for_revision("a", 8), 3)
        self.assertEqual(shard_for_revision("a", 8), shard_for_revision("a", 8))
        with self.assertRaises(ValueError):
            shard_for_revision("a", 0)

    def test_package_change_includes_transitive_dependents(self):
        selection = affected_tests(
            ["libs/core/src/core.rs"],
            {"libs/core": "core", "apps/shell": "shell", "apps/other": "other"},
            {"core": {"shell"}, "shell": {"other"}},
        )
        self.assertEqual(selection.packages, {"core", "shell", "other"})
        self.assertFalse(selection.ui)
        self.assertFalse(selection.all_tests)

    def test_web_change_includes_web_dependents_without_selecting_everything(self):
        selection = affected_tests(
            ["libs/design/src/button.ts"], {"libs/core": "core"}, {},
            {"libs/design": "design", "apps/site": "site", "apps/other": "other"},
            {"design": {"site"}},
        )
        self.assertEqual(selection.web_packages, {"design", "site"})
        self.assertEqual(selection.packages, set())
        self.assertFalse(selection.all_tests)

    def test_most_specific_root_wins_between_rust_and_web(self):
        selection = affected_tests(
            ["apps/shepherd/src-tauri/src/lib.rs", "apps/shepherd/ui/src/App.tsx"],
            {"apps/shepherd/src-tauri": "shepherd"}, {},
            {"apps/shepherd": "shell", "apps/shepherd/ui": "ui"}, {},
        )
        self.assertEqual(selection.packages, {"shepherd"})
        self.assertEqual(selection.web_packages, {"ui"})

    def test_unknown_path_still_selects_the_complete_suite(self):
        selection = affected_tests(["shared-config.toml"], {"libs/core": "core"}, {},
                                   {"apps/site": "site"}, {})
        self.assertTrue(selection.all_tests)
        self.assertEqual(selection.packages, {"core"})
        self.assertEqual(selection.web_packages, {"site"})

    def test_ui_and_deleted_paths_are_mapped(self):
        selection = affected_tests(
            ["apps/shepherd/ui/src/deleted.ts", "libs/core/deleted.rs"],
            {"libs/core": "core"}, {},
        )
        self.assertEqual(selection.packages, {"core"})
        self.assertTrue(selection.ui)
        self.assertFalse(selection.all_tests)

    def test_unknown_path_requires_complete_suite(self):
        selection = affected_tests(["Cargo.lock"], {"libs/core": "core"}, {})
        self.assertTrue(selection.all_tests)
        self.assertTrue(selection.ui)
        self.assertEqual(selection.packages, {"core"})


if __name__ == "__main__":
    unittest.main()

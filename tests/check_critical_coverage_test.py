import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "check-critical-coverage.py"
SPEC = importlib.util.spec_from_file_location("check_critical_coverage", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class ModuleCoverageTests(unittest.TestCase):
    def test_split_module_is_measured_with_its_submodules_but_not_tests(self):
        coverage = {
            "src/core/message_verification.rs": (5, 10, 1, 2),
            "src/core/message_verification/staged.rs": (90, 100, 30, 40),
            "src/core/message_verification/tests.rs": (1000, 1000, 0, 0),
            "src/core/message_verification_other.rs": (0, 50, 0, 9),
        }
        self.assertEqual(
            MODULE.module_coverage(coverage, "src/core/message_verification.rs"),
            (95, 110, 31, 42),
        )

    def test_unsplit_module_and_missing_module(self):
        coverage = {"src/process.rs": (8, 10, 3, 4)}
        self.assertEqual(MODULE.module_coverage(coverage, "src/process.rs"), (8, 10, 3, 4))
        self.assertIsNone(MODULE.module_coverage(coverage, "src/missing.rs"))


if __name__ == "__main__":
    unittest.main()

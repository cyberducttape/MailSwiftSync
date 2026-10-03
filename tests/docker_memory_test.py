import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "docker_memory.py"
SPEC = importlib.util.spec_from_file_location("docker_memory", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class DockerMemoryTests(unittest.TestCase):
    def test_parses_docker_stats_memory_and_limit_column(self):
        self.assertEqual(MODULE.parse_memory_bytes("18MiB / 7.76GiB"), 18 * 1024**2)
        self.assertEqual(MODULE.parse_memory_bytes("1.5GB / 8GB"), 1_500_000_000)
        self.assertEqual(MODULE.parse_memory_bytes("128B"), 128)

    def test_summarizes_sampled_peak_and_count(self):
        with tempfile.TemporaryDirectory() as directory:
            samples = Path(directory) / "memory.txt"
            samples.write_text("1MiB / 8GiB\n2MiB / 8GiB\n1.5MiB / 8GiB\n", encoding="utf-8")
            self.assertEqual(MODULE.peak_memory_bytes(samples), (2 * 1024**2, 3))

    def test_rejects_missing_or_invalid_samples(self):
        with self.assertRaisesRegex(ValueError, "invalid Docker memory"):
            MODULE.parse_memory_bytes("not memory")
        with tempfile.TemporaryDirectory() as directory:
            samples = Path(directory) / "empty.txt"
            samples.write_text("\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "no memory samples"):
                MODULE.peak_memory_bytes(samples)


if __name__ == "__main__":
    unittest.main()

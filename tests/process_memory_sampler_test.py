import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "process-memory-sampler.py"
SPEC = importlib.util.spec_from_file_location("process_memory_sampler", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class ProcessMemorySamplerTests(unittest.TestCase):
    def test_sums_rss_per_process_name_and_skips_unreadable_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = Path(directory)
            for pid, name, rss in [("10", "dovecot", 1000), ("11", "dovecot", 500), ("12", "imapsync", 2048)]:
                (proc / pid).mkdir()
                (proc / pid / "status").write_text(f"Name:\t{name}\nVmRSS:\t   {rss} kB\n", encoding="utf-8")
            (proc / "13").mkdir()  # exited before its status could be read
            (proc / "self").mkdir()
            self.assertEqual(
                MODULE.process_rss_by_name(proc),
                {"dovecot": 1500 * 1024, "imapsync": 2048 * 1024},
            )

    def test_reads_cgroup_anon_and_file_memory(self):
        with tempfile.TemporaryDirectory() as directory:
            stat = Path(directory) / "memory.stat"
            stat.write_text("anon 4096\nfile 8192\nkernel 12\n", encoding="utf-8")
            self.assertEqual(MODULE.cgroup_memory(stat), {"anon": 4096, "file": 8192})
            self.assertEqual(MODULE.cgroup_memory(Path(directory) / "missing"), {})

    def test_summary_orders_peaks_and_sanitizes_names(self):
        text = MODULE.render({"mailswiftsync": 10, "bad name/x": 20}, {"anon": 5, "file": 7}, 3)
        self.assertEqual(
            text.splitlines(),
            [
                "memory_samples=3",
                "peak_cgroup_anon_bytes=5",
                "peak_cgroup_file_bytes=7",
                "peak_rss_bytes[bad_name_x]=20",
                "peak_rss_bytes[mailswiftsync]=10",
            ],
        )


if __name__ == "__main__":
    unittest.main()

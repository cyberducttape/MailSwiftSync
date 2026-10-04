#!/usr/bin/env python3
"""Tests for the bounded byte-volume Maildir scale fixture generator."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "generate-scale-maildir.py"
SPEC = importlib.util.spec_from_file_location("generate_scale_maildir", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class ScaleMaildirTests(unittest.TestCase):
    def test_distributes_exact_body_bytes_and_writes_rfc822_identities(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            MODULE.generate_maildir(root, 3, 8)
            files = sorted(root.glob("scale-*.eml"))
            self.assertEqual(len(files), 3)
            bodies = [path.read_bytes().split(b"\r\n\r\n", 1)[1][:-2] for path in files]
            self.assertEqual([len(body) for body in bodies], [3, 3, 2])
            self.assertEqual(sum(map(len, bodies)), 8)
            self.assertIn(b"Message-ID: <mailswiftsync-scale-000001@example.test>", files[0].read_bytes())
            self.assertTrue(all(body == b"x" * len(body) for body in bodies))

    def test_refuses_out_of_range_and_overwriting_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, "message count"):
                MODULE.generate_maildir(root, 0, 0)
            with self.assertRaisesRegex(ValueError, "2 GiB"):
                MODULE.generate_maildir(root, 1, MODULE.MAX_TOTAL_BODY_BYTES + 1)
            (root / "scale-000001.eml").write_bytes(b"existing")
            with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
                MODULE.generate_maildir(root, 1, 1)


if __name__ == "__main__":
    unittest.main()

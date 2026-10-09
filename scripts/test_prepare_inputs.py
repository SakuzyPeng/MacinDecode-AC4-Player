"""Cached decoder inputs must survive reuse and invalidate on stale content."""
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from prepare_inputs import SPEC_TABLES, prepare_spec_tables


class SpecCacheTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.core = Path(temporary.name)
        (self.core / ".git").mkdir()
        self.payloads = {name: f"pinned fixture: {name}".encode() for name in SPEC_TABLES}

    def generate(self, args):
        if str(args[-1]).endswith("generate_spec_tables.py"):
            for name, payload in self.payloads.items():
                path = self.core / "spec" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(payload)

    def prepare(self, revision="first"):
        with patch("prepare_inputs.run", side_effect=self.generate) as run:
            prepare_spec_tables(self.core, revision)
        return run

    def test_warm_cache_does_not_install_tools_or_rewrite_tables(self):
        self.prepare()
        before = {name: (self.core / "spec" / name).stat().st_mtime_ns for name in SPEC_TABLES}
        self.prepare().assert_not_called()
        self.assertEqual(before, {
            name: (self.core / "spec" / name).stat().st_mtime_ns for name in SPEC_TABLES
        })

    def test_new_revision_rebuilds_even_when_old_tables_are_intact(self):
        self.prepare()
        self.assertTrue(self.prepare("second").called)
        self.prepare("second").assert_not_called()

    def test_missing_or_corrupt_tables_are_not_reused(self):
        for name in SPEC_TABLES:
            for missing in (True, False):
                with self.subTest(name=name, missing=missing):
                    self.prepare()
                    path = self.core / "spec" / name
                    if missing:
                        path.unlink()
                    else:
                        path.write_bytes(b"corrupt")
                    self.assertTrue(self.prepare().called)
                    self.assertEqual(path.read_bytes(), self.payloads[name])

    def test_broken_stamp_rebuilds(self):
        self.prepare()
        (self.core / ".git/player-spec-inputs.json").write_text("{", encoding="utf-8")
        self.assertTrue(self.prepare().called)

    def test_failed_generation_cannot_leave_a_valid_stamp(self):
        self.prepare()
        with patch("prepare_inputs.run", side_effect=subprocess.CalledProcessError(1, "generate")):
            with self.assertRaises(subprocess.CalledProcessError):
                prepare_spec_tables(self.core, "second")
        self.assertFalse((self.core / ".git/player-spec-inputs.json").exists())
        self.assertTrue(self.prepare("second").called)


if __name__ == "__main__":
    unittest.main()

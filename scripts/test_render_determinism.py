"""Keep the conditions MacinRender's cross-platform bit equality rests on.

ADM Core gates its PCM as bit-identical on macOS arm64, Linux x64 and Windows
x64, and the player links that Rust code through Cargo. Cargo unifies features
across the whole graph, so one unrelated dependency asking for rustfft's
default features would quietly restore the run-time SIMD/FMA dispatch Core
removed (ADR 0015). Check the graph the installers are built from, and that
the three places pinning Core name one commit.
"""

import os
from pathlib import Path
import re
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ("x86_64-pc-windows-msvc", "aarch64-apple-darwin")
SCALAR_ONLY = ("rustfft", "realfft", "rubato")
CORE = "MacinRender-ADM-Core.git?rev="


def core_pin():
    text = (ROOT / "crates/macinrender/native/CMakeLists.txt").read_text(encoding="utf-8")
    return re.search(r"GIT_TAG\s+([0-9a-f]{40})", text).group(1)


def resolved(target):
    command = ["cargo", "tree", "--locked", "--offline", "--target", target, "-e", "normal",
               "--prefix", "none", "--format", "{p} [{f}]"]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode:
        if os.environ.get("GITHUB_ACTIONS") == "true":
            raise AssertionError(result.stderr)
        raise unittest.SkipTest("dependencies are not fetched: " + result.stderr.strip()[-200:])
    packages = {}
    for line in result.stdout.splitlines():
        match = re.match(r"(\S+) v\S+(?: \((\S+)\))?.*\[(.*)\]", line)
        if match:
            name, source, features = match.groups()
            packages.setdefault(name, set()).add((source or "", features))
    return packages


@unittest.skipUnless(shutil.which("cargo"), "Requires cargo")
class RenderDeterminismTests(unittest.TestCase):
    def test_renderer_dsp_keeps_core_scalar_and_portable_math(self):
        pin = core_pin()
        for target in TARGETS:
            packages = resolved(target)
            for name in SCALAR_ONLY:
                with self.subTest(target=target, package=name):
                    self.assertIn(name, packages)
                    for source, features in packages[name]:
                        self.assertEqual(features, "", f"{name} must not enable SIMD features")
                        if name != "realfft":
                            self.assertIn(CORE + pin, source, f"{name} must be Core's patched copy")
            with self.subTest(target=target, package="nalgebra"):
                self.assertTrue(all("libm-force" in features.split(",")
                                    for _, features in packages["nalgebra"]))
            with self.subTest(target=target, package="mradm-ffi"):
                self.assertTrue(all(CORE + pin in source for source, _ in packages["mradm-ffi"]))

    def test_release_flags_do_not_target_the_build_machine(self):
        # target-cpu=native or +fma would let LLVM contract and vectorise by host.
        config = (ROOT / ".cargo/config.toml").read_text(encoding="utf-8")
        self.assertNotRegex(config, r"target-cpu|\+fma|\+avx")


if __name__ == "__main__":
    unittest.main()

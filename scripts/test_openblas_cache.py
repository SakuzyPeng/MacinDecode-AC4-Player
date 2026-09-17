import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import prepare_openblas as sdk


class OpenBlasCacheTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.probe = self.root / "scripts/native/openblas_probe.c"
        self.probe.parent.mkdir(parents=True)
        self.probe.write_bytes(b"numerical probe fixture")
        self.compiler = {"llvm": sdk.LLVM_VERSION, "msvc": "14.44.35207", "windows_sdk": "10.0.26100.0"}
        toolchain = patch.object(sdk, "toolchain", return_value=({}, self.compiler))
        toolchain.start()
        self.addCleanup(toolchain.stop)

    def seed_sdk(self):
        spec = sdk.build_spec(self.compiler)
        install = self.root / sdk.cache_info(self.root)["path"]
        library = install / "lib/openblas.lib"
        library.parent.mkdir(parents=True, exist_ok=True)
        library.write_bytes(b"previously verified static library")
        headers = install / "include/openblas"
        headers.mkdir(parents=True, exist_ok=True)
        for name in ("cblas.h", "lapacke.h", "openblas_config.h"):
            (headers / name).write_bytes(b"header fixture")
        manifest = dict(spec, library_sha256=sdk.digest(library),
                        probe_sha256=sdk.digest(self.probe), numerical_probe={"ok": True})
        (install / "build.json").write_text(json.dumps(manifest))
        return install, spec, manifest

    def test_renderer_and_cargo_changes_do_not_invalidate_the_sdk(self):
        renderer = self.root / "crates/macinrender/native/CMakeLists.txt"
        renderer.parent.mkdir(parents=True)
        renderer.write_text("GIT_TAG old-renderer")
        cargo = self.root / "Cargo.lock"
        cargo.write_text("old Rust dependencies")
        before = sdk.cache_info(self.root)
        renderer.write_text("GIT_TAG new-renderer")
        cargo.write_text("new Rust dependencies")
        self.assertEqual(sdk.cache_info(self.root), before)

    def test_sources_compilers_sdk_and_build_options_invalidate_the_cache(self):
        before = sdk.cache_info(self.root)
        for field in ("VERSION", "SOURCE_SHA", "LLVM_SHA", "CMAKE_VERSION"):
            with self.subTest(field=field), patch.object(sdk, field, "changed"):
                after = sdk.cache_info(self.root)
                self.assertNotEqual(after["key"], before["key"])
                self.assertNotEqual(after["path"], before["path"])
        for field in self.compiler:
            with self.subTest(compiler=field), patch.dict(self.compiler, {field: "changed"}):
                self.assertNotEqual(sdk.cache_info(self.root)["key"], before["key"])
        with patch.dict(sdk.OPTIONS, {"MSVC_STATIC_CRT": "OFF"}):
            self.assertNotEqual(sdk.cache_info(self.root)["key"], before["key"])

    def test_new_probe_requires_validation_again(self):
        install, spec, _ = self.seed_sdk()
        before = sdk.cache_info(self.root)
        self.probe.write_bytes(b"additional numerical ABI check")
        after = sdk.cache_info(self.root)
        self.assertNotEqual(after["key"], before["key"])
        self.assertEqual(after["path"], before["path"])
        self.assertFalse(sdk.valid_sdk(install, spec, sdk.digest(self.probe)))

    def test_sdk_restored_without_llvm_or_sources_skips_all_downloads_and_builds(self):
        install, _, _ = self.seed_sdk()
        self.assertFalse((self.root / ".ci-tools").exists())
        download = Mock(side_effect=AssertionError("a valid SDK must not download build inputs"))
        with patch.object(sdk, "run", side_effect=AssertionError("a valid SDK must not run a compiler")):
            result = sdk.prepare(self.root, download)
        download.assert_not_called()
        self.assertEqual(result["OPENBLAS_LIBRARY"], str(install / "lib/openblas.lib"))
        self.assertEqual(result["OPENBLAS_BUILD_MANIFEST"], str(install / "build.json"))
        self.assertFalse((self.root / ".ci-tools").exists())

    def test_incomplete_corrupt_or_unverified_sdks_are_rejected(self):
        for failure in ("library", "header", "compiler", "probe", "missing_manifest", "invalid_json", "wrong_json_type"):
            with self.subTest(failure=failure):
                install, spec, manifest = self.seed_sdk()
                if failure == "library":
                    (install / "lib/openblas.lib").write_bytes(b"damaged library")
                elif failure == "header":
                    (install / "include/openblas/lapacke.h").unlink()
                elif failure == "compiler":
                    manifest["toolchain"] = dict(self.compiler, msvc="incompatible compiler")
                    (install / "build.json").write_text(json.dumps(manifest))
                elif failure == "probe":
                    manifest["numerical_probe"] = {"ok": False}
                    (install / "build.json").write_text(json.dumps(manifest))
                elif failure == "missing_manifest":
                    (install / "build.json").unlink()
                elif failure == "invalid_json":
                    (install / "build.json").write_text("unfinished manifest")
                else:
                    (install / "build.json").write_text("[]")
                self.assertFalse(sdk.valid_sdk(install, spec, sdk.digest(self.probe)))


if __name__ == "__main__":
    unittest.main()

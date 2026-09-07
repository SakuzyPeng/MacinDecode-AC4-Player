import struct
import tempfile
import unittest
from pathlib import Path

from verify_runtime import clean_environment, pe_imports, verify_binary, verify_modules
from package import BINARY, build_msi, verify_app


def pe_fixture(direct="kernel32.dll", delayed=None):
    image = bytearray(1024)
    image[:2] = b"MZ"
    struct.pack_into("<I", image, 0x3C, 0x80)
    image[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HH", image, 0x84, 0x8664, 1)
    struct.pack_into("<H", image, 0x94, 240)
    optional = 0x98
    struct.pack_into("<H", image, optional, 0x20B)
    struct.pack_into("<H", image, optional + 68, 2)
    struct.pack_into("<I", image, optional + 120, 0x1000)
    section = optional + 240
    struct.pack_into("<IIII", image, section + 8, 512, 0x1000, 512, 512)
    struct.pack_into("<I", image, 0x20C, 0x1080)
    image[0x280:0x280 + len(direct) + 1] = direct.encode() + b"\0"
    if delayed:
        struct.pack_into("<I", image, optional + 112 + 13 * 8, 0x10A0)
        struct.pack_into("<II", image, 0x2A0, 1, 0x10E0)
        image[0x2E0:0x2E0 + len(delayed) + 1] = delayed.encode() + b"\0"
    return image


class RuntimeAuditTests(unittest.TestCase):
    def test_system_dxil_converter_is_distinct_from_the_dxc_runtime(self):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / "player.exe"
            system = Path(folder) / "System32"
            modules = {str(binary), str(system / "kernel32.dll"), str(system / "dxilconv.dll")}
            verify_modules(modules, binary, {"SYSTEMROOT":str(system)}, True)
            for name in ("dxil.dll", "dxcompiler.dll"):
                with self.subTest(name=name), self.assertRaisesRegex(RuntimeError, "non-system dependency"):
                    verify_modules(modules | {str(system / name)}, binary, {"SYSTEMROOT":str(system)}, True)

    def test_msi_authoring_rejects_extra_payload_before_invoking_wix(self):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / (BINARY + ".exe")
            binary.write_bytes(b"executable")
            (binary.parent / "libopenblas.dll").write_bytes(b"unwanted runtime")
            with self.assertRaisesRegex(RuntimeError, "only the executable"):
                build_msi(binary, "0.1.0", binary.parent / "player.msi")

    def test_app_payload_rejects_native_libraries(self):
        with tempfile.TemporaryDirectory() as folder:
            app = Path(folder) / "Player.app"
            for name in ["Contents/Info.plist", "Contents/MacOS/" + BINARY,
                         "Contents/Resources/app.icns", "Contents/_CodeSignature/CodeResources",
                         "Contents/Frameworks/libmradm_capi.dylib"]:
                path = app / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"fixture")
            with self.assertRaisesRegex(RuntimeError, "Unexpected app payload"):
                verify_app(app, "0.1.0")

    def test_windows_environment_preserves_system_root_and_removes_developer_paths(self):
        for spelling in ["SystemRoot", "SYSTEMROOT", "systemroot"]:
            env = clean_environment({spelling:r"C:\Windows", "Path":r"D:\developer-tools", "WGPU_BACKEND":"vulkan", "DYLD_LIBRARY_PATH":"/custom"}, True)
            self.assertEqual(env["SYSTEMROOT"], r"C:\Windows")
            self.assertEqual(env["PATH"], r"C:\Windows\System32;C:\Windows")
            self.assertNotIn("WGPU_BACKEND", env)
            self.assertNotIn("DYLD_LIBRARY_PATH", env)

    def check_image(self, image, operation):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / "player.exe"
            binary.write_bytes(image)
            return operation(binary)

    def test_accepts_system_imports(self):
        result = self.check_image(pe_fixture(), lambda binary: verify_binary(binary, "x86_64-pc-windows-msvc"))
        self.assertEqual(result["direct"], ["kernel32.dll"])

    def test_rejects_dynamic_crt_and_sqlite(self):
        for library in ["VCRUNTIME140.dll", "MSVCP140.dll", "sqlite3.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "mradm_capi.dll", "libopenblas.dll", "libgfortran-5.dll", "dxcompiler.dll"]:
            with self.subTest(library=library), self.assertRaisesRegex(RuntimeError, "Unexpected DLL"):
                self.check_image(pe_fixture(direct=library), lambda binary: verify_binary(binary, "x86_64-pc-windows-msvc"))

    def test_delay_imports_are_audited(self):
        image = pe_fixture(delayed="custom-renderer.dll")
        self.assertEqual(self.check_image(image, pe_imports)["delay"], ["custom-renderer.dll"])
        with self.assertRaisesRegex(RuntimeError, "custom-renderer"):
            self.check_image(image, lambda binary: verify_binary(binary, "x86_64-pc-windows-msvc"))

    def test_runtime_does_not_allow_adjacent_or_framework_libraries(self):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / "player.exe"
            system = Path(folder) / "System32"
            modules = {str(binary), str(system / "kernel32.dll"), str(Path(folder) / "libopenblas.dll")}
            with self.assertRaisesRegex(RuntimeError, "non-system dependency"):
                verify_modules(modules, binary, {"SYSTEMROOT":str(system)}, True)
            modules = {str(binary), "/usr/lib/libSystem.B.dylib", str(Path(folder) / "Contents/Frameworks/libmradm_capi.dylib")}
            with self.assertRaisesRegex(RuntimeError, "non-system library"):
                verify_modules(modules, binary, {}, False)

    def test_rejects_wrong_architecture_and_console_executable(self):
        for offset, value in [(0x84, 0xAA64), (0x98 + 68, 3)]:
            image = pe_fixture()
            struct.pack_into("<H", image, offset, value)
            with self.assertRaises(RuntimeError):
                self.check_image(image, pe_imports)


if __name__ == "__main__":
    unittest.main()

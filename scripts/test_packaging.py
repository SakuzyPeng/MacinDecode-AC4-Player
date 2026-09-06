import struct
import tempfile
import unittest
from pathlib import Path

from verify_runtime import pe_imports, verify_binary


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
    def check_image(self, image, operation):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder) / "player.exe"
            binary.write_bytes(image)
            return operation(binary)

    def test_accepts_system_imports(self):
        result = self.check_image(pe_fixture(), lambda binary: verify_binary(binary, "x86_64-pc-windows-msvc"))
        self.assertEqual(result["direct"], ["kernel32.dll"])

    def test_rejects_dynamic_crt_and_sqlite(self):
        for library in ["VCRUNTIME140.dll", "MSVCP140.dll", "sqlite3.dll", "api-ms-win-crt-runtime-l1-1-0.dll"]:
            with self.subTest(library=library), self.assertRaisesRegex(RuntimeError, "Unexpected DLL"):
                self.check_image(pe_fixture(direct=library), lambda binary: verify_binary(binary, "x86_64-pc-windows-msvc"))

    def test_delay_imports_are_audited(self):
        image = pe_fixture(delayed="custom-renderer.dll")
        self.assertEqual(self.check_image(image, pe_imports)["delay"], ["custom-renderer.dll"])
        with self.assertRaisesRegex(RuntimeError, "custom-renderer"):
            self.check_image(image, lambda binary: verify_binary(binary, "x86_64-pc-windows-msvc"))

    def test_rejects_wrong_architecture_and_console_executable(self):
        for offset, value in [(0x84, 0xAA64), (0x98 + 68, 3)]:
            image = pe_fixture()
            struct.pack_into("<H", image, offset, value)
            with self.assertRaises(RuntimeError):
                self.check_image(image, pe_imports)


if __name__ == "__main__":
    unittest.main()

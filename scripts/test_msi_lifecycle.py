"""Exercise rebuilt MSIs using a disposable product identity, never the player.

Runs on Windows with WiX, including in the normal packaging test job. The
unversioned fixture deliberately changes contents without changing X.Y.Z.
"""
import ctypes
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import uuid
import xml.etree.ElementTree as ET

import package


@unittest.skipUnless(os.name == "nt", "Requires Windows Installer")
class MsiLifecycleTests(unittest.TestCase):
    def test_rebuilt_preview_replaces_legacy_and_preserves_maintenance(self):
        import msilib

        fixture_id = str(uuid.uuid4())
        fixture_name = "MacinDecode MSI test " + fixture_id
        namespace = {"w": "http://wixtoolset.org/schemas/v4/wxs"}
        parent = package.ROOT / ".local-test-media/.tmp"
        parent.mkdir(parents=True, exist_ok=True)
        product_codes = []
        with tempfile.TemporaryDirectory(prefix="msi-lifecycle-", dir=parent) as folder:
            root = Path(folder)
            authoring = root / "packaging/windows"
            authoring.mkdir(parents=True)
            icons = root / "assets/icons"
            icons.mkdir(parents=True)
            shutil.copy2(package.ROOT / "assets/icons/app-windows.ico", icons)
            shutil.copy2(package.ROOT / "packaging/windows/ui.wxs", authoring)
            source = ET.parse(package.ROOT / "packaging/windows/player.wxs")
            product = source.find("w:Package", namespace)
            product.set("Name", fixture_name)
            for node in product.iter():
                if node.tag.endswith("Directory") and node.get("Id") == "INSTALLFOLDER":
                    node.set("Name", fixture_name)
                if node.tag.endswith("Shortcut"):
                    node.set("Name", fixture_name)
                if node.tag.endswith("RegistryValue"):
                    node.set("Key", "Software\\MacinDecode\\InstallerTests\\" + fixture_id)
            ET.register_namespace("", namespace["w"])
            source.write(authoring / "player.wxs", encoding="utf-8", xml_declaration=True)
            payload = root / "payload"
            payload.mkdir()
            binary = payload / (package.BINARY + ".exe")
            installed = root / "installed" / binary.name
            guid = lambda name: "{" + str(uuid.uuid5(uuid.UUID(fixture_id), name)).upper() + "}"

            def product_code(msi):
                database = msilib.OpenDatabase(str(msi), msilib.MSIDBOPEN_READONLY)
                code = package.msi_rows(database, "SELECT `Value` FROM `Property` WHERE `Property`='ProductCode'", 1)[0][0]
                product_codes.append(code)
                return code

            def state(code):
                return ctypes.windll.msi.MsiQueryProductStateW(ctypes.c_wchar_p(code))

            def invoke(msi, label, *extra):
                result = subprocess.run(["msiexec", "/i", str(msi), "/qn", "/norestart",
                                         "INSTALLFOLDER=" + str(installed.parent), "/l*v", str(root / (label + ".log")), *extra],
                                        capture_output=True, timeout=120)
                return result.returncode

            def success(msi, label, *extra):
                code = invoke(msi, label, *extra)
                self.assertIn(code, (0, 3010), (label, code))

            try:
                with patch.object(package, "ROOT", root), patch.object(package, "guid", guid):
                    # Reproduce the former version-derived ProductCode, then
                    # migrate from that identity using the real new authoring.
                    original_code = guid("product.0.1.1")
                    product.set("ProductCode", original_code)
                    source.write(authoring / "player.wxs", encoding="utf-8", xml_declaration=True)
                    binary.write_bytes(b"Unversioned player fixture: preview A")
                    legacy = root / "legacy.msi"
                    package.build_msi(binary, "0.1.1", legacy)
                    self.assertEqual(product_code(legacy), original_code)
                    success(legacy, "legacy-install")

                    # Even an unchanged executable in a freshly packed MSI
                    # used to fail (for example, after documentation changes).
                    conflicting = root / "conflicting.msi"
                    package.build_msi(binary, "0.1.1", conflicting)
                    self.assertEqual(product_code(conflicting), original_code)
                    self.assertEqual(invoke(conflicting, "legacy-conflict"), 1638)

                    binary.write_bytes(b"Unversioned player fixture: preview B")
                    product.set("ProductCode", "*")
                    source.write(authoring / "player.wxs", encoding="utf-8", xml_declaration=True)
                    replacement = root / "replacement.msi"
                    package.build_msi(binary, "0.1.1", replacement)
                    audit = root / "audit"
                    audit.mkdir()
                    extracted = package.verify_msi(replacement, audit, "0.1.1", {binary.name})
                    self.assertEqual(extracted.read_bytes(), binary.read_bytes())
                    replacement_code = product_code(replacement)
                    self.assertNotEqual(replacement_code, original_code)
                    success(replacement, "same-version-replacement")
                    self.assertEqual(state(original_code), -1)  # INSTALLSTATE_UNKNOWN
                    self.assertEqual(state(replacement_code), 5)  # INSTALLSTATE_DEFAULT
                    self.assertEqual(installed.read_bytes(), binary.read_bytes())
                    # /i on the identical package must remain valid maintenance.
                    success(replacement, "same-package-reopen")
                    installed.unlink()
                    success(replacement, "repair", "REINSTALL=ALL", "REINSTALLMODE=amus")
                    self.assertEqual(installed.read_bytes(), binary.read_bytes())

                    upgraded = root / "upgraded.msi"
                    package.build_msi(binary, "0.1.2", upgraded)
                    upgraded_code = product_code(upgraded)
                    success(upgraded, "upgrade")
                    self.assertEqual(state(replacement_code), -1)
                    self.assertEqual(state(upgraded_code), 5)
                    self.assertEqual(invoke(replacement, "downgrade"), 1603)
                    self.assertEqual(state(upgraded_code), 5)
                    success(upgraded, "uninstall", "REMOVE=ALL")
                    self.assertFalse(installed.exists())
                    self.assertEqual(state(upgraded_code), -1)
            except Exception:
                destination = package.ROOT / "target/packaging-failures" / ("msi-lifecycle-" + fixture_id)
                destination.mkdir(parents=True, exist_ok=True)
                for log in root.glob("*.log"):
                    shutil.copy2(log, destination)
                raise
            finally:
                for code in set(product_codes):
                    if state(code) == 5:
                        result = subprocess.run(["msiexec", "/x", code, "/qn", "/norestart"], timeout=120)
                        self.assertIn(result.returncode, (0, 3010), "Could not remove isolated MSI fixture")


if __name__ == "__main__":
    unittest.main()

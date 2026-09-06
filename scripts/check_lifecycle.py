"""Destructive installer lifecycle checks, restricted to fresh GitHub-hosted jobs.

The upgrade fixture reuses the tested executable with a higher installer/bundle
version. It tests installer replacement and data retention, not new application code.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import sqlite3
import subprocess

from package import APP_ID, APP_NAME, BINARY, ROOT, TARGETS, build_msi, build_pkg, diagnostic_workspace, make_app, sha256
from verify_runtime import require, run_smoke


def install(package, log, *, repair=False, uninstall=False, downgrade=False):
    if os.name == "nt":
        mode = "/x" if uninstall else ("/fa" if repair else "/i")
        arguments = ["msiexec", mode, str(package), "/qn", "/norestart", "/l*v", str(log)]
    else:
        arguments = ["installer", "-pkg", str(package), "-target", "CurrentUserHomeDirectory"]
    result = subprocess.run(arguments, capture_output=True, text=True, timeout=120)
    if os.name != "nt":
        log.write_text(result.stdout + result.stderr)
    if downgrade and os.name == "nt":
        require(result.returncode not in (0, 3010), "Windows allowed a downgrade")
    else:
        require(result.returncode in (0, 3010), f"Installer failed: {result.returncode}; see {log}\n{result.stderr}")


def check_state(data, expected_hash):
    require(sha256(data / "sofa/keep.sofa") == expected_hash, "Installer changed user SOFA")
    preferences = json.loads((data / "settings.json").read_text())["preferences"]
    require((preferences["volume"], preferences["muted"]) == (0.125, True), "Installer changed user settings")
    with sqlite3.connect(data / "library.sqlite3") as connection:
        require(connection.execute("SELECT name FROM playlists ORDER BY position LIMIT 1").fetchone()[0] == "Retained playlist", "Installer changed playlists")


def lifecycle(target):
    require(os.environ.get("GITHUB_ACTIONS") == "true", "Lifecycle installation is restricted to ephemeral CI runners")
    manifests = list((ROOT / "dist").glob(f"*-{target}.json"))
    require(len(manifests) == 1, "Expected one freshly built installer")
    manifest = json.loads(manifests[0].read_text())
    original = ROOT / "dist" / manifest["installer"]
    windows = os.name == "nt"
    location = Path(os.environ["LOCALAPPDATA"]) / "Programs" / APP_NAME if windows else Path.home() / "Applications" / (APP_NAME + ".app")
    data = Path(os.environ["APPDATA"]) / APP_ID / "data" if windows else Path.home() / "Library/Application Support" / APP_ID
    binary = location / (BINARY + ".exe") if windows else location / "Contents/MacOS" / BINARY
    require(not location.exists() and not data.exists(), "CI runner already contains application/user data; refusing to overwrite it")
    major, minor, patch = map(int, manifest["version"].split("."))
    require(patch < 65535, "No room for upgrade test version")
    upgrade_version = f"{major}.{minor}.{patch + 1}"
    with diagnostic_workspace("installer-lifecycle-") as work:
        installed_package = original
        try:
            install(original, work / "install.log")
            require(binary.is_file(), "Installer did not use the expected current-user location")
            run_smoke(binary, data)
            (data / "sofa/keep.sofa").write_bytes(b"User-owned SOFA lifecycle fixture")
            retained_hash = sha256(data / "sofa/keep.sofa")
            preferences_path = data / "settings.json"
            preferences = json.loads(preferences_path.read_text())
            preferences["preferences"].update(volume=0.125, muted=True)
            preferences_path.write_text(json.dumps(preferences))
            with sqlite3.connect(data / "library.sqlite3") as connection:
                connection.execute("UPDATE playlists SET name='Retained playlist'")
            payload_copy = work / "payload"
            payload_copy.mkdir()
            copy = payload_copy / binary.name
            shutil.copy2(binary, copy)
            if windows:
                for library in location.glob("*.dll"): shutil.copy2(library, payload_copy / library.name)
            else:
                shutil.copytree(location / "Contents/Frameworks", work / "frameworks")
            binary.unlink()
            install(original, work / "repair.log", repair=True)
            require(binary.is_file(), "Repair did not restore the executable")
            check_state(data, retained_hash)
            upgrade = work / ("upgrade.msi" if windows else "upgrade.pkg")
            if windows:
                build_msi(copy, upgrade_version, upgrade)
            else:
                app = make_app(copy, work / "upgrade-app", upgrade_version)
                shutil.copytree(work / "frameworks", app / "Contents/Frameworks")
                subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(app)], check=True)
                build_pkg(app, work, upgrade_version, upgrade)
            install(upgrade, work / "upgrade.log")
            installed_package = upgrade
            check_state(data, retained_hash)
            install(original, work / "downgrade.log", downgrade=True)
            if not windows:
                with (location / "Contents/Info.plist").open("rb") as file:
                    require(plistlib.load(file)["CFBundleVersion"] == upgrade_version, "macOS replaced a newer application")
            if windows:
                install(upgrade, work / "uninstall.log", uninstall=True)
            else:
                shutil.rmtree(location)
            require(not binary.exists(), "Uninstall left the executable")
            check_state(data, retained_hash)
            install(original, work / "reinstall.log")
            installed_package = original
            check_state(data, retained_hash)
            run_smoke(binary, data)
            check_state(data, retained_hash)
            print("Passed: per-user install, repair, upgrade, downgrade prevention, uninstall, reinstall, data retention")
        finally:
            if windows and binary.exists():
                install(installed_package, work / "cleanup.log", uninstall=True)
            elif not windows and location.exists():
                shutil.rmtree(location)
            if data.exists():
                shutil.rmtree(data)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    lifecycle(parser.parse_args().target)

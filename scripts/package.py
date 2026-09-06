#!/usr/bin/env python3
"""Build, inspect and smoke-test the exact installer payload (Python 3.11+)."""
import argparse
import importlib.util
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import tempfile
import uuid
import xml.etree.ElementTree as ET

from verify_runtime import require, run_smoke, verify_binary

ROOT = Path(__file__).resolve().parents[1]
APP_NAME = "MacinDecode AC-4 Player"
APP_ID = "com.macinrender.macindecode-ac4-player"
BINARY = "macindecode-ac4-player"
TARGETS = ("x86_64-pc-windows-msvc", "aarch64-apple-darwin")
ABOUT_VERSION = "0.9.2"
WIX_VERSION = "5.0.2"
MAC_MINIMUM = "14.0"
TOOLS = ROOT / "target/package-tools"


@contextmanager
def diagnostic_workspace(prefix):
    parent = ROOT / "target/package-work"
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as temporary:
        work = Path(temporary)
        try:
            yield work
        except Exception:
            destination = ROOT / "target/packaging-failures" / work.name
            destination.mkdir(parents=True, exist_ok=True)
            for pattern in ("*.log", "*report.json", "install-check.json"):
                for source in work.rglob(pattern):
                    target = destination / source.relative_to(work)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(source, target)
            print(f"Failure diagnostics: {destination}", flush=True)
            raise


def run(arguments, **kwargs):
    print("+", " ".join(map(str, arguments)), flush=True)
    return subprocess.run(list(map(str, arguments)), cwd=ROOT, check=True, **kwargs)


def output(arguments):
    return subprocess.check_output(list(map(str, arguments)), cwd=ROOT, text=True).strip()


def sha256(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def guid(name):
    return "{" + str(uuid.uuid5(uuid.NAMESPACE_DNS, APP_ID + "." + name)).upper() + "}"


def version_info(tag):
    metadata = json.loads(output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
    package = next(package for package in metadata["packages"] if package["name"] == BINARY)
    version = package["version"]
    require(re.fullmatch(r"\d+\.\d+\.\d+", version), "Installer versions must be numeric X.Y.Z")
    require(all(int(part) <= maximum for part, maximum in zip(version.split("."), (255, 255, 65535))), "Version exceeds MSI limits")
    commit = output(["git", "rev-parse", "HEAD"])
    dirty = bool(output(["git", "status", "--porcelain"]))
    if tag:
        require(tag == "v" + version, "Release tag must exactly match Cargo package version")
        require(not dirty, "Tagged installers require a clean checkout")
        require(output(["git", "rev-parse", f"refs/tags/{tag}^{{commit}}"] ) == commit, "Release tag must point to HEAD")
    artifact_version = version if tag else f"{version}-dev.{commit[:8]}" + (".dirty" if dirty else "")
    return metadata, version, artifact_version, commit, dirty


def cargo_about():
    executable = "cargo-about.exe" if os.name == "nt" else "cargo-about"
    candidate = os.environ.get("CARGO_ABOUT") or shutil.which(executable) or str(TOOLS / "bin" / executable)
    if not Path(candidate).is_file():
        run(["cargo", "install", "cargo-about", "--version", ABOUT_VERSION, "--locked", "--root", TOOLS,
             "--target-dir", ROOT / "target/package-tool-build"])
        candidate = str(TOOLS / "bin" / executable)
    require(output([candidate, "--version"]).split()[-1] == ABOUT_VERSION, "cargo-about version mismatch")
    return candidate


def license_report(destination):
    raw = destination.with_name("licenses-raw.json")
    run([cargo_about(), "about", "generate", "--locked", "--fail", "--format", "json", "--output-file", raw])
    data = json.loads(raw.read_text(encoding="utf-8"))
    cleaned = []
    for item in data["licenses"]:
        users = [{"crate": {key: used["crate"].get(key) for key in ("name", "version", "repository")}}
                 for used in item["used_by"]]
        text = item["text"]
        if item["id"] == "MPL-2.0":
            sources = [f"https://crates.io/api/v1/crates/{used['crate']['name']}/{used['crate']['version']}/download" for used in users]
            text = "Unmodified covered source is available at:\n" + "\n".join(sources) + "\n\n" + text
        cleaned.append({"id": item["id"], "name": item["name"], "text": text, "used_by": users})
    # cargo-about also emits absolute source paths and complete Cargo metadata.
    # Only these attribution fields belong in a distributed executable.
    require(cleaned and all(item["text"] and item["used_by"] for item in cleaned), "Incomplete license report")
    names = {used["crate"]["name"] for item in cleaned for used in item["used_by"]}
    require({"eframe", "epaint_default_fonts", "rusqlite", "macindecode-ac4-inspect"} <= names, "Missing dependency notices")
    cleaned.append({"id": "SQLite-public-domain", "name": "SQLite public domain dedication",
                    "text": "SQLite is in the public domain.\nhttps://sqlite.org/copyright.html\n\n"
                            "The author disclaims copyright to this source code. In place of a legal notice, here is a blessing:\n"
                            "May you do good and not evil.\nMay you find forgiveness for yourself and forgive others.\n"
                            "May you share freely, never taking more than you give.\n",
                    "used_by": [{"crate": {"name": "SQLite (embedded by libsqlite3-sys)", "version": "", "repository": "https://sqlite.org"}}]})
    native = Path(os.environ["MACINRENDER_SOURCE_DIR"])
    legal_files = [ROOT / "LICENSE", ROOT / "assets/fonts/OFL.txt", native / "LICENSE", native / "docs/THIRD_PARTY_LICENSES.md"]
    legal_files += sorted(path for path in (native / "third_party/licenses").rglob("*") if path.is_file())
    if os.name == "nt": legal_files.append(ROOT / "assets/licenses/OPENBLAS.txt")
    for index, path in enumerate(legal_files):
        cleaned.append({"id":f"native-{index}", "name":path.name, "text":path.read_text(encoding="utf-8", errors="replace"),
                        "used_by":[{"crate":{"name":"MacinDecode / MacinRender native dependencies", "version":"", "repository":"https://github.com/SakuzyPeng/MacinRender-ADM-Core"}}]})
    destination.write_text(json.dumps({"licenses": cleaned}, ensure_ascii=False, indent=2), encoding="utf-8")
    raw.unlink()


def make_app(binary, stage, version):
    app = stage / (APP_NAME + ".app")
    contents = app / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    (contents / "Resources").mkdir()
    executable = contents / "MacOS" / BINARY
    shutil.copy2(binary, executable)
    executable.chmod(0o755)
    shutil.copy2(ROOT / "assets/icons/app-macos.icns", contents / "Resources/app.icns")
    info = {"CFBundleIdentifier": APP_ID, "CFBundleName": APP_NAME, "CFBundleDisplayName": APP_NAME,
            "CFBundleExecutable": BINARY, "CFBundlePackageType": "APPL", "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleShortVersionString": version, "CFBundleVersion": version, "CFBundleIconFile": "app.icns",
            "LSMinimumSystemVersion": MAC_MINIMUM, "NSHighResolutionCapable": True,
            "NSMotionUsageDescription":"MacinDecode uses AirPods head orientation for spatial audio playback and its scene view."}
    with (contents / "Info.plist").open("wb") as destination:
        plistlib.dump(info, destination)
    run(["codesign", "--force", "--sign", "-", "--timestamp=none", app])
    run(["codesign", "--verify", "--strict", app])
    return app


def build_pkg(app, work, version, destination):
    components = [{"RootRelativeBundlePath": app.name, "BundleIsRelocatable": False,
                   "BundleIsVersionChecked": True, "BundleHasStrictIdentifier": True, "BundleOverwriteAction": "upgrade"}]
    component_plist = work / "components.plist"
    with component_plist.open("wb") as file:
        plistlib.dump(components, file)
    component = work / "player-component.pkg"
    run(["pkgbuild", "--root", app.parent, "--component-plist", component_plist,
         "--identifier", APP_ID + ".pkg", "--version", version, "--install-location", "/Applications", component])
    distribution = work / "Distribution.xml"
    distribution.write_text((ROOT / "packaging/macos/Distribution.xml").read_text().replace("@VERSION@", version))
    run(["productbuild", "--distribution", distribution, "--package-path", work, destination])


def verify_app(app, version):
    expected = {"Contents/Info.plist", "Contents/MacOS/" + BINARY, "Contents/Resources/app.icns", "Contents/_CodeSignature/CodeResources"}
    actual = {str(file.relative_to(app)) for file in app.rglob("*") if file.is_file()}
    expected |= {"Contents/Frameworks/libmradm_capi.dylib", "Contents/Frameworks/libmr_headtrack.dylib"}
    require(actual == expected, f"Unexpected app payload: {actual ^ expected}")
    require(not any(file.is_symlink() for file in app.rglob("*")), "Unexpected symlink in app")
    with (app / "Contents/Info.plist").open("rb") as source:
        info = plistlib.load(source)
    require(info["CFBundleIdentifier"] == APP_ID and info["CFBundleVersion"] == version, "Bundle identity/version mismatch")
    require(info["LSMinimumSystemVersion"] == MAC_MINIMUM and info.get("NSMotionUsageDescription"), "Bundle requirements mismatch")
    run(["codesign", "--verify", "--strict", app])


def verify_pkg(package, work, version):
    extracted = work / "expanded-pkg"
    run(["pkgutil", "--expand-full", package, extracted])
    distribution = ET.parse(extracted / "Distribution").getroot()
    domains = distribution.find("domains")
    require(domains is not None and domains.attrib == {"enable_anywhere": "false", "enable_currentUserHome": "true", "enable_localSystem": "false"}, "PKG is not exclusively per-user")
    require(distribution.find("options").get("hostArchitectures") == "arm64", "PKG architecture mismatch")
    payloads = list(extracted.rglob("Payload"))
    require(len(payloads) == 1, "Unexpected number of PKG payloads")
    app = payloads[0] / (APP_NAME + ".app")
    require(list(payloads[0].iterdir()) == [app], "PKG contains files outside the app")
    info = ET.parse(payloads[0].parent / "PackageInfo").getroot()
    require(info.get("install-location") == "/Applications" and info.get("version") == version, "PKG install location/version mismatch")
    require(info.find("scripts") is None, "PKG must not run install scripts")
    verify_app(app, version)
    domains_report = output(["installer", "-dominfo", "-pkg", package])
    require("CurrentUserHomeDirectory" in domains_report and "LocalSystem" not in domains_report, "Installer does not offer only the user domain")
    return app


def wix_tool():
    tool = TOOLS / "wix/wix.exe"
    if not tool.exists():
        run(["dotnet", "tool", "install", "wix", "--version", WIX_VERSION, "--tool-path", tool.parent])
    reported = output([tool, "--version"])
    require(reported == WIX_VERSION or reported.startswith((WIX_VERSION + ".", WIX_VERSION + "+")), "WiX version mismatch")
    return tool


def build_msi(binary, version, destination):
    namespace = "http://wixtoolset.org/schemas/v4/wxs"
    extra = ET.Element("Wix", xmlns=namespace)
    fragment = ET.SubElement(extra, "Fragment")
    group = ET.SubElement(fragment, "ComponentGroup", Id="NativeRuntime", Directory="INSTALLFOLDER")
    for index, library in enumerate(sorted(binary.parent.glob("*.dll"))):
        component = ET.SubElement(group, "Component", Id=f"Native{index}", Guid=guid("runtime." + library.name.lower()))
        ET.SubElement(component, "File", Id=f"NativeFile{index}", Source=str(library), Name=library.name)
        ET.SubElement(component, "RegistryValue", Root="HKCU", Key="Software\\MacinDecode\\AC4Player\\Installer", Name=library.name, Type="string", Value="[ProductVersion]", KeyPath="yes")
    harvest = destination.with_suffix(".runtime.wxs")
    ET.ElementTree(extra).write(harvest, encoding="utf-8", xml_declaration=True)
    run([wix_tool(), "build", ROOT / "packaging/windows/player.wxs", harvest, "-arch", "x64",
         "-d", f"Version={version}", "-d", f"ProductCode={guid('product.' + version)}",
         "-d", f"UpgradeCode={guid('upgrade')}", "-d", f"ComponentCode={guid('executable')}",
         "-d", f"Executable={binary}", "-d", f"Icon={ROOT / 'assets/icons/app-windows.ico'}", "-o", destination])


def msi_rows(database, query, columns):
    view = database.OpenView(query)
    view.Execute(None)
    rows = []
    while record := view.Fetch():
        rows.append(tuple(record.GetString(index + 1) for index in range(columns)))
    view.Close()
    return rows


def verify_msi(package, work, version, expected_names):
    # Python 3.12 is pinned in CI; msilib was removed in Python 3.13.
    import msilib
    database = msilib.OpenDatabase(str(package), msilib.MSIDBOPEN_READONLY)
    properties = dict(msi_rows(database, "SELECT `Property`, `Value` FROM `Property`", 2))
    require(properties.get("ALLUSERS", "") == "", "MSI must be per-user")
    require(properties["ProductVersion"] == version and properties["UpgradeCode"] == guid("upgrade"), "MSI identity mismatch")
    summary = database.GetSummaryInformation(0)
    require(summary.GetProperty(15) & 8, "MSI must support installation without elevation")
    template = summary.GetProperty(7)
    require((template.decode() if isinstance(template, bytes) else template).startswith("x64;"), "MSI architecture mismatch")
    files = msi_rows(database, "SELECT `FileName` FROM `File`", 1)
    require({file[0].split("|")[-1] for file in files} == expected_names, f"Unexpected MSI files: {files}")
    directories = {row[0]: (row[1], row[2]) for row in msi_rows(database, "SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`", 3)}
    require(directories["INSTALLFOLDER"][0] == "UserPrograms" and directories["UserPrograms"][0] == "LocalAppDataFolder", "MSI install path is not per-user Programs")
    require(all(row[0] == "1" for row in msi_rows(database, "SELECT `Root` FROM `Registry`", 1)), "MSI writes outside HKCU")
    require(not msi_rows(database, "SELECT `Name` FROM `_Tables` WHERE `Name`='CustomAction'", 1), "MSI must not execute custom actions")
    del summary, database
    extracted = work / "expanded-msi"
    extracted.mkdir()
    run(["msiexec", "/a", package, "/qn", f"TARGETDIR={extracted}", "REBOOT=ReallySuppress", "/l*v", work / "msi-extract.log"], timeout=120)
    executables = list(extracted.rglob(BINARY + ".exe"))
    require(len(executables) == 1, "MSI extraction did not yield one executable")
    return executables[0]


def build(target, tag):
    require((target.startswith("aarch64") and platform.system() == "Darwin" and platform.machine() == "arm64")
            or (target.startswith("x86_64") and os.name == "nt"), "Installers must be built on their native platform")
    from prepare_inputs import prepare
    prepare()
    metadata, version, artifact_version, commit, dirty = version_info(tag)
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    with diagnostic_workspace("package-") as work:
        notices = work / "licenses.json"
        license_report(notices)
        env = dict(os.environ, MACINDECODE_LICENSES_JSON=str(notices), MACOSX_DEPLOYMENT_TARGET=MAC_MINIMUM)
        run(["cargo", "build", "--locked", "--release", "--target", target], env=env)
        binary = Path(metadata["target_directory"]) / target / "release" / (BINARY + (".exe" if os.name == "nt" else ""))
        legacy_spec = importlib.util.spec_from_file_location("player_bundle", ROOT / "scripts/package-player.py")
        legacy = importlib.util.module_from_spec(legacy_spec)
        legacy_spec.loader.exec_module(legacy)
        native = legacy.cargo_native(Path(metadata["target_directory"]) / target)
        extension = ".msi" if os.name == "nt" else ".pkg"
        name = f"MacinDecode-AC4-Player-{artifact_version}-{target}"
        installer = work / (name + extension)
        relocated = work / "relocated application"
        relocated.mkdir()
        if os.name == "nt":
            payload = work / "windows-payload"
            payload.mkdir()
            shutil.copy2(binary, payload / binary.name)
            for library in native["binary"].glob("*.dll"): shutil.copy2(library, payload / library.name)
            dumpbin = shutil.which("dumpbin") or str(native["compiler"] / "dumpbin.exe")
            legacy.windows_dependencies(payload, native, dumpbin)
            expected_names = {file.name for file in payload.iterdir()}
            build_msi(payload / binary.name, version, installer)
            extracted = verify_msi(installer, work, version, expected_names)
            require(sha256(extracted) == sha256(binary), "MSI changed executable bytes")
            executable = relocated / binary.name
            shutil.copy2(extracted, executable)
            for library in (work / "expanded-msi").rglob("*.dll"):
                require(library.name in expected_names, "Unlisted runtime in MSI")
                require(sha256(library) == sha256(payload / library.name), "MSI changed runtime bytes")
                shutil.copy2(library, relocated / library.name)
            dependencies = {file.name:verify_binary(file, target, relocated) for file in relocated.iterdir()}
        else:
            app = make_app(binary, work / "app-root", version)
            frameworks = app / "Contents/Frameworks"
            frameworks.mkdir()
            for name in ("libmradm_capi.dylib", "libmr_headtrack.dylib"):
                shutil.copy2(native["binary"] / name, frameworks / name, follow_symlinks=True)
                run(["codesign", "--force", "--sign", "-", "--timestamp=none", frameworks / name])
            run(["codesign", "--force", "--sign", "-", "--timestamp=none", app])
            build_pkg(app, work, version, installer)
            extracted = verify_pkg(installer, work, version)
            require(sha256(extracted / "Contents/MacOS" / BINARY) == sha256(app / "Contents/MacOS" / BINARY), "PKG changed executable bytes")
            relocated_app = relocated / extracted.name
            shutil.copytree(extracted, relocated_app)
            verify_app(relocated_app, version)
            executable = relocated_app / "Contents/MacOS" / BINARY
            dependencies = {file.name:verify_binary(file, target, relocated_app) for file in [executable, *(relocated_app / "Contents/Frameworks").glob("*.dylib")]}
        runtime = run_smoke(executable, work / "isolated profile")
        # Local paths used during verification are not distributed in manifests.
        runtime.pop("loaded_modules", None)
        manifest = {"version": version, "artifact_version": artifact_version, "target": target,
                    "source_commit": commit, "working_tree_dirty": dirty, "installer": installer.name,
                    "installer_sha256": sha256(installer), "executable_sha256": sha256(executable),
                    "cargo_lock_sha256": sha256(ROOT / "Cargo.lock"), "rustc": output(["rustc", "--version"]),
                    "signing": "unsigned-msi" if os.name == "nt" else "ad-hoc-app/unsigned-pkg",
                    "minimum_os": "Windows 10 22H2" if os.name == "nt" else "macOS " + MAC_MINIMUM,
                    "native_commit":output(["git", "-C", native["source"], "rev-parse", "HEAD"]),
                    "dependencies": dependencies, "smoke_test": runtime}
        shutil.copy2(installer, dist / installer.name)
        (dist / (installer.name + ".sha256")).write_text(f"{manifest['installer_sha256']}  {installer.name}\n", encoding="utf-8")
        (dist / (name + ".json")).write_text(json.dumps(manifest, indent=2), encoding="utf-8")
        print(f"Verified installer: {dist / installer.name}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--release-tag", default="")
    args = parser.parse_args()
    build(args.target, args.release_tag)

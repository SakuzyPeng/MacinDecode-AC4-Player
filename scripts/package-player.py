#!/usr/bin/env python3
"""Package an already-built Release player and its Cargo-built native libraries."""
import argparse
import json
import os
import pathlib
import plistlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]


def cargo_native(target):
    candidates = []
    for path in (target / "release" / "build").glob("macindecode-macinrender-*/output"):
        values = {}
        for line in path.read_text().splitlines():
            if line.startswith("cargo:lib_dir="):
                values["binary"] = pathlib.Path(line.split("=", 1)[1])
            elif line.startswith("cargo:source_dir="):
                values["source"] = pathlib.Path(line.split("=", 1)[1])
            elif line.startswith("cargo:compiler_dir="):
                values["compiler"] = pathlib.Path(line.split("=", 1)[1])
        if values.get("binary", pathlib.Path("/missing")).is_dir() and "source" in values:
            candidates.append((path.stat().st_mtime, values))
    if not candidates:
        raise SystemExit("No native Release build found; run cargo build --release first")
    return max(candidates, key=lambda item: item[0])[1]


def windows_dependencies(package, native, dumpbin):
    """Stage the MSVC redistributable and check every packaged PE import."""
    redist = None
    compiler = native.get("compiler")
    if compiler:
        vc = next((path for path in compiler.parents if path.name.lower() == "vc"), None)
        if vc:
            redist = vc / "Redist" / "MSVC"
    pending = sorted(package.glob("*.exe")) + sorted(package.glob("*.dll"))
    checked = set()
    reports = []
    while pending:
        artifact = pending.pop(0)
        if artifact.name.lower() in checked:
            continue
        checked.add(artifact.name.lower())
        report = subprocess.check_output([dumpbin, "/dependents", artifact.name], cwd=package, text=True)
        reports.append(report)
        for name in re.findall(r"^\s+([\w.+-]+\.dll)\s*$", report, flags=re.MULTILINE | re.IGNORECASE):
            bundled = package / name
            if bundled.is_file():
                pending.append(bundled)
                continue
            # The Rust executable uses the dynamic CRT even when the native
            # renderer was built with /MT. Do not rely on this PC's installed CRT.
            if name.lower().startswith(("vcruntime", "msvcp", "concrt")):
                candidates = list(redist.glob(f"*/x64/Microsoft.VC*.CRT/{name}")) if redist else []
                if not candidates:
                    raise SystemExit(f"Cannot find the x64 MSVC redistributable for {name}")
                source = max(candidates, key=lambda path: path.stat().st_mtime)
                shutil.copy2(source, bundled)
                pending.append(bundled)
            elif not name.lower().startswith(("api-ms-", "ext-ms-")) and not (pathlib.Path(os.environ["SystemRoot"]) / "System32" / name).is_file():
                raise SystemExit(f"Unbundled dependency: {artifact.name} requires {name}")
    return "\n".join(reports)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=pathlib.Path, default=pathlib.Path(os.getenv("CARGO_TARGET_DIR", ROOT / "target")))
    parser.add_argument("--output", type=pathlib.Path, default=ROOT / "dist")
    args = parser.parse_args()
    native = cargo_native(args.target_dir)
    extra_build_info = {}
    core_revisions = set(re.findall(
        r'^source = "git\+https://github\.com/SakuzyPeng/MacinDecode-AC4-Core\.git[^\"]*#([0-9a-f]{40})"$',
        (ROOT / "Cargo.lock").read_text(), flags=re.MULTILINE))
    extra_build_info["ac4_core_commit"] = next(iter(core_revisions)) if len(core_revisions) == 1 else None
    args.output.mkdir(parents=True, exist_ok=True)
    if sys.platform == "darwin":
        package = args.output / "MacinDecode AC-4 Player.app"
        if package.exists():
            raise SystemExit(f"Output already exists: {package}; choose a fresh --output directory")
        contents = package / "Contents"
        executable_dir = contents / "MacOS"
        libraries = contents / "Frameworks"
        resources = contents / "Resources"
        legal = resources / "Legal"
        executable_dir.mkdir(parents=True)
        libraries.mkdir()
        resources.mkdir()
        shutil.copy2(ROOT / "assets/icons/app-macos.icns", resources / "AppIcon.icns")
        shutil.copy2(args.target_dir / "release/macindecode-ac4-player", executable_dir)
        for name in ["libmradm_capi.dylib", "libmr_headtrack.dylib"]:
            shutil.copy2(native["binary"] / name, libraries / name, follow_symlinks=True)
        info = dict(CFBundleExecutable="macindecode-ac4-player", CFBundleIdentifier="com.macinrender.macindecode-ac4-player",
                    CFBundleName="MacinDecode AC-4 Player", CFBundlePackageType="APPL", CFBundleVersion="1",
                    CFBundleShortVersionString="0.1.0", LSMinimumSystemVersion="14.0",
                    CFBundleIconFile="AppIcon.icns",
                    NSHighResolutionCapable=True,
                    NSMotionUsageDescription="MacinDecode uses AirPods head orientation for spatial audio playback and its scene view.")
        with (contents / "Info.plist").open("wb") as stream:
            plistlib.dump(info, stream)
        dependencies = subprocess.check_output(["otool", "-L", str(libraries / "libmradm_capi.dylib")], text=True)
        forbidden = [line for line in dependencies.splitlines()[2:] if "/opt/homebrew/" in line or "/build/" in line or "/target/" in line]
        if forbidden:
            raise SystemExit("Native library depends on build-machine files: " + "\n".join(forbidden))
    elif sys.platform == "win32":
        package = args.output / "MacinDecode-AC4-Player-windows-x64"
        if package.exists():
            raise SystemExit(f"Output already exists: {package}; choose a fresh --output directory")
        package.mkdir()
        libraries = package
        legal = package / "Legal"
        shutil.copy2(args.target_dir / "release/macindecode-ac4-player.exe", package)
        for path in native["binary"].glob("*.dll"):
            shutil.copy2(path, package)
        # OpenBLAS is the sole non-system runtime outside the C API bundle in the
        # canonical MSVC build; copy its runtime family from the supplied toolchain.
        openblas = os.getenv("OPENBLAS_LIBRARY")
        if openblas:
            directory = pathlib.Path(openblas).parent.parent / "bin"
            for path in directory.glob("*.dll"):
                shutil.copy2(path, package)
            header = directory.parent / "include" / "openblas_config.h"
            if header.is_file():
                version = re.search(r'#define\s+OPENBLAS_VERSION\s+"([^"]+)"', header.read_text())
                if version:
                    extra_build_info["openblas"] = version.group(1).strip()
        dumpbin = shutil.which("dumpbin")
        if not dumpbin and "compiler" in native:
            dumpbin = str(native["compiler"] / "dumpbin.exe")
        if not (package / "mradm_capi.dll").is_file():
            raise SystemExit("Native Release build did not provide mradm_capi.dll")
        dependencies = windows_dependencies(package, native, dumpbin or "dumpbin")
    else:
        raise SystemExit("Packaging is currently supported on macOS and Windows")
    legal.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "LICENSE", legal / "PLAYER_LICENSE")
    shutil.copy2(ROOT / "assets/fonts/OFL.txt", legal / "PLAYER_FONT_OFL.txt")
    if sys.platform == "win32":
        shutil.copy2(ROOT / "assets/licenses/OPENBLAS.txt", legal / "OPENBLAS_LICENSE")
    shutil.copy2(native["source"] / "LICENSE", legal / "MACINRENDER_LICENSE")
    shutil.copy2(native["source"] / "docs/THIRD_PARTY_LICENSES.md", legal / "THIRD_PARTY_NOTICES.md")
    shutil.copytree(native["source"] / "third_party/licenses", legal / "licenses", dirs_exist_ok=True)
    shutil.copy2(native["source"] / "third_party/sbom.cyclonedx.json", legal)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    native_commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=native["source"], text=True).strip()
    (legal / "BUILD_INFO.json").write_text(json.dumps(dict(player_commit=commit, macinrender_commit=native_commit,
        player_dirty=bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT)),
        macinrender_dirty=bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=native["source"])),
        c_abi="1.36", native_build="Release", sofa=True, iamf=False, **extra_build_info), indent=2) + "\n")
    (legal / "DEPENDENCIES.txt").write_text(dependencies)
    if sys.platform == "darwin":
        for path in libraries.glob("*.dylib"):
            subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(path)], check=True)
        subprocess.run(["codesign", "--force", "--deep", "--sign", "-", "--timestamp=none", str(package)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(package)], check=True)
    print(package)


if __name__ == "__main__":
    main()

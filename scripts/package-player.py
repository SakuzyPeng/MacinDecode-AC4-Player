#!/usr/bin/env python3
"""Build a standalone single-executable payload with embedded third-party notices."""
import argparse
import json
import os
import pathlib
import shutil
import sys
import tempfile

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
            elif line.startswith("cargo:linkage="):
                values["linkage"] = line.split("=", 1)[1]
        if values.get("binary", pathlib.Path("/missing")).is_dir() and "source" in values:
            candidates.append((path.stat().st_mtime, values))
    if not candidates:
        raise SystemExit("No native Release build found; run cargo build --release first")
    return max(candidates, key=lambda item: item[0])[1]


def main():
    from package import (BINARY, ROOT, TOOLS, MAC_MINIMUM, license_report, make_app,
                         output, run, sha256, verify_app)
    from prepare_inputs import prepare
    from verify_runtime import require, run_smoke, verify_binary

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=pathlib.Path, default=pathlib.Path(os.getenv("CARGO_TARGET_DIR", ROOT / "target")))
    parser.add_argument("--output", type=pathlib.Path, default=ROOT / "dist")
    args = parser.parse_args()
    args.target_dir = args.target_dir.resolve()
    prepare()
    notices = TOOLS / "licenses/licenses.json"
    notices.parent.mkdir(parents=True, exist_ok=True)
    license_report(notices)
    # The standalone assembly entrypoint guarantees the same embedded notices
    # as the installer entrypoint, including after a plain development build.
    env = dict(os.environ, MACINDECODE_LICENSES_JSON=str(notices), MACOSX_DEPLOYMENT_TARGET=MAC_MINIMUM)
    run(["cargo", "build", "--locked", "--release", "--target-dir", args.target_dir], env=env)
    native = cargo_native(args.target_dir)
    require(native.get("linkage") == "static", "Packaging requires static native linkage")
    metadata = json.loads(output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == BINARY)
    args.output.mkdir(parents=True, exist_ok=True)
    if sys.platform == "darwin":
        target = "aarch64-apple-darwin"
        expected = args.output / "MacinDecode AC-4 Player.app"
        require(not expected.exists(), f"Choose a fresh output directory: {expected}")
        package = make_app(args.target_dir / "release" / BINARY, args.output, version)
        verify_app(package, version)
        executable = package / "Contents/MacOS" / BINARY
    elif sys.platform == "win32":
        target = "x86_64-pc-windows-msvc"
        package = args.output / "MacinDecode-AC4-Player-windows-x64"
        package.mkdir()
        executable = package / (BINARY + ".exe")
        shutil.copy2(args.target_dir / "release" / executable.name, executable)
    else:
        raise SystemExit("Packaging is supported on macOS ARM64 and Windows x64")
    dependencies = verify_binary(executable, target)
    with tempfile.TemporaryDirectory(prefix="macindecode-bundle-check-") as data:
        runtime = run_smoke(executable, pathlib.Path(data))
    runtime.pop("loaded_modules", None)
    manifest = dict(version=version, source_commit=output(["git", "rev-parse", "HEAD"]),
                    native_commit=output(["git", "-C", native["source"], "rev-parse", "HEAD"]),
                    native_linkage="static", executable_sha256=sha256(executable),
                    dependencies=dependencies, smoke_test=runtime)
    (args.output / (package.name + ".json")).write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(package)


if __name__ == "__main__":
    main()

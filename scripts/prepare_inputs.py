"""Prepare checksum/revision-pinned inputs for the full default-feature player."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
INPUTS = ROOT / ".ci-inputs"
SPEC_TABLES = ("generated/ts103190_pdf_tables.rs", "ts_103190_tables.c", "ts_103190_tables_part2.c")


def run(args, **kwargs):
    subprocess.run(list(map(str, args)), check=True, **kwargs)


def checkout(name, url, revision):
    destination = INPUTS / name
    if not (destination / ".git").exists():
        destination.mkdir(parents=True, exist_ok=True)
        run(["git", "init", destination])
        run(["git", "-C", destination, "remote", "add", "origin", url])
    current = subprocess.run(["git", "-C", str(destination), "rev-parse", "HEAD"], text=True, capture_output=True)
    if current.stdout.strip() != revision:
        dirty = subprocess.check_output(["git", "-C", str(destination), "status", "--porcelain"], text=True)
        if dirty.strip(): raise RuntimeError(f"Build input checkout is dirty: {destination}")
        run(["git", "-C", destination, "fetch", "--depth=1", "origin", revision])
        run(["git", "-C", destination, "checkout", "--detach", revision])
    return destination


def spec_identity(core, revision):
    return {"revision": revision, "sha256": {
        name: hashlib.sha256((core / "spec" / name).read_bytes()).hexdigest()
        for name in SPEC_TABLES
    }}


def prepare_spec_tables(core, revision):
    # Preserve table mtimes on cache hits: regenerating identical files forces
    # the decoder and all its dependants to compile again in every profile.
    stamp = core / ".git/player-spec-inputs.json"
    try:
        if json.loads(stamp.read_text(encoding="utf-8")) == spec_identity(core, revision):
            print(f"Reusing verified decoder inputs for {revision}", flush=True)
            return
    except (OSError, ValueError):
        pass
    stamp.unlink(missing_ok=True)
    run([sys.executable, "-m", "pip", "install", "-r", core / "scripts/requirements-spec.txt"])
    run([sys.executable, core / "scripts/fetch_specs.py"])
    run([sys.executable, core / "scripts/generate_spec_tables.py"])
    # Core's build script also checks these tables against its pinned hashes.
    stamp.write_text(json.dumps(spec_identity(core, revision)), encoding="utf-8")


def prepare():
    os.environ.setdefault("PYTHONUTF8", "1")
    os.environ.setdefault("PYTHONIOENCODING", "utf-8")
    INPUTS.mkdir(exist_ok=True)
    settings = {}
    cmake = (ROOT / "crates/macinrender/native/CMakeLists.txt").read_text()
    native_revision = re.search(r"GIT_TAG ([0-9a-f]{40})", cmake)[1]
    native = Path(os.environ["MACINRENDER_SOURCE_DIR"]) if os.getenv("MACINRENDER_SOURCE_DIR") else checkout(
        "macinrender", "https://github.com/SakuzyPeng/MacinRender-ADM-Core.git", native_revision)
    settings["MACINRENDER_SOURCE_DIR"] = str(native.resolve())
    settings["MACINRENDER_FETCHCONTENT_DIR"] = os.environ.get("MACINRENDER_FETCHCONTENT_DIR", str(INPUTS / "native-dependencies"))
    if not os.getenv("MACINDECODE_AC4_SPEC_DIR"):
        revision = re.search(r'MacinDecode-AC4-Core\.git", rev = "([0-9a-f]{40})"', (ROOT / "Cargo.toml").read_text())[1]
        core = checkout("ac4-core", "https://github.com/SakuzyPeng/MacinDecode-AC4-Core.git", revision)
        prepare_spec_tables(core, revision)
        settings["MACINDECODE_AC4_SPEC_DIR"] = str(core / "spec")
    os.environ.update(settings)
    if os.getenv("GITHUB_ENV"):
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as stream:
            for key, value in settings.items(): stream.write(f"{key}={value}\n")
    return settings


if __name__ == "__main__":
    prepare()

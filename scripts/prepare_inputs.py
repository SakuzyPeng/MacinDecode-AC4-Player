"""Prepare checksum/revision-pinned inputs for the full default-feature player."""
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
INPUTS = ROOT / ".ci-inputs"


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
        run([sys.executable, "-m", "pip", "install", "-r", core / "scripts/requirements-spec.txt"])
        run([sys.executable, core / "scripts/fetch_specs.py"])
        run([sys.executable, core / "scripts/generate_spec_tables.py"])
        settings["MACINDECODE_AC4_SPEC_DIR"] = str(core / "spec")
    os.environ.update(settings)
    if os.getenv("GITHUB_ENV"):
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as stream:
            for key, value in settings.items(): stream.write(f"{key}={value}\n")
    return settings


if __name__ == "__main__":
    prepare()

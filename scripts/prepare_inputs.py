"""Prepare checksum/revision-pinned inputs for the full default-feature player."""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
INPUTS = ROOT / ".ci-inputs"
OPENBLAS_VERSION = "0.3.34"
OPENBLAS_SHA = "e9cb6134541f36c27346d5fc5995652f060fba227cebbbabcbda5a5a44d7c76b"
BOOST_SHA = "85a33fa22621b4f314f8e85e1a5e2a9363d22e4f4992925d4bb3bc631b5a0c7a"


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


def download(url, destination, expected):
    if not destination.exists():
        run(["curl", "--fail", "--location", "--retry", "3", "--output", destination, url])
    with destination.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != expected: raise RuntimeError(f"Input checksum mismatch: {destination}")


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
    if os.name == "nt":
        archive = INPUTS / "openblas.zip"
        download(f"https://github.com/OpenMathLib/OpenBLAS/releases/download/v{OPENBLAS_VERSION}/OpenBLAS-{OPENBLAS_VERSION}-x64.zip", archive, OPENBLAS_SHA)
        blas = INPUTS / "openblas"
        with zipfile.ZipFile(archive) as source:
            source.extractall(blas)
        settings.update(OPENBLAS_LIBRARY=str(blas / "lib/libopenblas.lib"), LAPACKE_LIBRARY=str(blas / "lib/libopenblas.lib"),
                        OPENBLAS_HEADER_PATH=str(blas / "include"), LAPACKE_HEADER_PATH=str(blas / "include"))
    if not os.getenv("BOOST_ROOT"):
        boost_archive = INPUTS / "boost.tar.bz2"
        download("https://archives.boost.io/release/1.89.0/source/boost_1_89_0.tar.bz2", boost_archive, BOOST_SHA)
        boost = INPUTS / "boost_1_89_0"
        if not (boost / "boost/version.hpp").exists():
            with tarfile.open(boost_archive) as source:
                source.extractall(INPUTS, members=(m for m in source if m.name.startswith("boost_1_89_0/boost/")), filter="data")
        settings["BOOST_ROOT"] = str(boost)
    os.environ.update(settings)
    if os.getenv("GITHUB_ENV"):
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as stream:
            for key, value in settings.items(): stream.write(f"{key}={value}\n")
    return settings


if __name__ == "__main__":
    prepare()

"""Publish only verified, matching target artifacts to a GitHub draft release."""
import argparse
import json
from pathlib import Path
import subprocess

from package import ROOT, TARGETS, sha256
from verify_runtime import require


def publish(tag, folder):
    manifests = [json.loads(path.read_text()) for path in folder.glob("*.json")]
    require(len(manifests) == len(TARGETS) and {manifest["target"] for manifest in manifests} == set(TARGETS), "Missing installer target")
    assets = []
    expected_commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    for manifest in manifests:
        require(tag == "v" + manifest["version"] and not manifest["working_tree_dirty"], "Unreleasable installer version")
        require(manifest["source_commit"] == expected_commit, "Artifacts are not from this commit")
        name = manifest["installer"]
        require(Path(name).name == name, "Invalid installer filename")
        installer = folder / name
        require(sha256(installer) == manifest["installer_sha256"], "Installer checksum mismatch")
        checksum = installer.with_suffix(installer.suffix + ".sha256")
        require(checksum.read_text().strip() == f"{manifest['installer_sha256']}  {name}", "Checksum file mismatch")
        assets.extend([str(installer), str(checksum), str(installer.with_suffix(".json"))])
    lookup = subprocess.run(["gh", "release", "view", tag, "--json", "isDraft"], text=True, capture_output=True)
    if lookup.returncode == 0:
        require(json.loads(lookup.stdout)["isDraft"], "Refusing to replace assets of a published release")
        subprocess.run(["gh", "release", "upload", tag, "--clobber", *assets], check=True)
    else:
        subprocess.run(["gh", "release", "create", tag, "--verify-tag", "--draft", "--prerelease",
                        "--title", f"MacinDecode AC-4 Player {tag} preview",
                        "--notes-file", str(ROOT / "packaging/release-notes.md"), *assets], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--dist", required=True, type=Path)
    args = parser.parse_args()
    publish(args.tag, args.dist)

#!/bin/bash
set -euo pipefail
PROBE_SOURCE_DIR="$(cd "$(dirname "$0")" && pwd)"
PROBE_ROOT="$(cd "$PROBE_SOURCE_DIR/../.." && pwd)"
PROBE_OUT="$PROBE_ROOT/target/atmos-control-center"
PROBE_APP="$PROBE_OUT/AtmosBadgeProbe.app"
mkdir -p "$PROBE_APP/Contents/MacOS" "$PROBE_APP/Contents/Resources"
swiftc -O -swift-version 5 -parse-as-library -target arm64-apple-macos15.0 \
    "$PROBE_SOURCE_DIR/main.swift" -o "$PROBE_APP/Contents/MacOS/AtmosBadgeProbe"
python3 - "$PROBE_APP" "$PROBE_OUT" "${1:-}" "${2:-}" <<'PY'
import json, pathlib, plistlib, sys
app, out, joc, pcm = sys.argv[1:]
contents = pathlib.Path(app) / "Contents"
info = dict(CFBundleExecutable="AtmosBadgeProbe", CFBundleIdentifier="dev.macinrender.atmos-badge-probe",
    CFBundleName="Atmos 标识实验", CFBundleDisplayName="Atmos 标识实验", CFBundlePackageType="APPL",
    CFBundleShortVersionString="0.1", CFBundleVersion="1", LSMinimumSystemVersion="15.0",
    NSHighResolutionCapable=True, CFBundleDevelopmentRegion="zh-Hans", CFBundleLocalizations=["zh-Hans", "en"])
(contents / "Info.plist").write_bytes(plistlib.dumps(info))
(contents / "Resources/config.json").write_text(json.dumps(dict(joc=joc, pcm=pcm or None,
    log=str(pathlib.Path(out) / "events.jsonl")), ensure_ascii=False))
PY
codesign --force --sign - --timestamp=none "$PROBE_APP"
codesign --verify --deep --strict "$PROBE_APP"
echo "$PROBE_APP"

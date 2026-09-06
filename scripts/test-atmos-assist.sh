#!/bin/bash
set -euo pipefail
ATMOS_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ATMOS_OUT="$ATMOS_ROOT/target/atmos-assist-tests"
mkdir -p "$ATMOS_OUT"
xcrun clang++ -std=c++17 -fobjc-arc -fblocks -O2 -Wall -Wextra -Werror \
  -mmacosx-version-min=14.0 \
  "$ATMOS_ROOT/crates/macinrender/native/atmos_assist.mm" \
  "$ATMOS_ROOT/crates/macinrender/native/atmos_assist_test.mm" \
  -framework AVFoundation -framework Foundation -framework CoreAudio \
  -framework AudioToolbox -framework CoreMedia -framework MediaToolbox \
  -o "$ATMOS_OUT/atmos-assist-test"
exec "$ATMOS_OUT/atmos-assist-test" "$ATMOS_ROOT/assets/audio/atmos-assist.m4a" "${1:-65}"

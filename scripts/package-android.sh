#!/bin/bash
# The lite's client for Android as one archive: dist/outcast-lite-client-<version>-android.tar.gz,
# unpacking to outcast-lite-client-<version>-android/ with the reference client built for
# each ABI (arm64-v8a, armeabi-v7a, x86_64), the crate's source for a remote that links it,
# README.md and LICENSE. The three builds come from cargo-ndk (the CI android job); the
# signatures come at release from scripts/sign-release.sh.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
top="outcast-lite-client-$VERSION-android"
STAGE=$ROOT/dist/stage-android
rm -rf "$STAGE"
mkdir -p "$STAGE/$top" "$ROOT/dist"
for pair in aarch64-linux-android:arm64-v8a armv7-linux-androideabi:armeabi-v7a x86_64-linux-android:x86_64; do
  triple=${pair%%:*}; abi=${pair##*:}
  bin="$ROOT/target/$triple/release/examples/lite-client"
  [ -f "$bin" ] || { echo "package-android: $bin is missing; build with cargo ndk first" >&2; exit 1; }
  install -D -m 755 "$bin" "$STAGE/$top/$abi/lite-client"
done
cp -r "$ROOT/crates/lite-client" "$STAGE/$top/crate"
rm -rf "$STAGE/$top/crate/target"
install -m 644 "$ROOT/docs/outcast-lite.md" "$STAGE/$top/README.md"
install -m 644 "$ROOT/LICENSE" "$STAGE/$top/LICENSE"
rm -f "$ROOT/dist/$top.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='@0' -C "$STAGE" -czf "$ROOT/dist/$top.tar.gz" "$top"
echo "package-android: dist/$top.tar.gz ($(stat -c %s "$ROOT/dist/$top.tar.gz") bytes)"
rm -rf "$STAGE"

#!/bin/bash
# The lite's client for a remote: dist/outcast-lite-client-<version>-<platform>.tar.gz, one
# per platform asked for, each unpacking to outcast-lite-client-<version>-<platform>/ with the
# reference client built for the platform, the crate's source for a remote that links it,
# README.md (docs/outcast-lite.md) and LICENSE. The archive is listed after it is made and
# refused when a file is missing.
#   package-client.sh PLATFORM...     linux-x64 linux-armv7 linux-armv8 windows-x64 android
# The builds come from scripts/ship.sh (Linux, static against musl), scripts/ship-client-
# windows.sh (MinGW-w64) and cargo-ndk (Android, the CI job); the signatures come at release
# from scripts/sign-release.sh, which packs the archive again with the same flags.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
[ $# -gt 0 ] || { echo "package-client: which platforms? linux-x64 linux-armv7 linux-armv8 windows-x64 android" >&2; exit 2; }
mkdir -p "$ROOT/dist"

take() { # take <built file> <path in the archive>
  [ -f "$1" ] || { echo "package-client: $1 is missing; build it first" >&2; exit 1; }
  install -D -m 755 "$1" "$stage/$2"
  expected="$expected $2"
}

for platform in "$@"; do
  top="outcast-lite-client-$VERSION-$platform"
  stage="$ROOT/dist/stage-client/$top"
  rm -rf "$stage"
  mkdir -p "$stage"
  expected=""
  case "$platform" in
    linux-x64)   take "target/x86_64-unknown-linux-musl/release/examples/lite-client" bin/lite-client ;;
    linux-armv7) take "target/armv7-unknown-linux-musleabihf/release/examples/lite-client" bin/lite-client ;;
    linux-armv8) take "target/aarch64-unknown-linux-musl/release/examples/lite-client" bin/lite-client ;;
    windows-x64) take "target/x86_64-pc-windows-gnu/release/examples/lite-client.exe" bin/lite-client.exe ;;
    android)
      take "target/aarch64-linux-android/release/examples/lite-client" arm64-v8a/lite-client
      take "target/armv7-linux-androideabi/release/examples/lite-client" armeabi-v7a/lite-client
      take "target/x86_64-linux-android/release/examples/lite-client" x86_64/lite-client ;;
    *) echo "package-client: no such platform: $platform" >&2; exit 2 ;;
  esac
  mkdir -p "$stage/crate"
  cp -r "$ROOT/crates/lite-client/Cargo.toml" "$ROOT/crates/lite-client/src" "$ROOT/crates/lite-client/examples" "$stage/crate/"
  install -m 644 "$ROOT/docs/outcast-lite.md" "$stage/README.md"
  install -m 644 "$ROOT/LICENSE" "$stage/LICENSE"
  expected="$expected crate/Cargo.toml crate/src/lib.rs crate/examples/lite-client.rs README.md LICENSE"
  rm -f "$ROOT/dist/$top.tar.gz"
  # Owners and order fixed, so the archive is the same for the same tree.
  tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='@0' -C "$ROOT/dist/stage-client" -czf "$ROOT/dist/$top.tar.gz" "$top"
  listed=$(tar -tzf "$ROOT/dist/$top.tar.gz")
  count=0
  for f in $expected; do
    echo "$listed" | grep -qx "$top/$f" || { echo "package-client: $f is not in $top.tar.gz" >&2; exit 1; }
    count=$((count + 1))
  done
  echo "package-client: dist/$top.tar.gz ($(stat -c %s "$ROOT/dist/$top.tar.gz") bytes, $count files)"
done
rm -rf "$ROOT/dist/stage-client"

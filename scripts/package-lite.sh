#!/bin/bash
# The lite as the component another project fetches: dist/outcast-lite-<version>-<arch>.tar.gz
# for every architecture, each unpacking to outcast-lite-<version>-<arch>/ with bin/outcast-lite,
# README.md (docs/outcast-lite.md) and LICENSE. The signatures (MANIFEST inside, SHA256SUMS
# beside) come at release from scripts/sign-release.sh, which packs the archive again with
# the same flags. The binaries come from scripts/ship.sh.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCHES="arm armv7 armv8 x64"
STAGE=$ROOT/dist/stage-lite
rm -rf "$STAGE"
mkdir -p "$STAGE" "$ROOT/dist"
for arch in $ARCHES; do
  bin="$ROOT/bin/$arch/outcast-lite"
  [ -x "$bin" ] || { echo "package-lite: $bin is missing; run scripts/ship.sh first" >&2; exit 1; }
  top="outcast-lite-$VERSION-$arch"
  install -D -m 755 "$bin" "$STAGE/$top/bin/outcast-lite"
  install -m 644 "$ROOT/docs/outcast-lite.md" "$STAGE/$top/README.md"
  install -m 644 "$ROOT/LICENSE" "$STAGE/$top/LICENSE"
  rm -f "$ROOT/dist/$top.tar.gz"
  # Owners and order fixed, so the archive is the same for the same tree.
  tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='@0' -C "$STAGE" -czf "$ROOT/dist/$top.tar.gz" "$top"
  echo "package-lite: dist/$top.tar.gz ($(stat -c %s "$ROOT/dist/$top.tar.gz") bytes)"
done
rm -rf "$STAGE"

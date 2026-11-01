#!/bin/bash
# Build the core for each Volumio architecture and put it in bin/<arch>/outcast.
# Players run those files; they do not run cargo.
#
# Every binary is linked statically against musl: the daemon needs nothing from a
# player's C library, and a cross glibc would otherwise bind the standard library to a
# glibc newer than Volumio's 2.36. The arm binary is the armv7 build: Volumio's universal
# Raspberry Pi image labels itself arm, and the evotap and evopeg the plugin ships for it
# are armv7 builds too, so a Pi Zero or Pi 1 (ARMv6) is not served.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
unset CARGO_TARGET_DIR

# The cross compilers only drive the link; the C runtime and libc come with the Rust
# musl targets themselves.
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=arm-linux-gnueabihf-gcc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc

ship() {
  local triple=$1 arch=$2
  echo "ship: $arch ($triple)"
  cargo build --release --locked --target "$triple" -p outcast
  install -D -m 755 "target/$triple/release/outcast" "bin/$arch/outcast"
}
ship x86_64-unknown-linux-musl x64
ship armv7-unknown-linux-musleabihf armv7
ship aarch64-unknown-linux-musl armv8
install -D -m 755 bin/armv7/outcast bin/arm/outcast

echo "ship: static"
for bin in bin/*/outcast; do
  # x86-64 musl links as a static PIE; file words that differently.
  file "$bin" | grep -qE 'statically linked|static-pie linked' || { echo "ship: $bin is not statically linked" >&2; exit 1; }
  if readelf -W --dyn-syms "$bin" 2>/dev/null | grep -q 'GLIBC_'; then
    echo "ship: $bin needs glibc" >&2; exit 1
  fi
done
echo "ship: armv7"
readelf -A bin/armv7/outcast | grep -q 'Tag_CPU_arch: v7' || { echo "ship: bin/armv7/outcast is not ARMv7 code" >&2; exit 1; }
echo "ship: version"
said=$(bin/x64/outcast --version)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
[ "$said" = "outcast $version" ] || { echo "ship: the core says '$said', the workspace is $version" >&2; exit 1; }
file bin/arm/outcast bin/armv7/outcast bin/armv8/outcast bin/x64/outcast

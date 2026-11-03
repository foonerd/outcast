#!/bin/bash
# The lite's reference client for Windows, cross-built with MinGW-w64 (in Glass's builder
# image): target/x86_64-pc-windows-gnu/release/examples/lite-client.exe, for
# scripts/package-client.sh windows-x64. The crate is pure Rust; MinGW only drives the link.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
unset CARGO_TARGET_DIR
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
cargo build --release --locked --target x86_64-pc-windows-gnu -p lite-client --examples
exe=target/x86_64-pc-windows-gnu/release/examples/lite-client.exe
[ -f "$exe" ] || { echo "ship-client-windows: no $exe" >&2; exit 1; }
file "$exe" | grep -q 'PE32+' || { echo "ship-client-windows: $exe is not a 64-bit Windows executable" >&2; exit 1; }
echo "ship-client-windows: $exe ($(stat -c %s "$exe") bytes)"

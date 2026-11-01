#!/bin/bash
# The plugin as the zip a player installs: dist/outcast-<version>.zip, with the plugin's
# files at its root, the core from bin/<arch> for every architecture, evotap's library and
# reader and evopeg's binary from the verified component releases (scripts/
# fetch-components.sh), components.json naming each component's version and every file's
# digest, build.json naming the commit, and the node modules.
#
# The plugin's version follows the workspace version in Cargo.toml.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
NAME=outcast
STAGE=$ROOT/dist/stage/$NAME
ARCHES="arm armv7 armv8 x64"
rm -rf "$ROOT/dist/stage"
mkdir -p "$STAGE" "$ROOT/dist"
cp -r "$ROOT/plugin/." "$STAGE/"
rm -rf "$STAGE/test" "$STAGE/node_modules" "$STAGE/package-lock.json"
rm -f "$STAGE"/asound/*.conf
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/" "$STAGE/package.json"

# The build the zip carries: the commit and the time.
printf '{ "commit": "%s", "built": "%s" }\n' \
  "$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)" \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$STAGE/build.json"

component_version() { sed -n 's/^VERSION=//p' "$ROOT/components/$1.lock"; }
EVOTAP=$(component_version evotap)
EVOPEG=$(component_version evopeg)
for name in evotap evopeg; do
  [ "$(cat "$ROOT/target/components/$name/VERSION" 2>/dev/null)" = "$(component_version "$name")" ] \
    || { echo "package: $name $(component_version "$name") is not fetched; run scripts/fetch-components.sh" >&2; exit 1; }
done

for arch in $ARCHES; do
  bin="$ROOT/bin/$arch/outcast"
  [ -x "$bin" ] || { echo "package: $bin is missing; run scripts/ship.sh first" >&2; exit 1; }
  install -D -m 755 "$bin" "$STAGE/bin/$arch/outcast"
  tap="$ROOT/target/components/evotap/$arch/evotap-$EVOTAP-$arch"
  install -D -m 644 "$tap/lib/libevotap.so" "$STAGE/lib/$arch/libevotap.so"
  install -D -m 755 "$tap/bin/evotap-cat" "$STAGE/bin/$arch/evotap-cat"
  peg="$ROOT/target/components/evopeg/$arch/evopeg-$EVOPEG-$arch"
  install -D -m 755 "$peg/bin/evopeg" "$STAGE/bin/$arch/evopeg"
done
# The licences of what the components carry, beside the plugin's own.
mkdir -p "$STAGE/licenses"
cp "$ROOT/target/components/evopeg/x64/evopeg-$EVOPEG-x64"/licenses/* "$STAGE/licenses/"
install -m 644 "$ROOT/target/components/evotap/x64/evotap-$EVOTAP-x64/LICENSE" "$STAGE/licenses/evotap-LICENSE"

# components.json: each component's version and repository, and the digest and size of
# every file the zip took from it, per architecture.
{
  echo "{"
  first=1
  for name in evotap evopeg; do
    [ "$first" -eq 1 ] || echo ","
    first=0
    printf '  "%s": {\n    "version": "%s",\n    "repository": "%s",\n    "files": {\n' \
      "$name" "$(component_version "$name")" "$(sed -n 's/^REPOSITORY=//p' "$ROOT/components/$name.lock")"
    inner=1
    for arch in $ARCHES; do
      for rel in "lib/$arch/libevotap.so" "bin/$arch/evotap-cat" "bin/$arch/evopeg"; do
        case "$name:$rel" in evotap:*evopeg) continue ;; evopeg:*evotap*) continue ;; esac
        f="$STAGE/$rel"
        [ "$inner" -eq 1 ] || echo ","
        inner=0
        printf '      "%s": { "sha256": "%s", "size": %s }' "$rel" "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c %s "$f")"
      done
    done
    printf '\n    }\n  }'
  done
  echo
  echo "}"
} > "$STAGE/components.json"

# Node modules: with npm on this machine, directly; otherwise in a container with the
# Node major Volumio ships. A zip without them is no plugin: the install must succeed,
# and every module package.json names must be there.
if command -v npm >/dev/null 2>&1; then
  ( cd "$STAGE" && npm install --omit=dev --no-audit --no-fund --loglevel=error >/dev/null )
else
  docker run --rm -v "$STAGE:/plugin" -w /plugin node:20-bookworm-slim sh -c \
    "npm install --omit=dev --no-audit --no-fund --loglevel=error >/dev/null; rc=\$?; chown -R $(id -u):$(id -g) /plugin/node_modules /plugin/package-lock.json 2>/dev/null; exit \$rc" \
    || { echo "package: npm install failed; the zip was not made" >&2; exit 1; }
fi
rm -f "$STAGE/package-lock.json"
for module in $(sed -n '/"dependencies"/,/}/p' "$STAGE/package.json" | sed -n 's/^ *"\([^"]*\)": *"[^"]*",*$/\1/p'); do
  [ -f "$STAGE/node_modules/$module/package.json" ] || { echo "package: the module $module is not in the stage; the zip was not made" >&2; exit 1; }
done

ZIP=$ROOT/dist/$NAME-$VERSION.zip
rm -f "$ZIP"
( cd "$STAGE" && zip -qr "$ZIP" . )
rm -rf "$ROOT/dist/stage"
echo "package: $ZIP: $(stat -c %s "$ZIP") bytes, sha256 $(sha256sum "$ZIP" | cut -d' ' -f1); evotap $EVOTAP, evopeg $EVOPEG"

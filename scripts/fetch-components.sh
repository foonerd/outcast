#!/bin/bash
# The components the plugin ships but does not build: evotap and evopeg, each from its
# own GitHub release at the version its lock file names (components/<name>.lock), one
# archive per architecture. Nothing is unpacked before it is verified: the release's
# SHA256SUMS against its Ed25519 signature with the evo release key the plugin carries
# (keys/release-signing.pub), each archive against the sums, and each archive's own
# MANIFEST against its signature and its files. Unpacked under
# target/components/<name>/<arch>/, where package.sh looks.
#
#   scripts/fetch-components.sh            every component, every architecture
#   scripts/fetch-components.sh evopeg     one component
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
ARCHES="armv7 armv8 x64"
PUB="$ROOT/keys/release-signing.pub"

fetch() { # url, file
  # GitHub's download addresses falter at times (504 for minutes on end); tried with
  # patience, then through the API's asset endpoint, which has served when they did not.
  local url=$1 file=$2
  if curl -fsSL --retry 6 --retry-delay 10 --retry-all-errors --retry-max-time 240 -o "$file" "$url"; then
    return 0
  fi
  echo "fetch-components: $url did not arrive from the download address; asking the API for it" >&2
  # https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>
  local rest=${url#https://github.com/} owner repo tag asset id
  owner=${rest%%/*}; rest=${rest#*/}
  repo=${rest%%/*}; rest=${rest#*/releases/download/}
  tag=${rest%%/*}; asset=${rest#*/}
  command -v jq >/dev/null 2>&1 || { echo "fetch-components: $url did not arrive, and jq is not here for the API" >&2; exit 1; }
  # The token, when the environment has one, through a curl configuration file, so the
  # header with its space never meets the shell's word splitting.
  local conf
  conf=$(mktemp)
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then printf 'header = "Authorization: Bearer %s"\n' "$GITHUB_TOKEN" > "$conf"; fi
  id=$(curl -fsSL --retry 3 --retry-delay 5 -K "$conf" -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/$owner/$repo/releases/tags/$tag" | jq -r --arg n "$asset" '.assets[] | select(.name == $n) | .id' | head -n1) || id=""
  [[ -n "$id" ]] || { rm -f "$conf"; echo "fetch-components: $url did not arrive, and the API names no asset $asset" >&2; exit 1; }
  curl -fsSL --retry 6 --retry-delay 10 --retry-all-errors --retry-max-time 240 -K "$conf" \
    -H "Accept: application/octet-stream" -o "$file" "https://api.github.com/repos/$owner/$repo/releases/assets/$id" \
    || { rm -f "$conf"; echo "fetch-components: $url did not arrive from the API either" >&2; exit 1; }
  rm -f "$conf"
}

one() {
  local lock=$1 name version repository base dir arch top
  name=$(sed -n 's/^NAME=//p' "$lock")
  version=$(sed -n 's/^VERSION=//p' "$lock")
  repository=$(sed -n 's/^REPOSITORY=//p' "$lock")
  [[ -n "$name" && -n "$version" && -n "$repository" ]] || { echo "fetch-components: $lock needs NAME, VERSION and REPOSITORY" >&2; exit 1; }
  base="https://github.com/$repository/releases/download/v$version"
  dir="$ROOT/target/components/$name"
  if [[ -f "$dir/VERSION" && "$(cat "$dir/VERSION")" == "$version" ]]; then
    echo "fetch-components: $name $version is here already"
    return 0
  fi
  rm -rf "$dir" && mkdir -p "$dir/download"
  echo "fetch-components: $name $version from $base"
  fetch "$base/SHA256SUMS" "$dir/download/SHA256SUMS"
  fetch "$base/SHA256SUMS.sig" "$dir/download/SHA256SUMS.sig"
  openssl pkeyutl -verify -pubin -inkey "$PUB" -rawin -in "$dir/download/SHA256SUMS" -sigfile "$dir/download/SHA256SUMS.sig" > /dev/null \
    || { echo "fetch-components: the SHA256SUMS of $name $version is not signed with the evo release key" >&2; exit 1; }
  for arch in $ARCHES; do
    top="$name-$version-$arch"
    fetch "$base/$top.tar.gz" "$dir/download/$top.tar.gz"
    (cd "$dir/download" && grep -F " $top.tar.gz" SHA256SUMS | sha256sum -c --quiet -) \
      || { echo "fetch-components: $top.tar.gz is not the file the signed sums name" >&2; exit 1; }
    mkdir -p "$dir/$arch"
    tar -C "$dir/$arch" -xzf "$dir/download/$top.tar.gz"
    [[ -f "$dir/$arch/MANIFEST" && -f "$dir/$arch/MANIFEST.sig" ]] || { echo "fetch-components: $top.tar.gz carries no MANIFEST" >&2; exit 1; }
    openssl pkeyutl -verify -pubin -inkey "$PUB" -rawin -in "$dir/$arch/MANIFEST" -sigfile "$dir/$arch/MANIFEST.sig" > /dev/null \
      || { echo "fetch-components: the MANIFEST of $top.tar.gz is not signed with the evo release key" >&2; exit 1; }
    (cd "$dir/$arch" && sha256sum -c --quiet MANIFEST) || { echo "fetch-components: $top.tar.gz does not match its MANIFEST" >&2; exit 1; }
    # The archive's top folder, by its name, so package.sh finds the files without it.
    [[ -d "$dir/$arch/$top" ]] || { echo "fetch-components: $top.tar.gz does not unpack to $top/" >&2; exit 1; }
    echo "  $top: verified, $(grep -c . "$dir/$arch/MANIFEST") files"
  done
  echo "$version" > "$dir/VERSION"
}

if [[ $# -gt 0 ]]; then
  for name in "$@"; do one "components/$name.lock"; done
else
  for lock in components/*.lock; do one "$lock"; done
fi

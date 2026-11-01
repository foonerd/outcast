#!/bin/bash
# The plugin's ALSA template goes to the player as it is, so it must hold no variable
# left to fill and must parse as ALSA configuration. The parse needs aplay; without it
# only the variables are checked.
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
status=0
for tmpl in "$ROOT"/plugin/*.conf.tmpl; do
  name=$(basename "$tmpl")
  if grep -n '\${' "$tmpl"; then
    echo "asound: $name holds a variable nothing fills"
    status=1
    continue
  fi
  grep -q 'ring "outcast"' "$tmpl" || { echo "asound: $name does not name the ring outcast"; status=1; }
  grep -q 'audio 1' "$tmpl" || { echo "asound: $name does not ask for the audio ring"; status=1; }
  if command -v aplay >/dev/null 2>&1; then
    work=$(mktemp -d)
    {
      echo '</usr/share/alsa/alsa.conf>'
      cat "$tmpl"
      # What the player defines around the snippet: the PCM after it.
      echo 'pcm.postOutcast { type null }'
    } > "$work/asound.conf"
    if ALSA_CONFIG_PATH="$work/asound.conf" aplay -L >/dev/null 2> "$work/err"; then
      echo "asound: $name parses"
    else
      echo "asound: $name does not parse:"
      cat "$work/err"
      status=1
    fi
    rm -rf "$work"
  else
    echo "asound: $name holds no variable (no aplay here to parse it)"
  fi
done
exit $status

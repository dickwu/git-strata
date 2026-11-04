#!/usr/bin/env bash
# Reproduced with rsvg-convert 2.63.4; PNG comparison permits +/-2 alpha.
# No app-icon generation, desktop launch, daemon or live-store access.
set -euo pipefail
src=$(cd "$(dirname "$0")" && pwd)
crate=$(cd "$src/../.." && pwd)
if [[ "${1:-}" == --check ]]; then
  out=$(mktemp -d "${TMPDIR:-/tmp}/strata-tray.XXXXXX")
  trap 'rm -rf "$out"' EXIT
  python3 "$src/render-tray.py" "$out"
  (cd "$crate" && cargo run --quiet --example tray_assets_check -- "$out")
else
  python3 "$src/render-tray.py" "$crate/icons/tray"
fi

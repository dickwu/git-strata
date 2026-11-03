#!/usr/bin/env bash
# Regenerate app icons from the SVGs in this directory (gui.md §5.8, §8.2).
# Needs rsvg-convert (librsvg) and the project's @tauri-apps/cli. Run anywhere.
#
#   app-icon.svg            concept b, "workspace fork": the app icon
#
# Only the files tauri.conf.json bundles are kept from `tauri icon` (macOS only).
set -euo pipefail
src=$(cd "$(dirname "$0")" && pwd)
icons=$(dirname "$src")
desktop=$(cd "$icons/../.." && pwd)

rsvg-convert -w 1024 -h 1024 "$src/app-icon.svg" -o "$icons/app-icon.png"
out=$(mktemp -d "${TMPDIR:-/tmp}/strata-icons.XXXXXX")
trap 'rm -rf "$out"' EXIT
(cd "$desktop" && bun run tauri icon src-tauri/icons/app-icon.png -o "$out" >/dev/null)
for f in 32x32.png 128x128.png 128x128@2x.png icon.icns icon.png; do
  cp "$out/$f" "$icons/$f"
done

echo "app icons regenerated in $icons (tray: run render-tray.sh separately)"

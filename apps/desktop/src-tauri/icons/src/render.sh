#!/usr/bin/env bash
# Regenerate every icon from the SVGs in this directory (gui.md §5.8, §8.2).
# Needs rsvg-convert (librsvg) and the project's @tauri-apps/cli. Run anywhere.
#
#   app-icon.svg            concept b, "workspace fork": the app icon
#   tray-template*.svg      menu-bar template images (black on transparent),
#                           18 pt: normal, pending (dot), down (slash)
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

mkdir -p "$icons/tray"
for state in "" -pending -down; do
  rsvg-convert -w 18 -h 18 "$src/tray-template$state.svg" -o "$icons/tray/trayTemplate$state.png"
  rsvg-convert -w 36 -h 36 "$src/tray-template$state.svg" -o "$icons/tray/trayTemplate$state@2x.png"
done
echo "icons regenerated in $icons"

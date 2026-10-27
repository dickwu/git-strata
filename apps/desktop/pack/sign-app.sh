#!/usr/bin/env bash
# Sign GIT STRATA.app inside-out with one identity (gui.md §2.1, §6):
#   Contents/MacOS/git-stratad        io.github.dickwu.git-strata.daemon
#   Contents/MacOS/git-strata         io.github.dickwu.git-strata.cli
#   Contents/MacOS/strata-pack-probe  io.github.dickwu.git-strata.pack-probe (P-pack only)
#   the app itself                    io.github.dickwu.git-strata (from Info.plist)
# usage: sign-app.sh <path/GIT STRATA.app> <identity SHA-1 | ->
# "-" is ad-hoc: the DR is the cdhash and changes with every build.
# STRATA_SIGN_KEYCHAIN (optional) names the keychain holding the identity, so
# the user's keychain search list is never touched.
set -euo pipefail
app="$1"; id="$2"
kc=()
[[ -n "${STRATA_SIGN_KEYCHAIN:-}" && "$id" != "-" ]] && kc=(--keychain "$STRATA_SIGN_KEYCHAIN")
macos="$app/Contents/MacOS"
prefix="io.github.dickwu.git-strata"

sign() { # file identifier
  codesign --force "${kc[@]}" --sign "$id" --identifier "$2" --options runtime --timestamp=none "$1"
}
for need in git-stratad git-strata; do
  [[ -f "$macos/$need" ]] || { echo "error: $macos/$need is missing" >&2; exit 1; }
done
sign "$macos/git-stratad" "$prefix.daemon"
sign "$macos/git-strata" "$prefix.cli"
[[ -f "$macos/strata-pack-probe" ]] && sign "$macos/strata-pack-probe" "$prefix.pack-probe"
codesign --force "${kc[@]}" --sign "$id" --options runtime --timestamp=none "$app"
codesign --verify --deep --strict --verbose=2 "$app"

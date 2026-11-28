#!/usr/bin/env bash
# Check the actual inputs to the CLI archive and the app, before signing or copying.
# Usage: check-release-workers.sh <dir with CLI and daemon> [tauri .app]
set -euo pipefail
export LC_ALL=C
[[ $# == 1 || $# == 2 ]] || { echo 'expected binary directory and optional app bundle' >&2; exit 64; }
bins=$1
bad() { echo "unsafe release input: $*" >&2; exit 1; }

check_test_marker() {
  local rc=0
  grep -aFq 'GIT-STRATA:TEST-SEAMS:V1' "$1" || rc=$?
  case $rc in
    0) bad "test-seams binary at $1" ;;
    1) ;;
    *) bad "could not inspect $1" ;;
  esac
}
require_production() {
  [[ -f "$1" && -r "$1" ]] || bad "missing binary at $1"
  check_test_marker "$1"
  grep -aFq 'GIT-STRATA:PRODUCTION:V1' "$1" || bad "missing production build marker at $1"
}

[[ ! -e "$bins/git-stratad-test" && ! -L "$bins/git-stratad-test" ]] || bad "git-stratad-test in $bins"
require_production "$bins/git-stratad"
require_production "$bins/git-strata"
if [[ $# == 2 ]]; then
  app=$2
  require_production "$app/Contents/MacOS/git-strata-desktop"
  # ditto copies the whole source app. Catch a renamed helper in Resources too.
  # Keep find in the foreground so traversal errors propagate.
  list=$(mktemp)
  trap 'rm -f "$list"' EXIT
  find "$app" \( -type f -o -type l \) -print0 > "$list"
  while IFS= read -r -d '' file; do
    [[ ${file##*/} != git-stratad-test ]] || bad "git-stratad-test in $app"
    if [[ -f "$file" ]]; then
      check_test_marker "$file"
    fi
  done < "$list"
fi

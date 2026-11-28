#!/usr/bin/env bash
# All inputs are inert files. A refusal must precede assembly and signing.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
desktop=$(cd "$here/../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
bins="$fixture/bin"
app="$fixture/Input.app"
out="$fixture/out"
mkdir -p "$bins" "$app/Contents/MacOS" "$app/Contents/Resources" "$out/GIT STRATA.app"
touch "$out/GIT STRATA.app/preserve"
clean() { printf '\0GIT-STRATA:PRODUCTION:V1\0' > "$1"; }
seams() { printf '\0GIT-STRATA:TEST-SEAMS:V1\0' > "$1"; }
check=(bash "$desktop/scripts/check-release-workers.sh" "$bins" "$app")
assemble=(bash "$desktop/pack/assemble.sh" "$app" "$bins" - "$out")
tarball=(bash "$desktop/../../scripts/release/package-cli.sh" "$bins" - 0.0.0 "$out")
reject() {
  local expected=$1
  shift
  if "$@" > "$fixture/log" 2>&1; then
    echo "expected refusal for $expected" >&2; exit 1
  fi
  grep -Fq "unsafe release input: $expected" "$fixture/log" || { cat "$fixture/log" >&2; exit 1; }
  [[ -f "$out/GIT STRATA.app/preserve" ]]
}
clean "$bins/git-strata"
clean "$bins/git-stratad"
clean "$app/Contents/MacOS/git-strata-desktop"
"${check[@]}"
seams "$bins/git-stratad-test"
reject git-stratad-test "${assemble[@]}"
reject git-stratad-test "${tarball[@]}"
reject git-stratad-test "${check[@]}"
mv "$bins/git-stratad-test" "$bins/git-stratad"
reject test-seams "${assemble[@]}"
reject test-seams "${check[@]}"
clean "$bins/git-stratad"
seams "$bins/git-strata"
reject test-seams "${check[@]}"
clean "$bins/git-strata"
touch "$app/Contents/Resources/git-stratad-test"
reject git-stratad-test "${assemble[@]}"
mv "$app/Contents/Resources/git-stratad-test" "$app/Contents/Resources/helper"
seams "$app/Contents/Resources/helper"
reject test-seams "${check[@]}"
rm "$app/Contents/Resources/helper"
: > "$bins/git-stratad"
reject 'missing production build marker' "${check[@]}"
echo 'release worker refusals passed'

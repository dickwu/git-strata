#!/usr/bin/env bash
# check-release-no-connector.sh fails on each way the connector could leak
# into a release build, and passes on the repo's real config.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
desktop=$(dirname "$(dirname "$here")")
check="$desktop/scripts/check-release-no-connector.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pass=0
failn=0

copy() {
  rm -rf "$work/d"
  mkdir -p "$work/d/src-tauri"
  cp "$desktop/package.json" "$work/d/"
  cp -R "$desktop/src-tauri/tauri.conf.json" "$desktop/src-tauri/Cargo.toml" \
        "$desktop/src-tauri/capabilities" "$work/d/src-tauri/"
}
run() { DESKTOP_DIR="$work/d" SKIP_CARGO_TREE=1 bash "$check" "$@" >"$work/out" 2>&1; }
expect() { # expect <ok|fail> <name> [args]
  local want=$1 name=$2; shift 2
  if run "$@"; then got=ok; else got=fail; fi
  if [ "$got" = "$want" ]; then pass=$((pass+1)); echo "ok   $name"
  else failn=$((failn+1)); echo "FAIL $name (want $want, got $got)"; cat "$work/out"; fi
}
edit_json() { local f=$1 q=$2; jq "$q" "$f" >"$f.new" && mv "$f.new" "$f"; }

copy; expect ok "repo config passes"

copy; edit_json "$work/d/src-tauri/tauri.conf.json" '.app.security.csp = null'
expect fail "null CSP"

copy; edit_json "$work/d/src-tauri/tauri.conf.json" '.build.features = ["dev-connector"]'
expect fail "build.features enables dev-connector"

copy; edit_json "$work/d/package.json" '.scripts.tauri = "tauri --features dev-connector"'
expect fail "non-dev script enables dev-connector"

copy; edit_json "$work/d/src-tauri/capabilities/default.json" '.permissions += ["connector:default"]'
expect fail "connector permission in capabilities/"

copy; sed -i '' 's/^default = \[\]/default = ["dev-connector"]/' "$work/d/src-tauri/Cargo.toml"
expect fail "dev-connector in default features"

copy; edit_json "$work/d/src-tauri/tauri.conf.json" '.plugins["deep-link"] = {"desktop": {"schemes": ["git-strata"]}}'
expect fail "deep-link URL scheme"

copy; printf 'x tauri_plugin_connector::init y' >"$work/bin"
expect fail "binary containing the plugin" "$work/bin"

copy; printf 'plain binary' >"$work/bin"
expect ok "clean binary" "$work/bin"

echo "$pass passed, $failn failed"
[ "$failn" -eq 0 ]

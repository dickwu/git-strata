#!/usr/bin/env bash
# gui_release_has_no_connector (gui.md G4, §8 test table): a release build
# carries no tauri-plugin-connector, no connector capability, and keeps its
# CSP. The connector exists only behind the dev-connector feature in a
# debug build (`bun run tauri:dev`).
#
#   check-release-no-connector.sh [release-binary]
#
# DESKTOP_DIR overrides apps/desktop (tests); SKIP_CARGO_TREE=1 skips the
# dependency-graph check (tests on a copied tree without a cargo workspace).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
desktop=${DESKTOP_DIR:-$(dirname "$here")}
tauri="$desktop/src-tauri"
conf="$tauri/tauri.conf.json"
fail=0
bad() { echo "::error::gui_release_has_no_connector: $*"; fail=1; }

csp=$(jq -c '.app.security.csp' "$conf")
[ "$csp" != "null" ] || bad "tauri.conf.json app.security.csp is null"

if jq -e '(.build.features // []) | index("dev-connector")' "$conf" >/dev/null; then
  bad "tauri.conf.json build.features enables dev-connector"
fi

# Only tauri:dev may turn the feature on.
while IFS=$'\t' read -r name cmd; do
  case "$cmd" in
    *dev-connector*) [ "$name" = "tauri:dev" ] || bad "package.json script $name enables dev-connector" ;;
  esac
done < <(jq -r '.scripts | to_entries[] | [.key, .value] | @tsv' "$desktop/package.json")

if grep -rqs 'connector:' "$tauri/capabilities"; then
  bad "capabilities/ grants a connector permission (keep it in capabilities-dev/)"
fi

if awk '/^\[features\]/{f=1;next} /^\[/{f=0} f && /^default *=/' "$tauri/Cargo.toml" | grep -q 'dev-connector'; then
  bad "Cargo.toml default features include dev-connector"
fi

# No URL scheme: no deep-link plugin, no CFBundleURLTypes.
if jq -e '.plugins["deep-link"] != null' "$conf" >/dev/null \
   || grep -q 'tauri-plugin-deep-link' "$tauri/Cargo.toml" \
   || grep -rqs 'CFBundleURLTypes' "$tauri"/Info*.plist; then
  bad "the app registers a URL scheme"
fi

if [ "${SKIP_CARGO_TREE:-0}" != 1 ]; then
  # `cargo tree` has no --release; with no --features it is the graph
  # `tauri build` compiles.
  if cargo tree -e features --manifest-path "$tauri/Cargo.toml" --target aarch64-apple-darwin \
      | grep -Eq 'tauri-plugin-connector|connector-client'; then
    bad "release dependency graph contains tauri-plugin-connector"
  fi
fi

if [ $# -ge 1 ]; then
  [ -f "$1" ] || bad "no release binary at $1"
  if [ -f "$1" ] && strings -a "$1" | grep -Eq 'tauri_plugin_connector|tauri-plugin-connector'; then
    bad "release binary $1 contains tauri-plugin-connector"
  fi
fi

[ $fail -eq 0 ] && echo "gui_release_has_no_connector: ok"
exit $fail

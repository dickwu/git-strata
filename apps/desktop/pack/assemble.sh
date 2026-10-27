#!/usr/bin/env bash
# P-pack: turn the bundle `tauri build --bundles app` produced into the layout
# of gui.md §2.1, sign it, check the designated requirements, and make the DMG.
# usage: assemble.sh <tauri .app> <dir with git-stratad and git-strata> <identity SHA-1 | -> <out dir>
# Writes <out>/GIT STRATA.app, <out>/Git-Strata_<version>_<arch>.dmg, <out>/pack-record.md
set -euo pipefail
src_app="$1"; bins="$2"; id="$3"; out="$4"
here="$(cd "$(dirname "$0")" && pwd)"
prefix="io.github.dickwu.git-strata"
mkdir -p "$out"
app="$out/GIT STRATA.app"
rm -rf "$app"
ditto "$src_app" "$app"
macos="$app/Contents/MacOS"

install -m 755 "$bins/git-stratad" "$macos/git-stratad"
install -m 755 "$bins/git-strata" "$macos/git-strata"
arch="$(lipo -archs "$macos/git-stratad")"
case "$arch" in
  arm64|x86_64) ;;
  *) echo "error: git-stratad must be a single-arch arm64 or x86_64 build, got: $arch" >&2; exit 1 ;;
esac
swiftc -target "$arch-apple-macos13.0" -O -o "$macos/strata-pack-probe" "$here/probe/main.swift"
mkdir -p "$app/Contents/Library/LaunchAgents"
install -m 644 "$here/$prefix.daemon.plist" "$app/Contents/Library/LaunchAgents/$prefix.daemon.plist"
plutil -lint "$app/Contents/Library/LaunchAgents/$prefix.daemon.plist"

"$here/sign-app.sh" "$app" "$id"

record="$out/pack-record.md"
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")"
{
  echo "# Pack record"
  echo
  echo "- commit: ${GITHUB_SHA:-local}  run: ${GITHUB_RUN_ID:-local}"
  echo "- version: $version  arch: $arch  signing: $([[ $id == - ]] && echo ad-hoc || echo "self-signed $id")"
  echo "- built on: $(sw_vers -productVersion) $(uname -m), $(xcrun swiftc --version 2>&1 | head -1)"
  echo
  echo "## Designated requirements"
  echo
} > "$record"

fail=0
for pair in "$exe:$prefix" "git-stratad:$prefix.daemon" "git-strata:$prefix.cli" "strata-pack-probe:$prefix.pack-probe"; do
  bin="${pair%%:*}"; want="${pair#*:}"
  dr="$(codesign -d -r- "$macos/$bin" 2>&1 | sed -n 's/^[# ]*designated => //p')"
  ident="$(codesign -dv "$macos/$bin" 2>&1 | sed -n 's/^Identifier=//p')"
  echo "- \`$bin\` identifier \`$ident\`: \`$dr\`" >> "$record"
  [[ "$ident" == "$want" ]] || { echo "error: $bin identifier is $ident, want $want" >&2; fail=1; }
  if [[ "$id" != "-" ]]; then
    # The property TCC relies on: identifier + this certificate, nothing build-specific.
    # A self-signed certificate is its own root, so codesign writes `certificate root`.
    grep -Eqi "certificate (root|leaf) = H\"$id\"" <<<"$dr" || { echo "error: $bin DR does not pin certificate $id: $dr" >&2; fail=1; }
    grep -q "cdhash" <<<"$dr" && { echo "error: $bin DR contains a cdhash: $dr" >&2; fail=1; }
  fi
done
[[ $fail == 0 ]] || exit 1

dmg="$out/Git-Strata_${version}_${arch}.dmg"
stage="$(mktemp -d)"
ditto "$app" "$stage/GIT STRATA.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname "GIT STRATA" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg" > /dev/null
rm -rf "$stage"
kc=()
[[ -n "${STRATA_SIGN_KEYCHAIN:-}" && "$id" != "-" ]] && kc=(--keychain "$STRATA_SIGN_KEYCHAIN")
codesign --force "${kc[@]}" --sign "$id" --timestamp=none "$dmg"
{
  echo
  echo "## Artifacts"
  echo
  echo "- \`$(basename "$dmg")\` sha256 \`$(shasum -a 256 "$dmg" | cut -d' ' -f1)\`"
  echo "- bundle size: $(du -sh "$app" | cut -f1)"
} >> "$record"
echo "$dmg"

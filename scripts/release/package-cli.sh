#!/usr/bin/env bash
# Release: sign git-strata and git-stratad and pack the Homebrew tarball.
# usage: package-cli.sh <dir with git-strata and git-stratad> <identity SHA-1 | -> <version> <out dir>
# Writes <out>/git-strata-<version>-aarch64-apple-darwin.tar.gz with
#   bin/git-strata  bin/git-stratad  share/git-strata/config.toml  LICENSE-MIT  LICENSE-APACHE  README.md
# The binaries get the same identifiers as the copies inside GIT STRATA.app
# (io.github.dickwu.git-strata.cli / .daemon), so with a stable identity both
# share one designated requirement and one TCC grant.
# STRATA_SIGN_KIND: adhoc | self-signed | developer-id (default: adhoc when id is "-", else self-signed)
# STRATA_SIGN_KEYCHAIN (optional): keychain holding the identity.
set -euo pipefail
bins="$1"; id="$2"; version="$3"; out="$4"
root="$(cd "$(dirname "$0")/../.." && pwd)"
prefix="io.github.dickwu.git-strata"
kind="${STRATA_SIGN_KIND:-}"
[[ -n "$kind" ]] || { [[ "$id" == - ]] && kind=adhoc || kind=self-signed; }
case "$kind" in
  adhoc) [[ "$id" == - ]] || { echo "error: adhoc signing needs identity -" >&2; exit 1; } ;;
  self-signed|developer-id) [[ "$id" != - ]] || { echo "error: $kind signing needs an identity SHA-1" >&2; exit 1; } ;;
  *) echo "error: STRATA_SIGN_KIND must be adhoc, self-signed or developer-id, got $kind" >&2; exit 1 ;;
esac
kc=()
[[ -n "${STRATA_SIGN_KEYCHAIN:-}" && "$id" != - ]] && kc=(--keychain "$STRATA_SIGN_KEYCHAIN")
ts=(--timestamp=none)
[[ "$kind" == developer-id ]] && ts=(--timestamp)   # notarization needs a secure timestamp

name="git-strata-$version-aarch64-apple-darwin"
stage="$(mktemp -d)/$name"
mkdir -p "$stage/bin" "$stage/share/git-strata" "$out"
install -m 755 "$bins/git-strata" "$stage/bin/git-strata"
install -m 755 "$bins/git-stratad" "$stage/bin/git-stratad"
install -m 644 "$root/examples/config.toml" "$stage/share/git-strata/config.toml"
install -m 644 "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/README.md" "$stage/"

fail=0
for pair in "git-stratad:$prefix.daemon" "git-strata:$prefix.cli"; do
  bin="${pair%%:*}"; ident="${pair#*:}"
  arch="$(lipo -archs "$stage/bin/$bin")"
  [[ "$arch" == arm64 ]] || { echo "error: $bin must be an arm64 build, got: $arch" >&2; fail=1; }
  codesign --force ${kc[@]+"${kc[@]}"} --sign "$id" --identifier "$ident" --options runtime "${ts[@]}" "$stage/bin/$bin"
  codesign --verify --strict --verbose=2 "$stage/bin/$bin"
  dr="$(codesign -d -r- "$stage/bin/$bin" 2>&1 | sed -n 's/^[# ]*designated => //p')"
  got="$(codesign -dv "$stage/bin/$bin" 2>&1 | sed -n 's/^Identifier=//p')"
  [[ "$got" == "$ident" ]] || { echo "error: $bin identifier is $got, want $ident" >&2; fail=1; }
  case "$kind" in
    self-signed)
      grep -Eqi "certificate (root|leaf) = H\"$id\"" <<<"$dr" || { echo "error: $bin DR does not pin certificate $id: $dr" >&2; fail=1; } ;;
    developer-id)
      grep -q "anchor apple generic" <<<"$dr" || { echo "error: $bin DR is not a Developer ID DR: $dr" >&2; fail=1; } ;;
  esac
  [[ "$kind" != adhoc ]] && grep -q cdhash <<<"$dr" && { echo "error: $bin DR contains a cdhash: $dr" >&2; fail=1; }
  echo "$bin: $got ($kind) DR: $dr"
done
[[ $fail == 0 ]] || exit 1

# Only --version and --print-plist: neither starts a daemon nor touches launchd.
"$stage/bin/git-strata" --version | grep -q "$version" || { echo "error: git-strata --version is not $version" >&2; exit 1; }
"$stage/bin/git-stratad" --version | grep -q "git-stratad $version" || { echo "error: git-stratad --version is not $version" >&2; exit 1; }
"$stage/bin/git-stratad" --print-plist /opt/homebrew/opt/git-strata/bin/git-stratad | plutil -lint - > /dev/null

tarball="$out/$name.tar.gz"
COPYFILE_DISABLE=1 tar -C "$(dirname "$stage")" -czf "$tarball" "$name"
rm -rf "$(dirname "$stage")"
echo "$tarball"

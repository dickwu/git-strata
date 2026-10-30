#!/usr/bin/env bash
# Render the dickwu/homebrew-tap formula and cask for a release.
# usage: render-tap.sh <version> <tarball sha256> <dmg sha256> <out dir>
# Writes <out>/Formula/git-strata.rb and <out>/Casks/git-strata-desktop.rb from
# packaging/homebrew/*.rb.in. Used by .github/workflows/homebrew.yml; by hand,
# pass a homebrew-tap checkout as <out dir>, then review and commit.
set -euo pipefail
version="$1"; tar_sha="$2"; dmg_sha="$3"; out="$4"
root="$(cd "$(dirname "$0")/../.." && pwd)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$ ]] || { echo "error: bad version $version" >&2; exit 1; }
for s in "$tar_sha" "$dmg_sha"; do
  [[ "$s" =~ ^[0-9a-f]{64}$ ]] || { echo "error: bad sha256 $s" >&2; exit 1; }
done
render() { # template output sha
  mkdir -p "$(dirname "$2")"
  sed -e "s/@VERSION@/$version/g" -e "s/@TARBALL_SHA256@/$3/g" -e "s/@DMG_SHA256@/$3/g" "$1" > "$2"
  if grep -q '@[A-Z0-9_]*@' "$2"; then echo "error: unrendered placeholder in $2" >&2; exit 1; fi
}
render "$root/packaging/homebrew/git-strata.rb.in" "$out/Formula/git-strata.rb" "$tar_sha"
render "$root/packaging/homebrew/git-strata-desktop.rb.in" "$out/Casks/git-strata-desktop.rb" "$dmg_sha"
echo "rendered git-strata $version into $out"

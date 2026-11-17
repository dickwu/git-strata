#!/usr/bin/env bash
# Package the signed (and, when applicable, stapled) app for the Tauri updater.
# usage: updater-artifacts.sh <GIT STRATA.app> <version> <dist>
set -euo pipefail
set +x

fail() { echo "error: $*" >&2; exit 1; }

[[ $# == 3 ]] || fail "usage: updater-artifacts.sh <GIT STRATA.app> <version> <dist>"
[[ "${GITHUB_ACTIONS:-}" == true ]] || fail "updater signing requires GitHub Actions"
case "${GITHUB_EVENT_NAME:-}:${GITHUB_REF_TYPE:-}" in
  push:tag|workflow_dispatch:*) ;;
  *) fail "updater signing requires a release tag push or workflow_dispatch" ;;
esac
repository="${GITHUB_REPOSITORY:-}"
[[ -n "$repository" ]] || fail "GITHUB_REPOSITORY is required for updater signing"

missing_secret=""
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  missing_secret=TAURI_SIGNING_PRIVATE_KEY
elif [[ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]]; then
  missing_secret=TAURI_SIGNING_PRIVATE_KEY_PASSWORD
fi
if [[ -n "$missing_secret" ]]; then
  if [[ "$repository" == dickwu/git-strata && "$GITHUB_EVENT_NAME" == push ]]; then
    fail "$missing_secret is required for a dickwu/git-strata release tag"
  fi
  echo "Skipping updater artifacts: $missing_secret is unset."
  exit 0
fi

# Only the signer receives the credentials. The archive and extracted daemon never inherit them.
declare +x signing_key="$TAURI_SIGNING_PRIVATE_KEY" signing_password="$TAURI_SIGNING_PRIVATE_KEY_PASSWORD"
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD TAURI_SIGNING_PRIVATE_KEY_PATH
unset TAURI_PRIVATE_KEY TAURI_PRIVATE_KEY_PASSWORD TAURI_PRIVATE_KEY_PATH

app="${1%/}"
version="$2"
dist="$3"
root="$(cd "$(dirname "$0")/../.." && pwd)"
version_pattern='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
[[ -d "$app" && "${app##*/}" == "GIT STRATA.app" ]] || fail "expected a GIT STRATA.app bundle"
[[ "$version" =~ $version_pattern ]] || fail "expected a release version (X.Y.Z)"
command -v minisign >/dev/null || fail "minisign is required to verify the updater signature"
archive="Git-Strata_${version}_arm64.app.tar.gz"
mkdir -p "$dist"
dist="$(cd "$dist" && pwd)"
stage="$(mktemp -d "$dist/.updater.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
updater_pubkey="$(jq -er '.plugins.updater.pubkey | strings | select(test("\\A[A-Za-z0-9+/]+={0,2}\\z"))' \
  "$root/apps/desktop/src-tauri/tauri.conf.json")" ||
  fail "plugins.updater.pubkey must contain the Base64-encoded Minisign public key"
if ! printf '%s' "$updater_pubkey" | base64 --decode > "$stage/updater.pub" ||
    [[ "$updater_pubkey" != "$(base64 < "$stage/updater.pub")" ]]; then
  fail "plugins.updater.pubkey must contain the Base64-encoded Minisign public key"
fi

COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --no-acls -czf "$stage/$archive" \
  -C "$(dirname "$app")" "GIT STRATA.app"

if ! (cd "$root/apps/desktop" &&
    TAURI_SIGNING_PRIVATE_KEY="$signing_key" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$signing_password" \
      bun tauri signer sign --app-version "$version" "$stage/$archive") \
    </dev/null >/dev/null 2>&1; then
  fail "Tauri could not sign the updater archive. Check the CLI version and updater signing secrets."
fi
unset signing_key signing_password
base64 --decode < "$stage/$archive.sig" > "$stage/archive.minisig" ||
  fail "could not decode the Tauri updater signature"
trusted_comment="$(minisign -V -Q -m "$stage/$archive" -x "$stage/archive.minisig" -p "$stage/updater.pub")" ||
  fail "updater signature does not verify against plugins.updater.pubkey"
[[ "$trusted_comment" == *$'\t'"version:$version" ]] ||
  fail "updater signature does not carry version:$version"

# The updater drops the first path component and uses the extraction directory as the app root.
unpacked="$stage/extracted/GIT STRATA.app"
mkdir -p "$unpacked" "$stage/home"
tar --no-mac-metadata --no-xattrs --no-acls -xzf "$stage/$archive" -C "$unpacked" --strip-components 1
codesign --verify --deep --strict "$unpacked"
info="$unpacked/Contents/Info.plist"
bundle_version="$(plutil -extract CFBundleShortVersionString raw -expect string "$info")"
[[ "$bundle_version" == "$version" ]] || fail "expected bundle version $version, got $bundle_version"
store_schema="$(plutil -extract GitStrataStoreSchema raw -expect integer "$info")"
downgrade_floor="$(plutil -extract GitStrataDowngradeFloor raw -expect string "$info")"
[[ "$downgrade_floor" =~ $version_pattern ]] || fail "expected a downgrade floor version (X.Y.Z)"
if ! jq -en --arg floor "$downgrade_floor" --arg version "$bundle_version" '
  def version_key: split(".") | map([length, .]);
  ($floor | version_key) <= ($version | version_key)
' >/dev/null; then
  fail "downgrade floor $downgrade_floor exceeds bundle version $bundle_version"
fi
[[ -f "$unpacked/Contents/Resources/config.toml" ]] || fail "config.toml is missing from the updater bundle"
daemon_version="$(env -i HOME="$stage/home" "$unpacked/Contents/MacOS/git-stratad" --version)"
[[ "$daemon_version" == "git-stratad $version" ]] || fail "expected git-stratad $version, got $daemon_version"

notes=""
notes_file="$root/docs/release-notes/v$version.md"
if [[ -f "$notes_file" ]]; then
  notes="$(awk '
    /^[[:space:]]*$/ || /^#/ { if (paragraph) exit; next }
    { print; paragraph = 1 }
  ' "$notes_file")"
fi
if [[ -z "$notes" && "$repository" == dickwu/git-strata && "$GITHUB_EVENT_NAME" == push ]]; then
  echo "::warning file=docs/release-notes/v$version.md::No opening release-note paragraph for v$version. The updater feed will have empty notes."
fi
releases="${GITHUB_SERVER_URL:-https://github.com}/$repository/releases"
jq -n --arg version "$version" --arg notes "$notes" --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg url "$releases/download/v$version/$archive" --rawfile signature "$stage/$archive.sig" \
  --argjson schema "$store_schema" --arg notes_url "$releases/tag/v$version" '
  {
    version: $version, notes: $notes, pub_date: $date,
    platforms: {"darwin-aarch64": {signature: $signature, url: $url}},
    git_strata: {store_schema: $schema, notes_url: $notes_url, min_updatable_from: "0.3.0"}
  }
' > "$stage/latest.json"

# Preserve the CLI and DMG checksums and replace the updater rows on a repeat run.
{
  if [[ -f "$dist/SHA256SUMS.txt" ]]; then
    awk -v archive="$archive" '$2 != archive && $2 != archive ".sig" && $2 != "latest.json"' "$dist/SHA256SUMS.txt"
  fi
  (cd "$stage" && shasum -a 256 -- "$archive" "$archive.sig" latest.json)
} > "$stage/SHA256SUMS.txt"
mv -f "$stage/$archive" "$stage/$archive.sig" "$stage/latest.json" "$stage/SHA256SUMS.txt" "$dist/"

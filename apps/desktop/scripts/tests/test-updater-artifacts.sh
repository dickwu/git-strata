#!/usr/bin/env bash
# macOS test with the project Tauri CLI, minisign and a disposable signed app.
# The updater key and all artifacts are made in a temp directory and removed on exit.
set -euo pipefail
set +x
umask 077
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD TAURI_SIGNING_PRIVATE_KEY_PATH
unset TAURI_PRIVATE_KEY TAURI_PRIVATE_KEY_PASSWORD TAURI_PRIVATE_KEY_PATH

root="$(cd "$(dirname "$0")/../../../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
expect_failure() {
  local diagnostic="$1"
  shift
  if "$@" > "$work/rejected.log" 2>&1; then
    fail "expected rejection with message containing $diagnostic"
  fi
  grep -Fq -- "$diagnostic" "$work/rejected.log" || fail "rejection did not report $diagnostic"
}
command -v minisign >/dev/null || fail "minisign is required to verify the test signature"

version=0.3.1
test_repo="$work/repo"
desktop="$test_repo/apps/desktop"
mkdir -p "$test_repo/scripts/release" "$desktop/src-tauri" "$test_repo/docs/release-notes"
cp "$root/scripts/release/updater-artifacts.sh" "$test_repo/scripts/release/"
cp "$root/apps/desktop/package.json" "$desktop/"
ln -s "$root/apps/desktop/node_modules" "$desktop/node_modules"
script="$test_repo/scripts/release/updater-artifacts.sh"
app="$work/build output/GIT STRATA.app"
dist="$work/dist output"

run_release() {
  PATH="$work/bin:$PATH" GITHUB_ACTIONS="$1" GITHUB_REPOSITORY="$2" GITHUB_EVENT_NAME="$3" \
    GITHUB_SERVER_URL=https://github.com GITHUB_REF_TYPE=tag GITHUB_REF_NAME="v$version" \
    bash "$script" "$app" "$version" "$dist"
}
without_key() {
  TAURI_SIGNING_PRIVATE_KEY='' TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' run_release true "$@"
}
expect_failure 'TAURI_SIGNING_PRIVATE_KEY is required' without_key dickwu/git-strata push
without_key dickwu/git-strata workflow_dispatch
without_key someone/git-strata push
[[ ! -e "$dist" ]] || fail "a skipped run wrote release artifacts"

notes_file="$test_repo/docs/release-notes/v$version.md"
cat > "$notes_file" <<'NOTES'
# Release title

## Upgrade first

The "Upgrade" button updates the bundle.
The app includes both binaries.

Only the first paragraph belongs in the manifest.
NOTES

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work/bin"
info="$app/Contents/Info.plist"
plutil -create xml1 "$info"
plutil -insert CFBundleIdentifier -string io.github.dickwu.git-strata.updater-test "$info"
plutil -insert CFBundleExecutable -string git-strata-desktop "$info"
plutil -insert CFBundlePackageType -string APPL "$info"
plutil -insert CFBundleShortVersionString -string "$version" "$info"
plutil -insert GitStrataStoreSchema -integer 7 "$info"
plutil -insert GitStrataDowngradeFloor -string 0.3.0 "$info"
cp "$root/examples/config.toml" "$app/Contents/Resources/config.toml"

build_version_probe() {
  cc -DTEST_VERSION="\"$1\"" -x c -o "$app/Contents/MacOS/git-stratad" - <<'C'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, const char *argv[]) {
    if (argc != 2 || strcmp(argv[1], "--version") != 0) return 1;
    if (getenv("TAURI_SIGNING_PRIVATE_KEY") || getenv("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")) {
        fputs("daemon version probe inherited updater signing credentials\n", stderr);
        return 1;
    }
    puts("git-stratad " TEST_VERSION);
    return 0;
}
C
}
sign_fixture() {
  STRATA_SIGN_KIND=adhoc STRATA_SIGN_KEYCHAIN='' "$root/apps/desktop/pack/sign-app.sh" "$app" -
}
build_version_probe "$version"
cp "$app/Contents/MacOS/git-stratad" "$app/Contents/MacOS/git-strata"
cp "$app/Contents/MacOS/git-stratad" "$app/Contents/MacOS/git-strata-desktop"
sign_fixture
xattr -w io.github.dickwu.git-strata.test excluded "$app/Contents/Resources/config.toml"

cat > "$work/bin/tar" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
[[ -z "${TAURI_SIGNING_PRIVATE_KEY+x}" && -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD+x}" ]]
if [[ " $* " == *" -czf "* ]]; then
  [[ "${COPYFILE_DISABLE:-}" == 1 ]]
  for flag in --no-mac-metadata --no-xattrs --no-acls; do
    [[ " $* " == *" $flag "* ]] || { echo "tar lacks $flag" >&2; exit 1; }
  done
  touch "$UPDATER_TEST_TAR_LOG"
fi
exec /usr/bin/tar "$@"
SHIM
chmod +x "$work/bin/tar"
export UPDATER_TEST_TAR_LOG="$work/tar-called"

fixture_password='disposable test password'
generate_key() {
  if ! (cd "$desktop" &&
      bun tauri signer generate --ci --password "$fixture_password" --write-keys "$1") \
      </dev/null >/dev/null 2>&1; then
    fail "could not generate a temporary updater key"
  fi
}
generate_key "$work/updater.key"
generate_key "$work/other.key"

run_updater() {
  TAURI_SIGNING_PRIVATE_KEY="$(< "$1")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$fixture_password" \
    run_release "${2:-true}" dickwu/git-strata "${3:-push}"
}

TAURI_SIGNING_PRIVATE_KEY="$(< "$work/updater.key")" TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' \
  expect_failure 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD is required' run_release true dickwu/git-strata push
expect_failure 'requires GitHub Actions' run_updater "$work/updater.key" false
expect_failure 'requires a release tag push or workflow_dispatch' run_updater "$work/updater.key" true pull_request
[[ ! -e "$dist" ]] || fail "a rejected signing context wrote artifacts"

name="Git-Strata_${version}_arm64.app.tar.gz"
archive="$dist/$name"
config="$desktop/src-tauri/tauri.conf.json"
# The placeholder is invalid and AB== is a non-canonical encoding of a zero byte.
for invalid_pubkey in REPLACE_WITH_REAL_UPDATER_PUBKEY_T13 'AB=='; do
  jq -n --arg pubkey "$invalid_pubkey" '{plugins: {updater: {pubkey: $pubkey}}}' > "$config"
  expect_failure 'plugins.updater.pubkey must contain the Base64-encoded Minisign public key' \
    run_updater "$work/updater.key"
done
[[ ! -e "$UPDATER_TEST_TAR_LOG" ]] || fail "the updater packaged a bundle with an invalid public key"

jq -n --rawfile pubkey "$work/updater.key.pub" '{plugins: {updater: {pubkey: $pubkey}}}' > "$config"
expect_failure 'does not verify against plugins.updater.pubkey' run_updater "$work/other.key"
[[ ! -e "$archive" && ! -e "$archive.sig" && ! -e "$dist/latest.json" && ! -e "$dist/SHA256SUMS.txt" ]] ||
  fail "an invalid updater key produced release artifacts"

mkdir -p "$dist"
printf 'CLI fixture\n' > "$dist/cli.tar.gz"
printf 'DMG fixture\n' > "$dist/app.dmg"
(cd "$dist" && shasum -a 256 -- cli.tar.gz app.dmg > SHA256SUMS.txt)
run_updater "$work/updater.key"
[[ -f "$UPDATER_TEST_TAR_LOG" ]] || fail "archive creation did not call tar"

listing="$(/usr/bin/tar -tzf "$archive")"
grep -Fxq 'GIT STRATA.app/Contents/MacOS/git-stratad' <<< "$listing" || fail "archive is missing git-stratad"
grep -Fxq 'GIT STRATA.app/Contents/Resources/config.toml' <<< "$listing" || fail "archive is missing config.toml"
awk '!/^GIT STRATA\.app\// || /(^|\/)\._/ { exit 1 }' <<< "$listing" ||
  fail "archive contains a path outside the app or an AppleDouble entry"

base64 --decode < "$work/updater.key.pub" > "$work/minisign.pub"
base64 --decode < "$archive.sig" > "$work/archive.minisig"
trusted_comment="$(minisign -V -Q -m "$archive" -x "$work/archive.minisig" -p "$work/minisign.pub")"
[[ "$trusted_comment" == *$'\t'"version:$version" ]] || fail "signature did not bind the version"

notes=$'The "Upgrade" button updates the bundle.\nThe app includes both binaries.'
jq -e --arg version "$version" --arg notes "$notes" --arg name "$name" --rawfile sig "$archive.sig" '
  .version == $version and .notes == $notes
  and (.pub_date | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
  and .platforms == {"darwin-aarch64": {
    signature: $sig,
    url: ("https://github.com/dickwu/git-strata/releases/download/v" + $version + "/" + $name)
  }}
  and .git_strata == {
    store_schema: 7, min_updatable_from: "0.3.0",
    notes_url: ("https://github.com/dickwu/git-strata/releases/tag/v" + $version)
  }
' "$dist/latest.json" >/dev/null || fail "latest.json did not match the updater manifest contract"

mv "$notes_file" "$work/notes.md"
run_updater "$work/updater.key" > "$work/missing-notes.log"
grep -Fq "No opening release-note paragraph for v$version" "$work/missing-notes.log" ||
  fail "an upstream tag with missing release notes did not warn"
jq -e '.notes == ""' "$dist/latest.json" >/dev/null || fail "missing release notes must yield an empty notes string"
mv "$work/notes.md" "$notes_file"

# A repeat replaces the updater rows and preserves the two Homebrew rows.
run_updater "$work/updater.key"
[[ "$(wc -l < "$dist/SHA256SUMS.txt" | tr -d ' ')" == 5 ]] || fail "unexpected checksum rows"
(cd "$dist" && shasum -a 256 -c SHA256SUMS.txt)
cp "$dist/SHA256SUMS.txt" "$work/verified-checksums.txt"

# Reject a change to the signed bundle and leave the verified output intact.
printf '\n# changed after signing\n' >> "$app/Contents/Resources/config.toml"
if run_updater "$work/updater.key" > "$work/bad-app.log" 2>&1; then
  fail "the updater accepted an invalid code signature"
fi
grep -Fq 'code or signature modified' "$work/bad-app.log" ||
  grep -Fq 'sealed resource is missing or invalid' "$work/bad-app.log" ||
  fail "the modified bundle did not fail code-signature verification"

# A signed bundle must not advertise a floor newer than its own release.
plutil -replace GitStrataDowngradeFloor -string 0.3.10 "$info"
sign_fixture
expect_failure 'downgrade floor 0.3.10 exceeds bundle version' run_updater "$work/updater.key"
plutil -replace GitStrataDowngradeFloor -string 0.3.0 "$info"

wrong_version=0.3.2
plutil -replace CFBundleShortVersionString -string "$wrong_version" "$info"
sign_fixture
expect_failure "expected bundle version $version, got $wrong_version" run_updater "$work/updater.key"
plutil -replace CFBundleShortVersionString -string "$version" "$info"

mv "$app/Contents/Resources/config.toml" "$work/config.toml"
sign_fixture
expect_failure 'config.toml is missing from the updater bundle' run_updater "$work/updater.key"
mv "$work/config.toml" "$app/Contents/Resources/config.toml"

build_version_probe "$wrong_version"
sign_fixture
expect_failure "expected git-stratad $version, got git-stratad $wrong_version" run_updater "$work/updater.key"

cmp "$work/verified-checksums.txt" "$dist/SHA256SUMS.txt" || fail "a rejected bundle replaced the verified checksums"
(cd "$dist" && shasum -a 256 -c SHA256SUMS.txt)

echo "ok: updater artifacts (CI policy, archive flags, signature, manifest, checksums and extracted bundle)"

#!/usr/bin/env bash
# CI only: import the self-signed identity from secrets into a throwaway
# keychain and export STRATA_SIGN_ID (the certificate SHA-1) to GITHUB_ENV.
# Needs STRATA_CODESIGN_P12 (base64) and STRATA_CODESIGN_P12_PASSWORD.
# If STRATA_CODESIGN_CERT_SHA1 is set, the imported certificate must match it.
set -euo pipefail
: "${STRATA_CODESIGN_P12:?secret STRATA_CODESIGN_P12 is not set}"
: "${STRATA_CODESIGN_P12_PASSWORD:?secret STRATA_CODESIGN_P12_PASSWORD is not set}"
tmp="${RUNNER_TEMP:-$(mktemp -d)}"
keychain="$tmp/strata-signing.keychain-db"
kc_pass="$(uuidgen)"

security create-keychain -p "$kc_pass" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$kc_pass" "$keychain"
printf '%s' "$STRATA_CODESIGN_P12" | base64 --decode > "$tmp/id.p12"
security import "$tmp/id.p12" -k "$keychain" -f pkcs12 \
  -P "$STRATA_CODESIGN_P12_PASSWORD" -T /usr/bin/codesign
rm -f "$tmp/id.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kc_pass" "$keychain" > /dev/null
# Not added to the search list: sign-app.sh passes --keychain "$STRATA_SIGN_KEYCHAIN".

# No -v: a self-signed certificate is not trusted, and that is fine for codesign.
sha1="$(security find-identity -p codesigning "$keychain" | awk '/^ *1\)/ {print tolower($2); exit}')"
if [[ -z "$sha1" ]]; then
  security find-identity -p codesigning "$keychain" >&2
  echo "error: no code-signing identity in the imported p12" >&2
  exit 1
fi
if [[ -n "${STRATA_CODESIGN_CERT_SHA1:-}" && "$sha1" != "$(tr 'A-F' 'a-f' <<<"$STRATA_CODESIGN_CERT_SHA1")" ]]; then
  echo "error: imported certificate $sha1 is not STRATA_CODESIGN_CERT_SHA1; refusing to sign with an identity that changes the DR" >&2
  exit 1
fi
echo "Imported signing identity $sha1"
{
  echo "STRATA_SIGN_ID=$sha1"
  echo "STRATA_SIGN_KEYCHAIN=$keychain"
} >> "${GITHUB_ENV:-/dev/null}"

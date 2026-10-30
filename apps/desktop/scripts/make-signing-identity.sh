#!/usr/bin/env bash
# Generate the project's long-lived self-signed code-signing identity
# (gui.md §6 "打包与签名", D3, Q-cert decided 10-27: the identity lives ONLY in
# GitHub Actions secrets, with no local backup). Run once, by peilin, on his Mac.
#
# What it does:
#   1. Creates a private temp dir (mode 700) and generates the key, certificate
#      and .p12 there. Nothing is written anywhere else.
#   2. Checks the .p12 imports as a code-signing identity into a throwaway
#      keychain inside that temp dir. That keychain is never added to the search
#      list and never becomes the default; the login keychain is never touched.
#      The script fails if the user search list or default keychain changed.
#   3. Prints the `gh secret set` / `gh variable set` commands. It uploads nothing.
#   4. Asks whether the upload is done, lists everything it will remove, asks
#      y/N, then deletes the throwaway keychain (security delete-keychain) and
#      overwrites and deletes the .p12, its base64, the password and the
#      certificate. (The bare key.pem is scrubbed as soon as the .p12 exists.)
#      Answering N leaves them and prints where they are and how to remove them.
#
# Losing the secret means a new identity: the DR changes and every user grants
# TCC permissions again. That is the accepted cost of keeping no local copy.
#
# Environment (all optional):
#   STRATA_SIGNING_CN    certificate common name (default "Git Strata Self-Signed")
#   STRATA_SIGNING_DAYS  validity in days (default 3650)
#   STRATA_GH_REPO       repo for the printed gh commands (default dickwu/git-strata)
#   STRATA_SECURITY      path to the `security` tool (tests use a shim)
set -euo pipefail

CN="${STRATA_SIGNING_CN:-Git Strata Self-Signed}"
DAYS="${STRATA_SIGNING_DAYS:-3650}"
REPO="${STRATA_GH_REPO:-dickwu/git-strata}"
SECURITY="${STRATA_SECURITY:-/usr/bin/security}"

umask 077
work="$(mktemp -d "${TMPDIR:-/tmp}/git-strata-signing.XXXXXX")"
chmod 700 "$work"
keychain="$work/verify.keychain-db"
kc_made=0
done_msg=""

# Overwrite then unlink. On APFS/SSD an overwrite cannot guarantee the old
# blocks are gone (copy-on-write, wear levelling); FileVault covers the rest.
scrub() {
  local f
  for f in "$@"; do
    [[ -f "$f" ]] || continue
    local n
    n="$(wc -c < "$f" | tr -d ' ')"
    if [[ "$n" -gt 0 ]]; then
      dd if=/dev/urandom of="$f" bs="$n" count=1 conv=notrunc 2>/dev/null || true
    fi
    rm -f "$f"
  done
}

ask() { # ask "question" → 0 on y/yes
  local a
  printf '%s [y/N] ' "$1" >&2
  read -r a || a=""
  [[ "$a" == y || "$a" == Y || "$a" == yes ]]
}

cleanup_prompt() {
  echo >&2
  echo "This will remove:" >&2
  if [[ "$kc_made" == 1 ]]; then
    echo "  $keychain  (throwaway keychain, via security delete-keychain)" >&2
  fi
  find "$work" -type f ! -name 'verify.keychain-db' | sort | sed 's/^/  /' >&2
  echo "  $work/  (the directory)" >&2
  if ask "Delete these now?"; then
    if [[ "$kc_made" == 1 ]]; then
      "$SECURITY" delete-keychain "$keychain" 2>/dev/null || true
      kc_made=0
    fi
    # shellcheck disable=SC2046
    while IFS= read -r -d '' f; do scrub "$f"; done < <(find "$work" -type f -print0)
    rm -rf "$work"
    echo "Removed. ${done_msg:-}" >&2
  else
    echo "Left in place: $work" >&2
    echo "Remove it yourself when done: security delete-keychain \"$keychain\"; rm -P -rf \"$work\"" >&2
  fi
}

on_exit() {
  local rc=$?
  if [[ -d "$work" && "$rc" -ne 0 ]]; then
    echo "error: stopped before finishing (exit $rc). Nothing was uploaded by this script." >&2
    cleanup_prompt || true
  fi
}
trap on_exit EXIT

search_before="$("$SECURITY" list-keychains -d user)"
default_before="$("$SECURITY" default-keychain -d user)"

cat > "$work/req.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $CN
O = git-strata
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
CNF

openssl req -x509 -newkey rsa:3072 -sha256 -nodes -days "$DAYS" \
  -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/req.cnf" 2>/dev/null

pass="$(openssl rand -base64 24 | tr -d '/+=' | cut -c1-24)"
# macOS `security import` cannot read OpenSSL 3's default PKCS#12 (AES/PBKDF2);
# -legacy (OpenSSL 3) or the system LibreSSL default (3DES/RC2) both work.
if openssl pkcs12 -help 2>&1 | grep -q -- '-legacy'; then legacy=(-legacy); else legacy=(); fi
openssl pkcs12 -export "${legacy[@]}" -name "$CN" \
  -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/identity.p12" -passout "pass:$pass"
scrub "$work/key.pem" # the .p12 carries it from here on

base64 < "$work/identity.p12" | tr -d '\n' > "$work/identity.p12.b64"
printf '%s' "$pass" > "$work/p12-password.txt"
unset pass
sha1="$(openssl x509 -in "$work/cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g' | tr 'A-F' 'a-f')"

# Self-check in a throwaway keychain that only exists inside $work.
kc_pass="$(openssl rand -hex 16)"
"$SECURITY" create-keychain -p "$kc_pass" "$keychain"
kc_made=1
"$SECURITY" unlock-keychain -p "$kc_pass" "$keychain"
"$SECURITY" import "$work/identity.p12" -k "$keychain" -f pkcs12 \
  -P "$(cat "$work/p12-password.txt")" -T /usr/bin/codesign > /dev/null
got="$("$SECURITY" find-identity -p codesigning "$keychain" | awk '/^ *1\)/ {print tolower($2); exit}')"
if [[ "$got" != "$sha1" ]]; then
  echo "error: the .p12 did not import as code-signing identity $sha1 (got '${got:-none}')" >&2
  exit 1
fi
if [[ "$("$SECURITY" list-keychains -d user)" != "$search_before" ]]; then
  echo "error: the user keychain search list changed; check it with: security list-keychains -d user" >&2
  exit 1
fi
if [[ "$("$SECURITY" default-keychain -d user)" != "$default_before" ]]; then
  echo "error: the default keychain changed; check it with: security default-keychain -d user" >&2
  exit 1
fi

cat <<MSG
Generated in $work (mode 700). Certificate SHA-1: $sha1
The .p12 imported cleanly into a throwaway keychain inside that dir (not on the
search list, not the default); your login keychain and search list are unchanged.

Upload it (run these yourself, in another terminal):
  gh secret set STRATA_CODESIGN_P12 --repo $REPO < "$work/identity.p12.b64"
  gh secret set STRATA_CODESIGN_P12_PASSWORD --repo $REPO < "$work/p12-password.txt"
  gh variable set STRATA_CODESIGN_CERT_SHA1 --repo $REPO --body $sha1

Check:
  gh secret list --repo $REPO      # both STRATA_CODESIGN_* listed, updated just now
  gh variable list --repo $REPO    # STRATA_CODESIGN_CERT_SHA1 = $sha1

There is no local backup by design (gui.md §9 Q-cert). If the secret is lost,
the next release needs a new identity and every user grants permissions again.
MSG

if ask "Have the two secrets and the variable been uploaded and checked?"; then
  done_msg="The identity now exists only in the GitHub secrets."
  cleanup_prompt
else
  echo "Not confirmed. If you delete now, this identity is gone and you can simply run the script again." >&2
  cleanup_prompt
fi
trap - EXIT

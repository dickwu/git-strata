#!/usr/bin/env bash
# Generate the project's long-lived self-signed code-signing identity
# (gui.md §6 "打包与签名", D3). Run once, by the key holder, on their own Mac.
#
# Output (default ~/.git-strata-signing/, never inside the repo):
#   git-strata-signing.p12      private key + certificate (secret)
#   git-strata-signing.p12.b64  the same, base64, for STRATA_CODESIGN_P12
#   p12-password.txt            for STRATA_CODESIGN_P12_PASSWORD (secret)
#   git-strata-signing.cer.pem  public certificate (safe to share)
#   cert-sha1.txt               its SHA-1 = the `certificate root = H"…"` in the DR
#
# It uploads nothing. It prints the `gh secret set` commands to run by hand.
# Builders from source can use their own identity the same way; their builds
# simply have a different DR, so TCC treats them as a different program.
set -euo pipefail

CN="${STRATA_SIGNING_CN:-Git Strata Self-Signed}"
DAYS="${STRATA_SIGNING_DAYS:-3650}"
OUT="${1:-$HOME/.git-strata-signing}"

if [[ -e "$OUT/git-strata-signing.p12" ]]; then
  echo "error: $OUT/git-strata-signing.p12 exists; a new identity changes the DR and every user must grant TCC again. Move it away first if you really mean to rotate." >&2
  exit 1
fi
umask 077
mkdir -p "$OUT"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

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
  -keyout "$work/key.pem" -out "$OUT/git-strata-signing.cer.pem" -config "$work/req.cnf"

pass="$(openssl rand -base64 24 | tr -d '/+=' | cut -c1-24)"
# macOS `security import` cannot read OpenSSL 3's default PKCS#12 (AES/PBKDF2);
# -legacy (OpenSSL 3) or the system LibreSSL default (3DES/RC2) both work.
if openssl pkcs12 -help 2>&1 | grep -q -- '-legacy'; then legacy=(-legacy); else legacy=(); fi
openssl pkcs12 -export "${legacy[@]}" -name "$CN" \
  -inkey "$work/key.pem" -in "$OUT/git-strata-signing.cer.pem" \
  -out "$OUT/git-strata-signing.p12" -passout "pass:$pass"

base64 < "$OUT/git-strata-signing.p12" | tr -d '\n' > "$OUT/git-strata-signing.p12.b64"
printf '%s' "$pass" > "$OUT/p12-password.txt"
openssl x509 -in "$OUT/git-strata-signing.cer.pem" -noout -fingerprint -sha1 \
  | sed 's/.*=//; s/://g' | tr 'A-F' 'a-f' > "$OUT/cert-sha1.txt"

cat <<MSG
Wrote $OUT (mode 700). Certificate SHA-1: $(cat "$OUT/cert-sha1.txt")

To use it in GitHub Actions (run these yourself):
  gh secret set STRATA_CODESIGN_P12          < "$OUT/git-strata-signing.p12.b64"
  gh secret set STRATA_CODESIGN_P12_PASSWORD < "$OUT/p12-password.txt"
  gh variable set STRATA_CODESIGN_CERT_SHA1 --body "$(cat "$OUT/cert-sha1.txt")"

Keep a backup of the .p12 and its password offline (gui.md §9 Q-cert): losing it
means the next release has a new DR and every user grants TCC again.
MSG

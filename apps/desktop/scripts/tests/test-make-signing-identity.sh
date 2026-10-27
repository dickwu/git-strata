#!/usr/bin/env bash
# Exercises make-signing-identity.sh without touching any real keychain:
# `security` is replaced by a shim that logs every call and fakes the replies.
# openssl runs for real (throwaway key in a temp dir). Run: bash this-file
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
script="$here/../make-signing-identity.sh"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

cat > "$root/security" <<'SHIM'
#!/usr/bin/env bash
# Fake `security`. State in $SHIM_DIR.
echo "$*" >> "$SHIM_DIR/calls.log"
case "$1" in
  list-keychains)
    [[ "$*" == "list-keychains -d user" ]] || { echo "shim: refusing $*" >&2; exit 9; }
    cat "$SHIM_DIR/search" ;;
  default-keychain)
    [[ "$*" == "default-keychain -d user" ]] || { echo "shim: refusing $*" >&2; exit 9; }
    echo '    "/Users/x/Library/Keychains/login.keychain-db"' ;;
  create-keychain)
    touch "${@: -1}"
    if [[ "${SHIM_MODE:-}" == pollute ]]; then echo '    "/tmp/extra.keychain-db"' >> "$SHIM_DIR/search"; fi
    ;;
  unlock-keychain) ;;
  import)
    f="$2"; pass=""
    while [[ $# -gt 0 ]]; do [[ "$1" == -P ]] && pass="$2"; shift; done
    leg=(); openssl pkcs12 -help 2>&1 | grep -q -- -legacy && leg=(-legacy)
    openssl pkcs12 "${leg[@]}" -in "$f" -nokeys -passin "pass:$pass" 2>/dev/null \
      | openssl x509 -noout -fingerprint -sha1 | sed 's/.*=//; s/://g' > "$SHIM_DIR/sha1" ;;
  find-identity)
    if [[ "${SHIM_MODE:-}" != noid && -s "$SHIM_DIR/sha1" ]]; then
      echo "  1) $(cat "$SHIM_DIR/sha1") \"Git Strata Self-Signed\""
    fi
    echo "     1 valid identities found" ;;
  delete-keychain) rm -f "$2" ;;
  *) echo "shim: unexpected $*" >&2; exit 9 ;;
esac
SHIM
chmod +x "$root/security"

# run <name> <answers> [mode] → sets $out, $rc, $tmp, $log
run() {
  local name="$1" answers="$2" mode="${3:-}"
  export SHIM_DIR="$root/$name"; mkdir -p "$SHIM_DIR"
  echo '    "/Users/x/Library/Keychains/login.keychain-db"' > "$SHIM_DIR/search"
  tmp="$SHIM_DIR/tmp"; mkdir -p "$tmp"
  set +e
  out="$(printf '%b' "$answers" | SHIM_MODE="$mode" TMPDIR="$tmp" STRATA_SECURITY="$root/security" \
    STRATA_GH_REPO=example/repo bash "$script" 2>&1)"
  rc=$?
  set -e
  log="$SHIM_DIR/calls.log"
}

no_keychain_writes_outside() { # every keychain path the script used is under $tmp
  ! grep -Eq -- '(^| )-s( |$)|login\.keychain' "$log" || fail "$1: touched search list / login keychain: $(cat "$log")"
  grep -E '^(create-keychain|unlock-keychain|delete-keychain)' "$log" | awk '{print $NF}' | while read -r p; do
    [[ "$p" == "$tmp"/* ]] || fail "$1: keychain outside temp dir: $p"
  done
}

# 1. upload confirmed, delete confirmed → everything gone.
run ok 'y\ny\n'
[[ $rc -eq 0 ]] || fail "ok: exit $rc: $out"
for want in 'gh secret set STRATA_CODESIGN_P12 --repo example/repo <' \
            'gh secret set STRATA_CODESIGN_P12_PASSWORD --repo example/repo <' \
            'gh variable set STRATA_CODESIGN_CERT_SHA1 --repo example/repo --body ' \
            'This will remove:' 'identity.p12' 'verify.keychain-db' 'no local backup'; do
  grep -qF -- "$want" <<<"$out" || fail "ok: output lacks '$want'"
done
grep -q '^delete-keychain ' "$log" || fail "ok: throwaway keychain not deleted"
[[ -z "$(ls -A "$tmp")" ]] || fail "ok: left behind: $(ls -A "$tmp")"
grep -q 'BEGIN' <<<"$out" && fail "ok: printed key or cert material"
no_keychain_writes_outside ok
# Deletion happened only after the second answer: delete-keychain is the last call.
[[ "$(tail -1 "$log" | cut -d' ' -f1)" == delete-keychain ]] || fail "ok: keychain deleted before the prompt"

# 2. upload confirmed, delete declined → files kept, nothing deleted.
run keep 'y\nn\n'
[[ $rc -eq 0 ]] || fail "keep: exit $rc"
d="$(ls -d "$tmp"/git-strata-signing.* 2>/dev/null)" || fail "keep: work dir missing"
[[ -f "$d/identity.p12" && -f "$d/p12-password.txt" && -f "$d/identity.p12.b64" ]] || fail "keep: files missing"
[[ ! -e "$d/key.pem" ]] || fail "keep: bare key.pem still there"
[[ "$(stat -f %Lp "$d" 2>/dev/null || stat -c %a "$d")" == 700 ]] || fail "keep: dir not mode 700"
grep -q '^delete-keychain ' "$log" && fail "keep: deleted keychain despite N"
grep -qF 'Left in place' <<<"$out" || fail "keep: no 'Left in place' notice"

# 3. empty answers (EOF) default to No.
run eof ''
[[ -n "$(ls -A "$tmp")" ]] || fail "eof: deleted without a yes"

# 4. identity does not import → non-zero, cleanup still asks first.
run noid 'y\n' noid
[[ $rc -ne 0 ]] || fail "noid: should fail"
grep -qF 'did not import' <<<"$out" || fail "noid: missing error"
grep -qF 'This will remove:' <<<"$out" || fail "noid: no cleanup prompt"
[[ -z "$(ls -A "$tmp")" ]] || fail "noid: not cleaned after yes"
grep -qF 'gh secret set' <<<"$out" && fail "noid: printed upload commands for a bad identity"

# 5. search list changed behind our back → refuse.
run pollute 'y\n' pollute
[[ $rc -ne 0 ]] || fail "pollute: should fail"
grep -qF 'search list changed' <<<"$out" || fail "pollute: missing error"

echo "ok: make-signing-identity.sh (5 cases)"

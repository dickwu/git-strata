#!/usr/bin/env bash
# Exercises import-identity.sh without touching any real keychain:
# `security` is replaced by a shim that logs every call and fakes the replies.
# openssl runs for real (throwaway p12 in a temp dir). Run: bash this-file
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
script="$here/../../pack/import-identity.sh"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
unset STRATA_CODESIGN_CERT_SHA1

# A tiny disposable p12; password is local to this test and not printed.
pass="test-pass-not-a-secret"
cn="git-strata import-identity test"
work="$root/p12"; mkdir -p "$work"
cat > "$work/req.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $cn
O = git-strata-test
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
CNF
openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 1 \
  -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/req.cnf" 2>/dev/null
leg=()
openssl pkcs12 -help 2>&1 | grep -q -- -legacy && leg=(-legacy)
openssl pkcs12 -export ${leg[@]+"${leg[@]}"} -name "$cn" \
  -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/identity.p12" -passout "pass:$pass"
want_sha1="$(openssl x509 -in "$work/cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g' | tr 'A-F' 'a-f')"
p12_b64="$(base64 < "$work/identity.p12" | tr -d '\n')"
rm -f "$work/key.pem" "$work/cert.pem" "$work/identity.p12"

cat > "$root/security" <<'SHIM'
#!/usr/bin/env bash
echo "$*" >> "$SHIM_DIR/calls.log"
case "$1" in
  list-keychains)
    if [[ "$*" == "list-keychains -d user" ]]; then
      if [[ "${SHIM_MODE:-}" == listerr ]]; then
        echo "security: SecKeychainListCopy: The specified keychain could not be found." >&2
      fi
      cat "$SHIM_DIR/search"
      exit 0
    fi
    if [[ "$2" == "-d" && "$3" == "user" && "$4" == "-s" ]]; then
      shift 4
      : > "$SHIM_DIR/search"
      for p in "$@"; do
        printf '    "%s"\n' "$p" >> "$SHIM_DIR/search"
      done
      exit 0
    fi
    echo "shim: refusing $*" >&2; exit 9 ;;
  default-keychain)
    if [[ "${SHIM_MODE:-}" == chdefault && "$*" == "default-keychain -d user" ]]; then
      if [[ -f "$SHIM_DIR/default-polluted" ]]; then
        echo '    "/tmp/other.keychain-db"'
        exit 0
      fi
    fi
    [[ "$*" == "default-keychain -d user" ]] || { echo "shim: refusing $*" >&2; exit 9; }
    echo '    "/Users/x/Library/Keychains/login.keychain-db"' ;;
  create-keychain)
    touch "${@: -1}"
    if [[ "${SHIM_MODE:-}" == chdefault ]]; then
      touch "$SHIM_DIR/default-polluted"
    fi
    ;;
  set-keychain-settings|unlock-keychain) ;;
  set-key-partition-list) ;;
  import)
    f="$2"; pass=""
    while [[ $# -gt 0 ]]; do [[ "$1" == -P ]] && pass="$2"; shift; done
    leg=(); openssl pkcs12 -help 2>&1 | grep -q -- -legacy && leg=(-legacy)
    openssl pkcs12 ${leg[@]+"${leg[@]}"} -in "$f" -nokeys -passin "pass:$pass" 2>/dev/null \
      | openssl x509 -noout -fingerprint -sha1 | sed 's/.*=//; s/://g' | tr 'A-F' 'a-f' > "$SHIM_DIR/sha1" ;;
  find-identity)
    if [[ "${SHIM_MODE:-}" != noid && -s "$SHIM_DIR/sha1" ]]; then
      echo "  1) $(cat "$SHIM_DIR/sha1") \"git-strata import-identity test\""
    fi
    echo "     1 valid identities found" ;;
  delete-keychain)
    # Never unlink a path outside the shim dir (the official test must not
    # be able to rm /tmp/strata-signing.keychain-db or a login keychain).
    case "$2" in
      "$SHIM_DIR"/*) ;;
      *) echo "shim: refusing to delete $2" >&2; exit 9 ;;
    esac
    if [[ "${SHIM_MODE:-}" == nodel ]]; then
      exit 0
    fi
    rm -f "$2" ;;
  *) echo "shim: unexpected $*" >&2; exit 9 ;;
esac
SHIM
chmod +x "$root/security"

read_paths() {
  got=()
  while IFS= read -r line; do
    [[ -n "$line" ]] && got+=("$line")
  done < <(sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//' "$1" | sed '/^$/d')
}

s_count() { # how many logged calls start with $1
  grep -c "^$1" "$log" 2>/dev/null || true
}

run() { # name [mode] → sets $rc $out $log $tmp $envf $SHIM_DIR
  local name="$1" mode="${2:-}"
  export SHIM_DIR="$root/$name"; mkdir -p "$SHIM_DIR"
  printf '    "%s"\n    "%s"\n' \
    "/Users/x/Library/Keychains/login.keychain-db" \
    "/Library/Keychains/System.keychain" > "$SHIM_DIR/search"
  tmp="$SHIM_DIR/tmp"; mkdir -p "$tmp"
  envf="$SHIM_DIR/github.env"; : > "$envf"
  set +e
  out="$(PATH="$root:$PATH" SHIM_MODE="$mode" RUNNER_TEMP="$tmp" GITHUB_ENV="$envf" \
    STRATA_SECURITY="$root/security" \
    STRATA_CODESIGN_P12="$p12_b64" \
    STRATA_CODESIGN_P12_PASSWORD="$pass" \
    STRATA_CODESIGN_CERT_SHA1="${STRATA_CODESIGN_CERT_SHA1:-}" \
    bash "$script" 2>&1)"
  rc=$?
  set -e
  log="$SHIM_DIR/calls.log"
}

search_paths() {
  sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//' "$SHIM_DIR/search" | sed '/^$/d'
}

# 1. happy path: added to search list, env exported, list not restored, default unchanged.
run ok
[[ $rc -eq 0 ]] || fail "ok: exit $rc: $out"
grep -qF "Imported signing identity $want_sha1" <<<"$out" || fail "ok: missing import line: $out"
grep -q "^STRATA_SIGN_ID=$want_sha1$" "$envf" || fail "ok: bad STRATA_SIGN_ID"
kc="$(sed -n 's/^STRATA_SIGN_KEYCHAIN=//p' "$envf")"
backup="$(sed -n 's/^STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP=//p' "$envf")"
[[ "$kc" == "$tmp/strata-signing.keychain-db" ]] || fail "ok: unexpected keychain path: $kc"
[[ -f "$backup" ]] || fail "ok: backup file missing"
grep -q 'list-keychains -d user -s ' "$log" || fail "ok: did not set search list"
read_paths "$SHIM_DIR/search"
[[ "${got[0]}" == "$kc" ]] || fail "ok: throwaway keychain not prepended: ${got[*]}"
[[ "${got[1]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] || fail "ok: login not preserved"
[[ "${got[2]}" == "/Library/Keychains/System.keychain" ]] || fail "ok: system not preserved"
grep -q '^delete-keychain ' "$log" && fail "ok: deleted keychain on success"
[[ -f "$kc" ]] || fail "ok: throwaway keychain missing after success"
grep -q '^default-keychain -s' "$log" && fail "ok: changed default keychain"
grep -E '^import ' "$log" | grep -q 'login\.keychain' && fail "ok: imported into login keychain"

# --restore puts the original list back and deletes the keychain.
set +e
rout="$(PATH="$root:$PATH" STRATA_SECURITY="$root/security" \
  STRATA_SIGN_KEYCHAIN="$kc" \
  STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP="$backup" \
  bash "$script" --restore 2>&1)"
rrc=$?
set -e
[[ $rrc -eq 0 ]] || fail "restore: exit $rrc: $rout"
read_paths "$SHIM_DIR/search"
[[ "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" && \
   "${got[1]}" == "/Library/Keychains/System.keychain" && \
   ${#got[@]} -eq 2 ]] || fail "restore: search list not restored: ${got[*]}"
[[ ! -e "$kc" ]] || fail "restore: keychain not deleted"
grep -q '^delete-keychain ' "$log" || fail "restore: no delete-keychain call"

# 2. no identity after import → fail; the search list is changed and then the
#    EXIT trap restores it (add happens before find-identity).
run noid noid
[[ $rc -ne 0 ]] || fail "noid: should fail"
grep -qF 'no code-signing identity' <<<"$out" || fail "noid: missing error: $out"
read_paths "$SHIM_DIR/search"
[[ ${#got[@]} -eq 2 && "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] \
  || fail "noid: search list not restored: ${got[*]}"
# add + restore
[[ "$(s_count 'list-keychains -d user -s ')" -ge 2 ]] || fail "noid: expected add and restore -s calls"
grep -q '^delete-keychain ' "$log" || fail "noid: throwaway keychain not deleted"
[[ ! -e "$tmp/strata-signing.keychain-db" ]] || fail "noid: keychain file left behind"

# 3. SHA-1 mismatch after the search list was updated → trap restores and deletes.
export STRATA_CODESIGN_CERT_SHA1="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
run mismatch
unset STRATA_CODESIGN_CERT_SHA1
[[ $rc -ne 0 ]] || fail "mismatch: should fail"
grep -qF 'is not STRATA_CODESIGN_CERT_SHA1' <<<"$out" || fail "mismatch: missing error: $out"
read_paths "$SHIM_DIR/search"
[[ ${#got[@]} -eq 2 && "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] \
  || fail "mismatch: search list not restored: ${got[*]}"
[[ "$(s_count 'list-keychains -d user -s ')" -ge 2 ]] || fail "mismatch: expected add and restore -s calls"
grep -q '^delete-keychain ' "$log" || fail "mismatch: throwaway keychain not deleted"
[[ ! -e "$tmp/strata-signing.keychain-db" ]] || fail "mismatch: keychain file left behind"

# 4. default keychain changes after add → refuse, restore, delete.
run chdefault chdefault
[[ $rc -ne 0 ]] || fail "chdefault: should fail"
grep -qF 'default keychain changed' <<<"$out" || fail "chdefault: missing error: $out"
read_paths "$SHIM_DIR/search"
[[ ${#got[@]} -eq 2 && "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] \
  || fail "chdefault: search list not restored: ${got[*]}"
[[ "$(s_count 'list-keychains -d user -s ')" -ge 2 ]] || fail "chdefault: expected add and restore -s calls"
grep -q '^delete-keychain ' "$log" || fail "chdefault: throwaway keychain not deleted"

# 5. --restore fallback when the backup variable is unset: drop the throwaway
#    path (under $SHIM_DIR, never /tmp/strata-signing.keychain-db).
export SHIM_DIR="$root/fallback"; mkdir -p "$SHIM_DIR"
kc_fb="$SHIM_DIR/throwaway.keychain-db"
: > "$kc_fb"
printf '    "%s"\n    "%s"\n    "%s"\n' \
  "$kc_fb" \
  "/Users/x/Library/Keychains/login.keychain-db" \
  "/Library/Keychains/System.keychain" > "$SHIM_DIR/search"
: > "$SHIM_DIR/calls.log"
log="$SHIM_DIR/calls.log"
set +e
fout="$(PATH="$root:$PATH" STRATA_SECURITY="$root/security" \
  STRATA_SIGN_KEYCHAIN="$kc_fb" \
  bash "$script" --restore 2>&1)"
frc=$?
set -e
[[ $frc -eq 0 ]] || fail "fallback: exit $frc: $fout"
read_paths "$SHIM_DIR/search"
[[ ${#got[@]} -eq 2 && "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] \
  || fail "fallback: list not stripped: ${got[*]}"
[[ ! -e "$kc_fb" ]] || fail "fallback: keychain file not deleted"
grep -q '^delete-keychain ' "$log" || fail "fallback: no delete-keychain call"

# 6. backup path set but the file is missing → same fallback, not a hard error only.
export SHIM_DIR="$root/missingbak"; mkdir -p "$SHIM_DIR"
kc_mb="$SHIM_DIR/throwaway.keychain-db"
: > "$kc_mb"
printf '    "%s"\n    "%s"\n    "%s"\n' \
  "$kc_mb" \
  "/Users/x/Library/Keychains/login.keychain-db" \
  "/Library/Keychains/System.keychain" > "$SHIM_DIR/search"
: > "$SHIM_DIR/calls.log"
log="$SHIM_DIR/calls.log"
set +e
mout="$(PATH="$root:$PATH" STRATA_SECURITY="$root/security" \
  STRATA_SIGN_KEYCHAIN="$kc_mb" \
  STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP="$SHIM_DIR/no-such-backup.txt" \
  bash "$script" --restore 2>&1)"
mrc=$?
set -e
[[ $mrc -eq 0 ]] || fail "missingbak: exit $mrc: $mout"
grep -qF 'falling back' <<<"$mout" || fail "missingbak: no fallback warning: $mout"
read_paths "$SHIM_DIR/search"
[[ ${#got[@]} -eq 2 && "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] \
  || fail "missingbak: list not stripped: ${got[*]}"
[[ ! -e "$kc_mb" ]] || fail "missingbak: keychain file not deleted"

# 7. list-keychains reports a path lookup error (exit 0, stderr set, macOS 15)
#    → import refuses to start and does not change the list.
run listerr listerr
[[ $rc -ne 0 ]] || fail "listerr: should fail"
grep -qF 'possibly truncated list' <<<"$out" || fail "listerr: missing error: $out"
read_paths "$SHIM_DIR/search"
[[ ${#got[@]} -eq 2 && "${got[0]}" == "/Users/x/Library/Keychains/login.keychain-db" ]] \
  || fail "listerr: search list changed: ${got[*]}"
grep -q '^create-keychain ' "$log" && fail "listerr: created a keychain after a bad listing"
grep -q 'list-keychains -d user -s ' "$log" && fail "listerr: set the search list after a bad listing"

# 8. delete-keychain leaves the file (a directory, so rm -f cannot remove it)
#    → --restore must fail.
export SHIM_DIR="$root/nodel"; mkdir -p "$SHIM_DIR"
kc_nd="$SHIM_DIR/stuck.keychain-db"
mkdir -p "$kc_nd"
printf '    "%s"\n    "%s"\n' \
  "/Users/x/Library/Keychains/login.keychain-db" \
  "/Library/Keychains/System.keychain" > "$SHIM_DIR/search"
: > "$SHIM_DIR/calls.log"
log="$SHIM_DIR/calls.log"
# Backup restores to the two original paths; delete must then fail because the
# "keychain" is a directory and SHIM_MODE=nodel skips even rm in the shim,
# while the script's own rm -f also leaves the directory.
bak="$SHIM_DIR/backup.txt"
printf '    "%s"\n    "%s"\n' \
  "/Users/x/Library/Keychains/login.keychain-db" \
  "/Library/Keychains/System.keychain" > "$bak"
set +e
dout="$(PATH="$root:$PATH" SHIM_MODE=nodel STRATA_SECURITY="$root/security" \
  STRATA_SIGN_KEYCHAIN="$kc_nd" \
  STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP="$bak" \
  bash "$script" --restore 2>&1)"
drc=$?
set -e
[[ $drc -ne 0 ]] || fail "nodel: should fail: $dout"
grep -qF 'still exists' <<<"$dout" || fail "nodel: missing still-exists error: $dout"
[[ -d "$kc_nd" ]] || fail "nodel: test fixture directory disappeared"

echo "ok: import-identity.sh (happy path + restore, noid, sha1 mismatch, default change, restore fallback, missing backup fallback, list error, delete failure)"

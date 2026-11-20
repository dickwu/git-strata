#!/usr/bin/env bash
# CI only: import the self-signed or Developer ID identity from secrets into a
# throwaway keychain and export STRATA_SIGN_ID (the certificate SHA-1) to
# GITHUB_ENV.
# Needs STRATA_CODESIGN_P12 (base64) and STRATA_CODESIGN_P12_PASSWORD.
# If STRATA_CODESIGN_CERT_SHA1 is set, the imported certificate must match it.
#
# codesign on macOS 15 does not reliably find an identity from --keychain
# alone; this script adds the throwaway keychain to the user search list for
# later signing steps. The original list is written to
# STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP. After signing (including on failure)
# run `import-identity.sh --restore`, which restores the list and deletes the
# throwaway keychain. If this script fails after changing the list, it
# restores and deletes itself (EXIT trap). The default keychain is never
# changed. The login keychain is never written to.
#
# Environment (optional):
#   STRATA_SECURITY   path to the `security` tool (tests use a shim)
set -euo pipefail

SECURITY="${STRATA_SECURITY:-/usr/bin/security}"

# Parse `security list-keychains -d user` output into one path per line.
keychain_paths_from_list() {
  sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//' | sed '/^$/d'
}

canon_path() {
  local p="$1" d
  d="$(cd "$(dirname "$p")" 2>/dev/null && pwd -P || true)"
  if [[ -n "$d" ]]; then
    printf '%s/%s' "$d" "$(basename "$p")"
  else
    printf '%s' "$p"
  fi
}

paths_match() {
  local a b
  a="$(canon_path "$1")"
  b="$(canon_path "$2")"
  [[ "$1" == "$2" || "$a" == "$b" ]]
}

# Fail if `security list-keychains -d user` errors or its stderr reports a
# path lookup problem (macOS 15 can omit an entry, print an error, and still
# exit 0). Prints the raw listing on stdout.
list_user_keychains_raw() {
  local err out rc
  err="$(mktemp "${TMPDIR:-/tmp}/gs-kc-list.XXXXXX")"
  set +e
  out="$("$SECURITY" list-keychains -d user 2>"$err")"
  rc=$?
  set -e
  if [[ "$rc" -ne 0 ]]; then
    echo "error: security list-keychains -d user failed" >&2
    cat "$err" >&2 || true
    rm -f "$err"
    return 1
  fi
  if grep -Eiq 'could not be found|unable to|no such|failed to|SecKeychain|error:' "$err"; then
    echo "error: security list-keychains -d user reported a path lookup error; refusing to use a possibly truncated list" >&2
    cat "$err" >&2 || true
    rm -f "$err"
    return 1
  fi
  rm -f "$err"
  printf '%s\n' "$out"
}

read_paths_from_raw() {
  # stdin: raw listing → paths appended to the named array via stdout
  keychain_paths_from_list
}

# Verify the live user search list contains every expected path and no other
# paths (duplicates of an expected path are allowed).
verify_user_search_list() {
  local raw line exp got match
  local -a have=()
  raw="$(list_user_keychains_raw)" || return 1
  while IFS= read -r line; do
    [[ -n "$line" ]] && have+=("$line")
  done < <(printf '%s\n' "$raw" | keychain_paths_from_list)
  for exp in "$@"; do
    match=0
    for got in "${have[@]+"${have[@]}"}"; do
      if paths_match "$exp" "$got"; then match=1; break; fi
    done
    if [[ "$match" != 1 ]]; then
      echo "error: user keychain search list is missing expected path: $exp" >&2
      printf '%s\n' "$raw" >&2
      return 1
    fi
  done
  for got in "${have[@]+"${have[@]}"}"; do
    match=0
    for exp in "$@"; do
      if paths_match "$exp" "$got"; then match=1; break; fi
    done
    if [[ "$match" != 1 ]]; then
      echo "error: user keychain search list has an unexpected path: $got" >&2
      printf '%s\n' "$raw" >&2
      return 1
    fi
  done
  return 0
}

set_user_search_list() {
  if [[ "$#" -eq 0 ]]; then
    echo "error: refusing to set an empty user keychain search list" >&2
    return 1
  fi
  "$SECURITY" list-keychains -d user -s "$@"
  verify_user_search_list "$@"
}

restore_user_search_list() {
  local backup="$1"
  local -a paths=()
  local line
  if [[ ! -f "$backup" ]]; then
    echo "error: search-list backup missing: $backup" >&2
    return 1
  fi
  while IFS= read -r line; do
    [[ -n "$line" ]] && paths+=("$line")
  done < <(keychain_paths_from_list < "$backup")
  if [[ ${#paths[@]} -eq 0 ]]; then
    echo "error: search-list backup is empty: $backup" >&2
    return 1
  fi
  set_user_search_list "${paths[@]}"
}

# Strip the throwaway keychain from a *complete* current listing. Fails
# loudly if the current list cannot be trusted (partial / lookup error).
fallback_strip_throwaway() {
  local keychain="$1"
  local raw line
  local -a paths=()
  raw="$(list_user_keychains_raw)" || return 1
  while IFS= read -r line; do
    if [[ -n "$line" ]]; then
      paths_match "$line" "$keychain" && continue
      paths+=("$line")
    fi
  done < <(printf '%s\n' "$raw" | keychain_paths_from_list)
  if [[ ${#paths[@]} -eq 0 ]]; then
    echo "error: cannot restore search list (no backup, list would be empty)" >&2
    return 1
  fi
  set_user_search_list "${paths[@]}"
}

is_throwaway_keychain_path() {
  local p="$1"
  case "$p" in
    ""|*login.keychain*|*System.keychain*|*metadata.keychain*) return 1 ;;
  esac
  return 0
}

# delete-keychain must actually remove the file; otherwise the caller fails.
delete_throwaway_keychain() {
  local keychain="$1"
  if ! is_throwaway_keychain_path "$keychain"; then
    echo "error: refusing to delete keychain $keychain" >&2
    return 1
  fi
  "$SECURITY" delete-keychain "$keychain" 2>/dev/null || true
  if [[ -e "$keychain" ]]; then
    echo "warning: security delete-keychain left the file; removing $keychain" >&2
    rm -f "$keychain"
  fi
  if [[ -e "$keychain" ]]; then
    echo "error: throwaway keychain still exists: $keychain" >&2
    return 1
  fi
  return 0
}

restore_and_delete() {
  local keychain="${STRATA_SIGN_KEYCHAIN:-}"
  local backup="${STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP:-}"
  local rc=0
  if [[ -n "$backup" && -f "$backup" && -s "$backup" ]]; then
    restore_user_search_list "$backup" || rc=1
  elif [[ -n "$keychain" ]]; then
    # Backup path unset, missing, or empty: strip the throwaway from the
    # current list if that listing is complete.
    if [[ -n "$backup" ]]; then
      echo "warning: search-list backup missing or empty ($backup); falling back to stripping the throwaway keychain from the current list" >&2
    fi
    fallback_strip_throwaway "$keychain" || rc=1
  else
    echo "error: nothing to restore (no backup, no STRATA_SIGN_KEYCHAIN)" >&2
    rc=1
  fi
  if [[ -n "$keychain" ]]; then
    delete_throwaway_keychain "$keychain" || rc=1
  fi
  return "$rc"
}

if [[ "${1:-}" == --restore ]]; then
  restore_and_delete
  exit $?
fi

: "${STRATA_CODESIGN_P12:?secret STRATA_CODESIGN_P12 is not set}"
: "${STRATA_CODESIGN_P12_PASSWORD:?secret STRATA_CODESIGN_P12_PASSWORD is not set}"
tmp="${RUNNER_TEMP:-$(mktemp -d)}"
mkdir -p "$tmp"
keychain="$tmp/strata-signing.keychain-db"
kc_pass="$(uuidgen)"
backup="$tmp/keychain-search-before.txt"
search_before="$(list_user_keychains_raw)"
default_before="$("$SECURITY" default-keychain -d user)"
printf '%s\n' "$search_before" > "$backup"

orig=()
while IFS= read -r line; do
  [[ -n "$line" ]] && orig+=("$line")
done < <(keychain_paths_from_list < "$backup")
if [[ ${#orig[@]} -eq 0 ]]; then
  echo "error: user keychain search list is empty" >&2
  exit 1
fi

added=0
created=0
on_err() {
  rm -f "$tmp/id.p12"
  if [[ "$added" == 1 ]]; then
    restore_user_search_list "$backup" || echo "error: EXIT trap failed to restore the user keychain search list" >&2
    added=0
  fi
  if [[ "$created" == 1 ]]; then
    if ! delete_throwaway_keychain "$keychain"; then
      echo "error: EXIT trap failed to remove the throwaway keychain $keychain" >&2
    fi
    created=0
  fi
}
trap on_err EXIT

"$SECURITY" create-keychain -p "$kc_pass" "$keychain"
created=1
"$SECURITY" set-keychain-settings -lut 21600 "$keychain"
"$SECURITY" unlock-keychain -p "$kc_pass" "$keychain"
printf '%s' "$STRATA_CODESIGN_P12" | base64 --decode > "$tmp/id.p12"
"$SECURITY" import "$tmp/id.p12" -k "$keychain" -f pkcs12 \
  -P "$STRATA_CODESIGN_P12_PASSWORD" -T /usr/bin/codesign
rm -f "$tmp/id.p12"
"$SECURITY" set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kc_pass" "$keychain" > /dev/null

# Prepend the throwaway keychain; keep the original list intact.
# Mark added first so a failed/partial -s still restores.
added=1
set_user_search_list "$keychain" "${orig[@]}"

if [[ "$("$SECURITY" default-keychain -d user)" != "$default_before" ]]; then
  echo "error: the default keychain changed; check it with: security default-keychain -d user" >&2
  exit 1
fi

# No -v: a self-signed certificate is not trusted, and that is fine for codesign.
sha1="$("$SECURITY" find-identity -p codesigning "$keychain" | awk '/^ *1\)/ {print tolower($2); exit}')"
if [[ -z "$sha1" ]]; then
  "$SECURITY" find-identity -p codesigning "$keychain" >&2
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
  echo "STRATA_SIGN_KEYCHAIN_SEARCH_BACKUP=$backup"
} >> "${GITHUB_ENV:-/dev/null}"

# Success: leave the search list in place for later signing steps.
trap - EXIT

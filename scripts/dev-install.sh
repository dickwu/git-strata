#!/bin/sh
# Local dev install of git-strata + git-stratad into ~/.cargo/bin.
#
#   scripts/dev-install.sh            build, (sign), stop, back up, copy, start
#   scripts/dev-install.sh --no-start build, (sign) and copy only
#
# Signing is opt-in: set STRATA_CODESIGN_IDENTITY to a code-signing identity
# in your keychain (e.g. a local self-signed "git-strata dev"; see README,
# "Dev install"). A stable identity + identifier keeps macOS privacy (TCC)
# grants across rebuilds; unsigned/ad-hoc builds re-prompt every time.
set -eu
cd "$(dirname "$0")/.."

# cli.md D38: daemon stop/start act only for the login user's HOME (the
# launchd gui/<uid> domain is per user, not per HOME). Refuse up front so a
# foreign HOME never swaps binaries under a live daemon whose stop refused.
login_user="$(id -un)"
[ -n "$login_user" ] || { echo "dev-install: cannot resolve the login user" >&2; exit 1; }
login_home="$(eval "cd ~$login_user" 2>/dev/null && pwd -P)" || login_home=""
home_real="$(cd "$HOME" 2>/dev/null && pwd -P)" || home_real=""
if [ -z "$login_home" ] || [ "$login_home" != "$home_real" ]; then
    echo "dev-install: HOME=$HOME is not the login home ${login_home:-(unresolved)}; refusing (cli.md D38)" >&2
    exit 1
fi
BIN="${CARGO_BIN:-$HOME/.cargo/bin}"
OUT="${CARGO_TARGET_DIR:-target}/release"

cargo build --release -p git-strata -p strata-daemon

if [ -n "${STRATA_CODESIGN_IDENTITY:-}" ]; then
    codesign -f -s "$STRATA_CODESIGN_IDENTITY" --identifier com.gwd.git-stratad "$OUT/git-stratad"
    codesign -f -s "$STRATA_CODESIGN_IDENTITY" --identifier com.gwd.git-strata "$OUT/git-strata"
    codesign --verify --strict "$OUT/git-stratad" "$OUT/git-strata"
else
    echo "note: STRATA_CODESIGN_IDENTITY unset; installing unsigned (ad-hoc) binaries" >&2
fi

start=1
[ "${1:-}" = "--no-start" ] && start=0
if [ "$start" = 1 ] && [ -x "$BIN/git-strata" ]; then
    # Fail rather than swap binaries under a daemon that did not stop. (The
    # first install of D38 still stops through the previous binary.)
    "$BIN/git-strata" daemon stop
fi
backup="$(mktemp -d "${TMPDIR:-/tmp}/git-strata-prev.XXXXXX")"
for b in git-strata git-stratad; do
    [ -e "$BIN/$b" ] && cp -p "$BIN/$b" "$backup/$b"
done
echo "previous binaries: $backup" >&2
# Both together: the CLI and the daemon must match.
cp "$OUT/git-stratad" "$BIN/git-stratad.new" && cp "$OUT/git-strata" "$BIN/git-strata.new"
mv -f "$BIN/git-stratad.new" "$BIN/git-stratad" && mv -f "$BIN/git-strata.new" "$BIN/git-strata"
if [ "$start" = 1 ]; then
    "$BIN/git-strata" daemon start
    "$BIN/git-strata" daemon status
fi
